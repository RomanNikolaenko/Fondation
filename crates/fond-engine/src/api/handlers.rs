//! Обробники маршрутів API та обчислювані поля статусу.
//!
//! Дії оператора (схвалення, відхилення, ручне закриття, пауза, відновлення, зміна
//! конфігурації) виконуються під `Engine::exclusive()`, тобто ніколи одночасно з кроком циклу.
//! Аварійна зупинка спершу ставить `Killed` без замка, а скасовує ордери — вже під ним.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use fond_api::config::EngineConfig;
use fond_api::dto::{
    AccountDto, AddSymbolRequest, BacktestDetailDto, BacktestRunDto, EngineModeDto, ExcludeRequest,
    HealthDto, HistoryJobDto, HistoryRequest, KillRequest, NextStepDto, PositionDto, ScanResultDto,
    SecretStatusDto, SetSecretRequest, SignalDto, StatusDto, UniverseEntryDto, VerifyResultDto,
};
use fond_api::events::{EngineEventDto, JournalEventDto};
use fond_backtest::{BacktestRequest, BacktestResult};
use fond_core::calendar::{et_to_utc, utc_to_et};
use fond_core::error::BrokerError;
use fond_core::intent::{Intent, IntentStatus};
use fond_core::keys::SecretKey;
use fond_core::orders::BrokerOrder;
use fond_core::portfolio::BrokerPosition;
use fond_core::price::to_f64;
use fond_core::types::{AccountMode, Bar, EngineMode, Symbol};
use fond_store::{
    BacktestRow, EventKind, EventQuery, NewEvent, SignalStatus, Store, UniverseEntry,
    UniverseSource, parse_universe_csv,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{ApiError, ApiJson, ApiQuery, ApiState, status_name};
use crate::cycle::analysis::scan_reject_text;
use crate::cycle::kill::{announce_kill, execute_kill, pause_by_user, resume as resume_engine};
use crate::cycle::submit::close_position_now;
use crate::cycle::{journal_dto, week_monday};
use crate::deps::{SetupError, verify_secrets as verify_keys};
use crate::jobs::{JOB_RUNNING, history_download_job, publish_history, run_backtest_job};
use crate::runtime::{Engine, STALE_AFTER_MINUTES};
use crate::schedule::{ScheduleInput, next_step_at};

type ApiResult<T> = Result<Json<T>, ApiError>;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Сектор, коли галузь невідома.
const UNKNOWN_SECTOR: &str = "Unknown";
/// Типовий і найбільший розмір сторінки журналу.
const JOURNAL_LIMIT: u32 = 200;
const JOURNAL_MAX_LIMIT: u32 = 5000;
/// Ключі, які користувач задає через API (токен API — ні).
const USER_KEYS: [SecretKey; 4] = [
    SecretKey::AlpacaKeyId,
    SecretKey::AlpacaSecret,
    SecretKey::AnthropicApiKey,
    SecretKey::FinnhubApiKey,
];

fn mode_dto(m: &EngineMode) -> EngineModeDto {
    let (state, reason) = match m {
        EngineMode::Running => ("running", None),
        EngineMode::Paused { reason } => ("paused", Some(reason.clone())),
        EngineMode::Killed { reason } => ("killed", Some(reason.clone())),
    };
    EngineModeDto {
        state: state.into(),
        reason,
    }
}

impl Engine {
    /// Знімок стану для UI. `needs_setup` — з `SetupError::MissingSecrets`, `next_step` — з
    /// `next_step_at` (календар і записи кроків — `records_and_calendar`). Збій сховища не
    /// ламає відповідь: поле лишається порожнім, причина — у лозі.
    pub async fn status(&self) -> StatusDto {
        let cfg = self.config().await;
        let now = self.clock().now();
        let engine_mode = match self.store().engine_mode().await {
            Ok(m) => mode_dto(&m),
            Err(e) => {
                tracing::warn!(error = %e, "статус: не вдалося прочитати режим рушія");
                EngineModeDto {
                    state: "unknown".into(),
                    reason: Some(format!("не вдалося прочитати режим рушія: {e}")),
                }
            }
        };
        let needs_setup = match self.deps().await {
            Ok(_) => Vec::new(),
            Err(SetupError::MissingSecrets(keys)) => keys.iter().map(status_name).collect(),
            Err(SetupError::Other(e)) => {
                tracing::warn!(error = %e, "статус: залежності не зібрано");
                Vec::new()
            }
        };
        let last_reconcile_at = self.store().last_reconcile_at().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "статус: не вдалося прочитати час звірки");
            None
        });
        let next_step = match self.records_and_calendar().await {
            Ok((cal, records)) => next_step_at(&ScheduleInput {
                now,
                cal: &cal,
                cfg: &cfg.schedule,
                records: &records,
                last_reconcile: last_reconcile_at,
                stale_after: Duration::minutes(STALE_AFTER_MINUTES),
            })
            .map(|(step, at)| NextStepDto {
                name: step.as_str().into(),
                at,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "статус: не вдалося обчислити наступний крок");
                None
            }
        };
        StatusDto {
            engine_mode,
            account_mode: AccountMode::Paper,
            execution_mode: cfg.execution_mode,
            needs_setup,
            next_step,
            last_reconcile_at,
            version: VERSION.into(),
            now,
        }
    }
}

fn today_et(e: &Engine) -> NaiveDate {
    utc_to_et(e.clock().now()).date_naive()
}

/// Початок дня за Нью-Йорком.
fn day_start(d: NaiveDate) -> DateTime<Utc> {
    et_to_utc(d, NaiveTime::MIN)
}

fn money(x: f64) -> String {
    format!("{x:.2}")
}

fn cents(x: Decimal) -> String {
    format!("{x:.2}")
}

fn broker_error(e: BrokerError) -> ApiError {
    ApiError::internal(format!("Помилка брокера: {e}"))
}

/// Список через кому (порожні елементи пропускаються); кожен — snake_case-значення `T`.
fn parse_list<T: DeserializeOwned>(raw: Option<&str>, what: &str) -> Result<Vec<T>, ApiError> {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for item in raw.unwrap_or_default().split(',').map(str::trim) {
        if item.is_empty() {
            continue;
        }
        match serde_json::from_value(serde_json::Value::String(item.to_string())) {
            Ok(v) => out.push(v),
            Err(_) => bad.push(format!("невідоме значення {what}: «{item}»")),
        }
    }
    if bad.is_empty() {
        Ok(out)
    } else {
        Err(ApiError::validation("Некоректні параметри запиту", bad))
    }
}

/// Запис у журнал без `Deps`; збій лише логується (дію вже виконано).
async fn journal_note(e: &Engine, event: NewEvent) {
    if let Err(err) = e.record_event(&event).await {
        tracing::error!(error = %err, "не вдалося записати подію в журнал");
    }
}

// ---------------------------------------------------------------- стан

pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        ok: true,
        version: VERSION.into(),
    })
}

pub async fn status(State(st): State<ApiState>) -> Json<StatusDto> {
    Json(st.engine.status().await)
}

/// `day_pnl = equity − last_equity`; `week_pnl` — відносно останньої Close-точки до понеділка
/// поточного тижня (ET).
pub async fn account(State(st): State<ApiState>) -> ApiResult<AccountDto> {
    let e = &st.engine;
    let d = e.deps().await?;
    let acc = d.broker.account().await.map_err(broker_error)?;
    let week_start = e
        .store()
        .last_close_equity_before(week_monday(today_et(e)))
        .await?;
    Ok(Json(AccountDto {
        equity: acc.equity.to_string(),
        cash: acc.cash.to_string(),
        buying_power: acc.buying_power.to_string(),
        day_pnl: cents(acc.equity - acc.last_equity),
        week_pnl: week_start.map(|(_, base)| money(to_f64(acc.equity) - base)),
    }))
}

/// Відстежувані позиції; поточна ціна й нереалізований P&L — з позицій брокера. Якщо брокер
/// позиції не показує (її щойно продано, звірка ще попереду), ціна — ціна входу, P&L — 0.
pub async fn positions(State(st): State<ApiState>) -> ApiResult<Vec<PositionDto>> {
    let e = &st.engine;
    let d = e.deps().await?;
    let held: HashMap<Symbol, BrokerPosition> = d
        .broker
        .positions()
        .await
        .map_err(broker_error)?
        .into_iter()
        .map(|p| (p.symbol.clone(), p))
        .collect();
    let tracked = e.store().positions().await?;
    let out = tracked
        .into_iter()
        .map(|t| {
            let p = t.position;
            let (current_price, unrealized_pnl) = match held.get(&p.symbol) {
                Some(b) => (
                    cents(b.current_price),
                    cents((b.current_price - b.avg_entry_price) * Decimal::from(b.qty)),
                ),
                None => (money(p.entry_price), money(0.0)),
            };
            PositionDto {
                symbol: p.symbol.to_string(),
                strategy: p.strategy.to_string(),
                qty: p.qty,
                entry_price: money(p.entry_price),
                entry_date: p.entry_date,
                current_price,
                stop_price: money(p.stop_price),
                take_profit: p.take_profit.map(money),
                unrealized_pnl,
                sector: p.sector,
            }
        })
        .collect();
    Ok(Json(out))
}

/// Відкриті ордери брокера (з ногами).
pub async fn orders(State(st): State<ApiState>) -> ApiResult<Vec<BrokerOrder>> {
    let d = st.engine.deps().await?;
    Ok(Json(d.broker.open_orders().await.map_err(broker_error)?))
}

#[derive(Debug, Deserialize)]
pub struct DateQuery {
    date: NaiveDate,
}

pub async fn signals(
    State(st): State<ApiState>,
    ApiQuery(q): ApiQuery<DateQuery>,
) -> ApiResult<Vec<SignalDto>> {
    let rows = st.engine.store().signals_on(q.date).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| SignalDto {
                signal: r.signal,
                status: match r.status {
                    SignalStatus::Accepted => "accepted",
                    SignalStatus::Rejected => "rejected",
                }
                .into(),
                reject_reason: r.reject_reason,
            })
            .collect(),
    ))
}

pub async fn scan(
    State(st): State<ApiState>,
    ApiQuery(q): ApiQuery<DateQuery>,
) -> ApiResult<Vec<ScanResultDto>> {
    let rows = st.engine.store().scan_results(q.date).await?;
    Ok(Json(
        rows.into_iter()
            .map(|(symbol, reject)| ScanResultDto {
                symbol: symbol.to_string(),
                passed: reject.is_none(),
                reason: reject.as_ref().map(scan_reject_text),
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------- наміри

const ALL_STATUSES: [IntentStatus; 8] = [
    IntentStatus::PendingApproval,
    IntentStatus::Approved,
    IntentStatus::Rejected,
    IntentStatus::Expired,
    IntentStatus::Submitted,
    IntentStatus::Filled,
    IntentStatus::Cancelled,
    IntentStatus::Failed,
];

#[derive(Debug, Deserialize)]
pub struct IntentsQuery {
    status: Option<String>,
}

/// `?status=a,b` (snake_case); без параметра — усі наміри.
pub async fn intents(
    State(st): State<ApiState>,
    ApiQuery(q): ApiQuery<IntentsQuery>,
) -> ApiResult<Vec<Intent>> {
    let mut statuses: Vec<IntentStatus> = parse_list(q.status.as_deref(), "статусу")?;
    if statuses.is_empty() {
        statuses = ALL_STATUSES.to_vec();
    }
    Ok(Json(
        st.engine.store().intents_with_status(&statuses).await?,
    ))
}

pub async fn approve(State(st): State<ApiState>, Path(id): Path<String>) -> ApiResult<Intent> {
    decide(&st.engine, &id, IntentStatus::Approved)
        .await
        .map(Json)
}

pub async fn reject(State(st): State<ApiState>, Path(id): Path<String>) -> ApiResult<Intent> {
    decide(&st.engine, &id, IntentStatus::Rejected)
        .await
        .map(Json)
}

/// `PendingApproval` → `Approved` / `Rejected`. Інший статус — 409; схвалення після дедлайну
/// сесії — теж 409 (крок дедлайну міг ще не встигнути виконатись).
async fn decide(e: &Engine, id: &str, next: IntentStatus) -> Result<Intent, ApiError> {
    let _job = e.exclusive().await;
    let Some(current) = e.store().intent(id).await? else {
        return Err(ApiError::not_found(format!("Намір «{id}» не знайдено")));
    };
    if current.status != IntentStatus::PendingApproval {
        return Err(ApiError::conflict(format!(
            "Намір «{id}» не чекає на схвалення (статус: {})",
            status_name(&current.status)
        )));
    }
    let now = e.clock().now();
    if next == IntentStatus::Approved {
        let deadline = e.config().await.schedule.approval_deadline;
        if now >= et_to_utc(current.session_date, deadline) {
            return Err(ApiError::conflict(format!(
                "Дедлайн схвалення ({} ET {}) минув",
                deadline.format("%H:%M"),
                current.session_date
            )));
        }
    }
    let updated = e.store().update_intent(id, next, now, None, None).await?;
    let verb = if next == IntentStatus::Approved {
        "схвалено"
    } else {
        "відхилено"
    };
    tracing::info!(intent = %id, status = %status_name(&next), "рішення користувача щодо наміру");
    journal_note(
        e,
        NewEvent {
            kind: EventKind::Intent,
            symbol: Some(updated.symbol.clone()),
            intent_id: Some(updated.id.clone()),
            message: format!("Намір {} {verb} користувачем", updated.id),
            data: json!({ "status": status_name(&next), "by": "user" }),
        },
    )
    .await;
    Ok(updated)
}

/// Ручне закриття позиції (`close_position_now`); позиції немає в обліку — 404.
pub async fn close_position(
    State(st): State<ApiState>,
    Path(symbol): Path<String>,
) -> ApiResult<Intent> {
    let e = &st.engine;
    let symbol = Symbol::new(&symbol);
    let d = e.deps().await?;
    let _job = e.exclusive().await;
    let held = e.store().positions().await?;
    if !held.iter().any(|p| p.position.symbol == symbol) {
        return Err(ApiError::not_found(format!(
            "Позиції {symbol} немає в обліку"
        )));
    }
    Ok(Json(close_position_now(&d, &symbol).await?))
}

// ---------------------------------------------------------------- режим рушія

pub async fn pause(State(st): State<ApiState>) -> ApiResult<StatusDto> {
    let e = &st.engine;
    let d = e.deps().await?;
    {
        let _job = e.exclusive().await;
        pause_by_user(&d).await?;
    }
    Ok(Json(e.status().await))
}

pub async fn resume(State(st): State<ApiState>) -> ApiResult<StatusDto> {
    let e = &st.engine;
    let d = e.deps().await?;
    {
        let _job = e.exclusive().await;
        resume_engine(&d).await?;
    }
    Ok(Json(e.status().await))
}

/// Аварійна зупинка: `Killed`, журнал і подія — одразу, без замка кроків (аналіз може ще
/// створювати наміри, але відправка бачить `Killed` і нічого не надсилає). Скасування
/// намірів і ордерів, закриття чи звірка — під замком. Без ключів брокера лишається тільки
/// перша частина.
pub async fn kill(
    State(st): State<ApiState>,
    ApiJson(req): ApiJson<KillRequest>,
) -> ApiResult<StatusDto> {
    let e = &st.engine;
    announce_kill(e.store(), e.clock().as_ref(), e.bus(), req.close_positions).await?;
    tracing::warn!(
        close_positions = req.close_positions,
        "аварійна зупинка користувачем"
    );
    match e.deps().await {
        Ok(d) => {
            let _job = e.exclusive().await;
            let report = execute_kill(&d, req.close_positions).await?;
            tracing::info!(?report, "аварійну зупинку виконано");
        }
        Err(SetupError::MissingSecrets(keys)) => {
            let missing: Vec<String> = keys.iter().map(status_name).collect();
            e.note(
                EventKind::Kill,
                "Аварійна зупинка: ключів брокера немає, ордери й позиції не перевірено".into(),
                json!({ "needs_setup": missing }),
            )
            .await;
        }
        Err(other) => return Err(other.into()),
    }
    Ok(Json(e.status().await))
}

// ---------------------------------------------------------------- конфігурація

pub async fn get_config(State(st): State<ApiState>) -> Json<EngineConfig> {
    Json(st.engine.config().await)
}

/// Невалідна конфігурація — 422 з усіма порушеннями; діє попередня.
pub async fn put_config(
    State(st): State<ApiState>,
    ApiJson(cfg): ApiJson<EngineConfig>,
) -> ApiResult<EngineConfig> {
    let e = &st.engine;
    {
        let _job = e.exclusive().await;
        e.update_config(cfg)
            .await
            .map_err(|errs| ApiError::validation("Некоректна конфігурація", errs))?;
    }
    Ok(Json(e.config().await))
}

// ---------------------------------------------------------------- свічки й журнал

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

/// Свічки зі сховища; типово — рік до сьогодні (ET).
pub async fn bars(
    State(st): State<ApiState>,
    Path(symbol): Path<String>,
    ApiQuery(q): ApiQuery<RangeQuery>,
) -> ApiResult<Vec<Bar>> {
    let e = &st.engine;
    let to = q.to.unwrap_or_else(|| today_et(e));
    let from = q.from.unwrap_or(to - Duration::days(365));
    if from > to {
        return Err(ApiError::validation(
            "Некоректні параметри запиту",
            vec![format!("from ({from}) пізніше за to ({to})")],
        ));
    }
    Ok(Json(e.store().bars(&Symbol::new(&symbol), from, to).await?))
}

#[derive(Debug, Deserialize)]
pub struct JournalQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    symbol: Option<String>,
    kinds: Option<String>,
    limit: Option<u32>,
}

/// Журнал, новіші першими. `from`/`to` — дні за Нью-Йорком (включно), `kinds` — через кому,
/// `limit` — типово 200.
pub async fn journal(
    State(st): State<ApiState>,
    ApiQuery(q): ApiQuery<JournalQuery>,
) -> ApiResult<Vec<JournalEventDto>> {
    let mut kinds = Vec::new();
    let mut bad = Vec::new();
    for k in q.kinds.as_deref().unwrap_or_default().split(',') {
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        match EventKind::parse(k) {
            Ok(kind) => kinds.push(kind),
            Err(_) => bad.push(format!("невідомий тип події: «{k}»")),
        }
    }
    if !bad.is_empty() {
        return Err(ApiError::validation("Некоректні параметри запиту", bad));
    }
    let query = EventQuery {
        from: q.from.map(day_start),
        to: q
            .to
            .map(|d| day_start(d + Duration::days(1)) - Duration::microseconds(1)),
        symbol: q
            .symbol
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Symbol::new),
        kinds,
        limit: q.limit.unwrap_or(JOURNAL_LIMIT).min(JOURNAL_MAX_LIMIT),
    };
    let events = st.engine.store().events(&query).await?;
    Ok(Json(events.iter().map(journal_dto).collect()))
}

// ---------------------------------------------------------------- бектести

fn validate_backtest(req: &BacktestRequest) -> Result<(), ApiError> {
    let mut errs = Vec::new();
    if req.from > req.to {
        errs.push(format!("from ({}) пізніше за to ({})", req.from, req.to));
    }
    if !(req.initial_equity.is_finite() && req.initial_equity > 0.0) {
        errs.push("initial_equity має бути більше 0".to_string());
    }
    if let Some(split) = req.split
        && (split <= req.from || split > req.to)
    {
        errs.push(format!("split ({split}) має бути між from і to"));
    }
    if let Err(v) = req.strategies.validate() {
        errs.extend(v);
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation("Некоректний запит бектесту", errs))
    }
}

fn backtest_id(now: DateTime<Utc>) -> String {
    format!(
        "bt-{}-{:06x}",
        now.format("%Y%m%d-%H%M%S"),
        rand::random::<u32>() & 0x00ff_ffff
    )
}

fn run_dto(row: &BacktestRow) -> Result<BacktestRunDto, ApiError> {
    let request = serde_json::from_value(row.request.clone())
        .map_err(|e| ApiError::internal(format!("Пошкоджений запит бектесту {}: {e}", row.id)))?;
    Ok(BacktestRunDto {
        id: row.id.clone(),
        created_at: row.created_at,
        status: crate::jobs::backtest_status_str(row.status).into(),
        request,
        error: row.error.clone(),
    })
}

/// 202 і запуск у фоні; одночасно — лише один бектест (інакше 409).
pub async fn start_backtest(
    State(st): State<ApiState>,
    ApiJson(req): ApiJson<BacktestRequest>,
) -> Result<(StatusCode, Json<BacktestRunDto>), ApiError> {
    validate_backtest(&req)?;
    let e = &st.engine;
    let Some(guard) = e.jobs().try_backtest() else {
        return Err(ApiError::conflict(
            "Бектест уже виконується — дочекайтеся його завершення",
        ));
    };
    let now = e.clock().now();
    let id = backtest_id(now);
    let request = serde_json::to_value(&req)
        .map_err(|err| ApiError::internal(format!("Не вдалося зберегти запит: {err}")))?;
    e.store().create_backtest(&id, &request, now).await?;
    let run = BacktestRunDto {
        id: id.clone(),
        created_at: now,
        status: "queued".into(),
        request: req.clone(),
        error: None,
    };
    let engine = e.clone();
    tokio::spawn(async move {
        let _guard = guard;
        run_backtest_job(engine, id, req).await;
    });
    Ok((StatusCode::ACCEPTED, Json(run)))
}

pub async fn list_backtests(State(st): State<ApiState>) -> ApiResult<Vec<BacktestRunDto>> {
    let rows = st.engine.store().backtests().await?;
    rows.iter()
        .map(run_dto)
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

pub async fn get_backtest(
    State(st): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<BacktestDetailDto> {
    let Some(row) = st.engine.store().backtest(&id).await? else {
        return Err(ApiError::not_found(format!("Бектест «{id}» не знайдено")));
    };
    let result = row
        .result
        .clone()
        .map(serde_json::from_value::<BacktestResult>)
        .transpose()
        .map_err(|e| ApiError::internal(format!("Пошкоджений результат бектесту {id}: {e}")))?;
    Ok(Json(BacktestDetailDto {
        run: run_dto(&row)?,
        result,
    }))
}

// ---------------------------------------------------------------- історія свічок

/// 202 і завантаження у фоні; одночасно — лише одне (інакше 409).
pub async fn start_history(
    State(st): State<ApiState>,
    ApiJson(req): ApiJson<HistoryRequest>,
) -> Result<(StatusCode, Json<HistoryJobDto>), ApiError> {
    let e = &st.engine;
    let today = today_et(e);
    if req.from > today {
        return Err(ApiError::validation(
            "Некоректний запит",
            vec![format!(
                "from ({}) не може бути пізніше за сьогодні ({today})",
                req.from
            )],
        ));
    }
    let Some(guard) = e.jobs().try_history() else {
        return Err(ApiError::conflict(
            "Історія вже завантажується — дочекайтеся завершення",
        ));
    };
    // Без ключів Alpaca завантажувати нічим: помилка одразу, а не у фоні.
    e.deps().await?;
    publish_history(
        e,
        JOB_RUNNING,
        0.0,
        format!("Завантаження історії з {}", req.from),
    );
    let job = e.jobs().history();
    let engine = e.clone();
    tokio::spawn(async move {
        let _guard = guard;
        history_download_job(engine, req.from).await;
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
}

pub async fn history_job(State(st): State<ApiState>) -> Json<HistoryJobDto> {
    Json(st.engine.jobs().history())
}

// ---------------------------------------------------------------- список тікерів

fn entry_dto(e: UniverseEntry) -> UniverseEntryDto {
    UniverseEntryDto {
        symbol: e.symbol.to_string(),
        name: e.name,
        sector: e.sector,
        source: e.source.as_str().into(),
        excluded: e.excluded,
    }
}

async fn find_entry(store: &Store, symbol: &Symbol) -> Result<Option<UniverseEntry>, ApiError> {
    Ok(store
        .universe(true)
        .await?
        .into_iter()
        .find(|e| e.symbol == *symbol))
}

/// Тікер: латинські літери, цифри, `.` і `-`, до 12 символів.
fn valid_symbol(s: &Symbol) -> bool {
    let t = s.as_str();
    !t.is_empty()
        && t.len() <= 12
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn invalid_symbol(raw: &str) -> String {
    format!(
        "«{}»: тікер — латинські літери, цифри, «.» або «-», до 12 символів",
        raw.trim()
    )
}

async fn universe_note(e: &Engine, symbol: Option<Symbol>, message: String) {
    journal_note(
        e,
        NewEvent {
            kind: EventKind::Config,
            symbol,
            intent_id: None,
            message,
            data: json!({ "section": "universe" }),
        },
    )
    .await;
}

/// Увесь список, разом із виключеними.
pub async fn universe(State(st): State<ApiState>) -> ApiResult<Vec<UniverseEntryDto>> {
    let all = st.engine.store().universe(true).await?;
    Ok(Json(all.into_iter().map(entry_dto).collect()))
}

/// Тікер, доданий вручну. Сектор — галузь із Finnhub; будь-яка помилка (зокрема вимкнений
/// фільтр звітності чи відсутні ключі) дає «Unknown», а не помилку запиту.
pub async fn add_symbol(
    State(st): State<ApiState>,
    ApiJson(req): ApiJson<AddSymbolRequest>,
) -> ApiResult<UniverseEntryDto> {
    let e = &st.engine;
    let symbol = Symbol::new(&req.symbol);
    if !valid_symbol(&symbol) {
        return Err(ApiError::validation(
            "Некоректний тікер",
            vec![invalid_symbol(&req.symbol)],
        ));
    }
    if find_entry(e.store(), &symbol).await?.is_some() {
        return Err(ApiError::conflict(format!("{symbol} уже є у списку")));
    }
    let industry = match e.deps().await {
        Ok(d) => d
            .industry
            .industry(&symbol)
            .await
            .map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    };
    let sector = match industry {
        Ok(Some(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Ok(_) => UNKNOWN_SECTOR.to_string(),
        Err(err) => {
            tracing::info!(symbol = %symbol, error = %err, "галузь невідома");
            UNKNOWN_SECTOR.to_string()
        }
    };
    let entry = UniverseEntry {
        symbol: symbol.clone(),
        name: symbol.to_string(),
        sector,
        source: UniverseSource::Manual,
        excluded: false,
    };
    e.store().add_manual(&entry).await?;
    universe_note(
        e,
        Some(symbol.clone()),
        format!("Додано тікер {symbol} (сектор: {})", entry.sector),
    )
    .await;
    Ok(Json(entry_dto(entry)))
}

pub async fn set_excluded(
    State(st): State<ApiState>,
    Path(symbol): Path<String>,
    ApiJson(req): ApiJson<ExcludeRequest>,
) -> ApiResult<UniverseEntryDto> {
    let e = &st.engine;
    let symbol = Symbol::new(&symbol);
    let Some(mut entry) = find_entry(e.store(), &symbol).await? else {
        return Err(ApiError::not_found(format!("{symbol} немає у списку")));
    };
    e.store().set_excluded(&symbol, req.excluded).await?;
    entry.excluded = req.excluded;
    let what = if req.excluded {
        "виключено зі"
    } else {
        "повернуто до"
    };
    universe_note(
        e,
        Some(symbol.clone()),
        format!("Тікер {symbol} {what} списку"),
    )
    .await;
    Ok(Json(entry_dto(entry)))
}

/// Видаляються лише тікери, додані вручну (решту можна виключити).
pub async fn remove_symbol(
    State(st): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<StatusCode, ApiError> {
    let e = &st.engine;
    let symbol = Symbol::new(&symbol);
    let Some(entry) = find_entry(e.store(), &symbol).await? else {
        return Err(ApiError::not_found(format!("{symbol} немає у списку")));
    };
    if entry.source != UniverseSource::Manual {
        return Err(ApiError::conflict(format!(
            "Видаляти можна лише тікери, додані вручну; {symbol} можна виключити"
        )));
    }
    e.store().remove_manual(&symbol).await?;
    universe_note(
        e,
        Some(symbol.clone()),
        format!("Тікер {symbol} видалено зі списку"),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// CSV `symbol,name,sector` (тіло `text/csv`): кожен рядок — тікер, доданий вручну.
/// Повертає весь список.
pub async fn import_universe(
    State(st): State<ApiState>,
    body: String,
) -> ApiResult<Vec<UniverseEntryDto>> {
    let e = &st.engine;
    let entries = parse_universe_csv(&body, UniverseSource::Manual)
        .map_err(|m| ApiError::validation("Некоректний CSV", vec![m]))?;
    let bad: Vec<String> = entries
        .iter()
        .filter(|x| !valid_symbol(&x.symbol))
        .map(|x| invalid_symbol(x.symbol.as_str()))
        .collect();
    if !bad.is_empty() {
        return Err(ApiError::validation("Некоректний CSV", bad));
    }
    if entries.is_empty() {
        return Err(ApiError::validation(
            "Некоректний CSV",
            vec!["у файлі немає жодного тікера".into()],
        ));
    }
    for entry in &entries {
        e.store().add_manual(entry).await?;
    }
    universe_note(
        e,
        None,
        format!("Імпортовано тікерів зі CSV: {}", entries.len()),
    )
    .await;
    let all = e.store().universe(true).await?;
    Ok(Json(all.into_iter().map(entry_dto).collect()))
}

// ---------------------------------------------------------------- секрети

/// Чи збережено кожен із чотирьох ключів користувача. Значень API не повертає.
pub async fn secrets(State(st): State<ApiState>) -> ApiResult<Vec<SecretStatusDto>> {
    let store = st.engine.secrets();
    let mut out = Vec::with_capacity(USER_KEYS.len());
    for k in USER_KEYS {
        let present = store.get(k)?.is_some_and(|v| !v.expose().trim().is_empty());
        out.push(SecretStatusDto {
            key: status_name(&k),
            present,
        });
    }
    Ok(Json(out))
}

/// Лише чотири ключі користувача (snake_case); `engine_api_token` та інші — 400. Після запису
/// залежності збираються наново. Значення не логується й не потрапляє в журнал.
pub async fn set_secret(
    State(st): State<ApiState>,
    Path(key): Path<String>,
    ApiJson(req): ApiJson<SetSecretRequest>,
) -> Result<StatusCode, ApiError> {
    let e = &st.engine;
    let Some(k) = USER_KEYS.into_iter().find(|k| status_name(k) == key) else {
        return Err(ApiError::bad_request(format!(
            "Невідомий ключ «{key}»; дозволено: alpaca_key_id, alpaca_secret, \
             anthropic_api_key, finnhub_api_key"
        )));
    };
    let value = req.value.trim();
    if value.is_empty() {
        return Err(ApiError::validation(
            "Порожнє значення ключа",
            vec![format!("{key}: значення не може бути порожнім")],
        ));
    }
    e.secrets().set(k, value)?;
    e.invalidate_deps();
    tracing::info!(key = %key, "ключ оновлено");
    e.note(
        EventKind::Config,
        format!("Оновлено ключ {key}"),
        json!({ "key": key }),
    )
    .await;
    e.bus().publish(EngineEventDto::StatusChanged);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn verify_secrets(State(st): State<ApiState>) -> Json<VerifyResultDto> {
    let e = &st.engine;
    let cfg = e.config().await;
    Json(verify_keys(e.secrets().as_ref(), e.clock().clone(), &cfg).await)
}
