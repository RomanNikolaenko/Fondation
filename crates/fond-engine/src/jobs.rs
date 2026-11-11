//! Фонові завдання API: бектест і завантаження історії свічок.
//!
//! Кожного типу одночасно виконується не більше одного завдання: замок береться в обробнику
//! запиту (`try_*`), а його guard живе в задачі до кінця завдання (зокрема при паніці).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use chrono::{Duration, NaiveDate};
use fond_api::dto::HistoryJobDto;
use fond_api::events::EngineEventDto;
use fond_backtest::{BacktestError, BacktestRequest, HistoricalData, run_backtest};
use fond_core::calendar::utc_to_et;
use fond_core::types::Symbol;
use fond_store::{BacktestStatus, EventKind};
use serde_json::json;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::cycle::refresh::refresh_symbols;
use crate::runtime::Engine;

/// Скільки календарних днів до `from` бектест бере для розігріву індикаторів.
pub const BACKTEST_WARMUP_DAYS: i64 = 450;
/// Тікерів в одному запиті `daily_bars` під час завантаження історії.
pub const HISTORY_BATCH: usize = 100;

pub const JOB_IDLE: &str = "idle";
pub const JOB_RUNNING: &str = "running";
pub const JOB_DONE: &str = "done";
pub const JOB_FAILED: &str = "failed";

/// Замки завдань і стан завантаження історії.
pub struct Jobs {
    backtest: Arc<Mutex<()>>,
    history: Arc<Mutex<()>>,
    ai: Arc<Mutex<()>>,
    history_state: StdMutex<HistoryJobDto>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            backtest: Arc::new(Mutex::new(())),
            history: Arc::new(Mutex::new(())),
            ai: Arc::new(Mutex::new(())),
            history_state: StdMutex::new(HistoryJobDto {
                status: JOB_IDLE.into(),
                progress: 0.0,
                message: String::new(),
            }),
        }
    }
}

impl Jobs {
    /// Право запустити бектест; `None`, якщо інший ще виконується.
    pub fn try_backtest(&self) -> Option<OwnedMutexGuard<()>> {
        self.backtest.clone().try_lock_owned().ok()
    }

    /// Право запустити завантаження історії; `None`, якщо інше ще виконується.
    pub fn try_history(&self) -> Option<OwnedMutexGuard<()>> {
        self.history.clone().try_lock_owned().ok()
    }

    /// Право запустити AI-аналіз вручну; `None`, якщо інший ще виконується.
    pub fn try_ai(&self) -> Option<OwnedMutexGuard<()>> {
        self.ai.clone().try_lock_owned().ok()
    }

    pub fn history(&self) -> HistoryJobDto {
        self.history_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_history(&self, job: HistoryJobDto) {
        *self
            .history_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = job;
    }
}

/// Новий стан завантаження історії: у `Jobs` і подією `HistoryProgress`.
pub fn publish_history(engine: &Engine, status: &str, progress: f32, message: String) {
    let job = HistoryJobDto {
        status: status.into(),
        progress: progress.clamp(0.0, 1.0),
        message,
    };
    engine.jobs().set_history(job.clone());
    engine
        .bus()
        .publish(EngineEventDto::HistoryProgress { job });
}

pub fn backtest_status_str(s: BacktestStatus) -> &'static str {
    match s {
        BacktestStatus::Queued => "queued",
        BacktestStatus::Running => "running",
        BacktestStatus::Done => JOB_DONE,
        BacktestStatus::Failed => JOB_FAILED,
    }
}

fn backtest_error_text(e: &BacktestError, market: &Symbol) -> String {
    match e {
        BacktestError::MissingMarketSymbol => {
            format!("Немає історії ринкового індексу {market}: завантажте історію свічок")
        }
        BacktestError::EmptyRange => {
            format!("У вибраному періоді немає свічок ринкового індексу {market}")
        }
        BacktestError::InvalidConfig(errs) => {
            format!("Некоректні параметри стратегій: {}", errs.join("; "))
        }
    }
}

/// Свічки зі сховища для universe (без виключених) і символу ринкового фільтра за
/// `[from − 450 днів, to]`, сектори з universe, без календаря звітів.
async fn historical_data(
    engine: &Engine,
    req: &BacktestRequest,
) -> Result<HistoricalData, fond_store::StoreError> {
    let store = engine.store();
    let universe = store.universe(false).await?;
    let mut symbols: BTreeSet<Symbol> = universe.iter().map(|e| e.symbol.clone()).collect();
    symbols.insert(req.strategies.market_filter.symbol.clone());
    let from = req.from - Duration::days(BACKTEST_WARMUP_DAYS);
    let mut bars = HashMap::new();
    for s in symbols {
        let b = store.bars(&s, from, req.to).await?;
        if !b.is_empty() {
            bars.insert(s, b);
        }
    }
    let sectors = universe.into_iter().map(|e| (e.symbol, e.sector)).collect();
    Ok(HistoricalData {
        bars,
        sectors,
        earnings: None,
        earnings_days_ahead: engine.config().await.earnings_filter.days_ahead,
    })
}

/// Queued → Running → Done (результат у сховищі) або Failed (повідомлення українською).
/// Сам прогін — у `spawn_blocking`. Кожна зміна статусу — подія `BacktestStatus`.
pub async fn run_backtest_job(engine: Arc<Engine>, id: String, req: BacktestRequest) {
    let store = engine.store().clone();
    let publish = |status: BacktestStatus| {
        engine.bus().publish(EngineEventDto::BacktestStatus {
            id: id.clone(),
            status: backtest_status_str(status).into(),
        });
    };
    let outcome: Result<serde_json::Value, String> = async {
        store
            .set_backtest_status(&id, BacktestStatus::Running)
            .await
            .map_err(|e| e.to_string())?;
        publish(BacktestStatus::Running);
        let data = historical_data(&engine, &req)
            .await
            .map_err(|e| e.to_string())?;
        let market = req.strategies.market_filter.symbol.clone();
        let run = tokio::task::spawn_blocking(move || run_backtest(&req, &data));
        match run.await {
            Ok(Ok(result)) => serde_json::to_value(&result)
                .map_err(|e| format!("не вдалося зберегти результат: {e}")),
            Ok(Err(e)) => Err(backtest_error_text(&e, &market)),
            Err(e) => Err(format!("бектест аварійно перервано: {e}")),
        }
    }
    .await;

    let saved = match &outcome {
        Ok(result) => store
            .finish_backtest(&id, result)
            .await
            .map(|()| BacktestStatus::Done),
        Err(message) => {
            tracing::warn!(id = %id, error = %message, "бектест завершився помилкою");
            store
                .fail_backtest(&id, message)
                .await
                .map(|()| BacktestStatus::Failed)
        }
    };
    match saved {
        Ok(status) => {
            tracing::info!(id = %id, status = backtest_status_str(status), "бектест завершено");
            publish(status);
        }
        Err(e) => tracing::error!(id = %id, error = %e, "не вдалося записати статус бектесту"),
    }
}

/// Свічки за `[from, сьогодні ET]` для тікерів оновлення (universe, ринковий індекс, позиції)
/// пачками по 100 через `daily_bars` → `upsert_bars`. Прогрес — у `Jobs` і подіях; підсумок —
/// у журналі.
pub async fn history_download_job(engine: Arc<Engine>, from: NaiveDate) {
    match download(&engine, from).await {
        Ok((symbols, bars)) => {
            let message = format!("Завантажено {bars} свічок для {symbols} тікерів з {from}");
            tracing::info!(symbols, bars, %from, "історію завантажено");
            engine
                .note(
                    EventKind::Cycle,
                    format!("Історія свічок: {message}"),
                    json!({ "from": from, "symbols": symbols, "bars": bars }),
                )
                .await;
            publish_history(&engine, JOB_DONE, 1.0, message);
        }
        Err(message) => {
            tracing::warn!(error = %message, "завантаження історії не вдалося");
            engine
                .note(
                    EventKind::Error,
                    format!("Не вдалося завантажити історію свічок: {message}"),
                    json!({ "from": from, "error": message }),
                )
                .await;
            let progress = engine.jobs().history().progress;
            publish_history(&engine, JOB_FAILED, progress, message);
        }
    }
}

async fn download(engine: &Engine, from: NaiveDate) -> Result<(usize, usize), String> {
    let d = engine.deps().await.map_err(|e| e.to_string())?;
    let cfg = engine.config().await;
    let symbols = refresh_symbols(&d, &cfg).await.map_err(|e| e.to_string())?;
    let to = utc_to_et(d.clock.now()).date_naive();
    let total = symbols.len();
    let mut done = 0;
    let mut bars = 0;
    for chunk in symbols.chunks(HISTORY_BATCH) {
        let fetched = d
            .data
            .daily_bars(chunk, from, to)
            .await
            .map_err(|e| format!("помилка ринкових даних: {e}"))?;
        for s in chunk {
            if let Some(b) = fetched.get(s) {
                bars += d.store.upsert_bars(s, b).await.map_err(|e| e.to_string())?;
            }
        }
        done += chunk.len();
        publish_history(
            engine,
            JOB_RUNNING,
            done as f32 / total.max(1) as f32,
            format!("Завантажено {done} з {total} тікерів"),
        );
    }
    Ok((total, bars))
}
