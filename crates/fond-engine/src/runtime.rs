//! Runtime рушія: кешовані залежності, тік розкладу (кожні 30 с), конфігурація.
//!
//! Тік бере знімок конфігурації, збирає залежності (без секретів — лише `needs_setup`), за
//! потреби довантажує торговий календар, рахує належні кроки (`schedule::due_steps`) і виконує
//! їх по черзі. Кожен крок іде під `job_lock` — тим самим замком, що й дії оператора
//! ([`Engine::exclusive`]), тож крок і, наприклад, схвалення чи аварійна зупинка ніколи не
//! виконуються одночасно. Помилка кроку — подія `StepFailed` і запис у журналі, а не збій тіку.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdGuard, PoisonError};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use fond_adapters::secrets::SecretStore;
use fond_api::config::EngineConfig;
use fond_api::events::EngineEventDto;
use fond_core::calendar::{TradingCalendar, et_to_utc, utc_to_et};
use fond_core::keys::SecretKey;
use fond_core::traits::Clock;
use fond_store::{EventKind, NewEvent, StepName, StepStatus, Store, StoreError};
use serde_json::{Value, json};
use tokio::sync::{Mutex, MutexGuard, RwLock, watch};
use tokio::time::MissedTickBehavior;

use crate::config_io::{ConfigError, save_atomic};
use crate::cycle::analysis::run_analysis;
use crate::cycle::reconcile::reconcile;
use crate::cycle::submit::{expire_pending, mark_missed, run_pre_open_submit};
use crate::cycle::{CALENDAR_SPAN_DAYS, CycleError, journal_dto};
use crate::deps::{Deps, DepsFactory, SetupError};
use crate::events::EventBus;
use crate::jobs::Jobs;
use crate::schedule::{DueStep, ScheduleInput, StepRecord, due_steps, latest_analysis_date};

/// Період тіку.
pub const TICK_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
/// Запис `Running`, старший за це, вважається покинутим (збій посеред кроку).
pub(crate) const STALE_AFTER_MINUTES: i64 = 15;
/// Календар довантажується, якщо у сховищі немає жодної сесії на `[сьогодні, +10 днів]`.
const CALENDAR_LOOKAHEAD_DAYS: i64 = 10;
/// Пауза перед повтором звірки чи завантаження календаря після збою.
const RETRY_AFTER_MINUTES: i64 = 5;

/// Записи кроків, потрібні розкладу.
pub type StepRecords = HashMap<(NaiveDate, StepName), StepRecord>;

/// Результат тіку: кроки, що виконувались (зокрема ті, що завершились помилкою), і секрети,
/// без яких рушій не працює.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub ran: Vec<DueStep>,
    pub needs_setup: Vec<SecretKey>,
}

/// Кеш залежностей; `generation` не дає записати в кеш збірку, розпочату до скидання.
#[derive(Default)]
struct DepsCache {
    generation: u64,
    deps: Option<Deps>,
}

/// Після збою наступна спроба — не раніше ніж за [`RETRY_AFTER_MINUTES`].
#[derive(Default)]
struct RetryGate(StdMutex<Option<DateTime<Utc>>>);

impl RetryGate {
    fn ready(&self, now: DateTime<Utc>) -> bool {
        lock(&self.0).is_none_or(|at| now >= at)
    }

    fn failed(&self, now: DateTime<Utc>) {
        *lock(&self.0) = Some(now + Duration::minutes(RETRY_AFTER_MINUTES));
    }

    fn succeeded(&self) {
        *lock(&self.0) = None;
    }
}

fn lock<T>(m: &StdMutex<T>) -> StdGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Назва кроку для `StepFailed` і даних журналу.
fn step_key(s: &DueStep) -> &'static str {
    match s {
        DueStep::Reconcile => "reconcile",
        DueStep::Analysis { .. } => "analysis",
        DueStep::ApprovalDeadline { .. } => "approval_deadline",
        DueStep::PreOpenSubmit { .. } => "pre_open_submit",
        DueStep::MissedSubmit { .. } => "missed_submit",
    }
}

/// Назва кроку для повідомлення журналу.
fn step_title(s: &DueStep) -> &'static str {
    match s {
        DueStep::Reconcile => "звірка",
        DueStep::Analysis { .. } => "аналіз",
        DueStep::ApprovalDeadline { .. } => "дедлайн схвалення",
        DueStep::PreOpenSubmit { .. } => "відправка перед відкриттям",
        DueStep::MissedSubmit { .. } => "пропущене вікно відправки",
    }
}

fn step_date(s: &DueStep) -> Option<NaiveDate> {
    match *s {
        DueStep::Reconcile => None,
        DueStep::Analysis { date } => Some(date),
        DueStep::ApprovalDeadline { session }
        | DueStep::PreOpenSubmit { session }
        | DueStep::MissedSubmit { session } => Some(session),
    }
}

/// Розділи верхнього рівня, що відрізняються (у порядку ключів).
fn changed_sections(old: &EngineConfig, new: &EngineConfig) -> Vec<String> {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) =
        (serde_json::to_value(old), serde_json::to_value(new))
    else {
        return Vec::new();
    };
    b.iter()
        .filter(|(k, v)| a.get(k.as_str()) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect()
}

pub struct Engine {
    factory: Arc<dyn DepsFactory>,
    store: Store,
    clock: Arc<dyn Clock>,
    bus: EventBus,
    cfg: RwLock<EngineConfig>,
    cfg_path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    cache: StdMutex<DepsCache>,
    /// Кроки циклу й дії оператора.
    job_lock: Mutex<()>,
    /// Тіки не перекриваються.
    tick_lock: Mutex<()>,
    /// Запис `config.toml` (спільний `<path>.tmp`).
    config_lock: Mutex<()>,
    reconcile_retry: RetryGate,
    calendar_retry: RetryGate,
    /// Остання помилка тіку, уже записана в журнал (повтор не дублюється).
    last_tick_error: StdMutex<Option<String>>,
    /// Фонові завдання API (бектест, завантаження історії).
    jobs: Jobs,
}

impl Engine {
    pub fn new(
        factory: Arc<dyn DepsFactory>,
        store: Store,
        clock: Arc<dyn Clock>,
        bus: EventBus,
        cfg: EngineConfig,
        cfg_path: PathBuf,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            factory,
            store,
            clock,
            bus,
            cfg: RwLock::new(cfg),
            cfg_path,
            secrets,
            cache: StdMutex::new(DepsCache::default()),
            job_lock: Mutex::new(()),
            tick_lock: Mutex::new(()),
            config_lock: Mutex::new(()),
            reconcile_retry: RetryGate::default(),
            calendar_retry: RetryGate::default(),
            last_tick_error: StdMutex::new(None),
            jobs: Jobs::default(),
        })
    }

    /// Один прохід розкладу. Конфігурація береться знімком на початку: зміни діють з
    /// наступного тіку. Помилка — лише інфраструктурна (сховище, секрети).
    pub async fn tick(&self) -> Result<TickReport, CycleError> {
        let _tick = self.tick_lock.lock().await;
        let cfg = self.config().await;
        let d = match self.deps().await {
            Ok(d) => d,
            Err(SetupError::MissingSecrets(keys)) => {
                return Ok(TickReport {
                    ran: Vec::new(),
                    needs_setup: keys,
                });
            }
            Err(SetupError::Other(m)) => return Err(CycleError::Other(m)),
        };

        let now = self.clock.now();
        self.ensure_calendar(&d, now).await?;
        let (cal, records) = self.load_schedule(&cfg, now).await?;
        let last_reconcile = self.store.last_reconcile_at().await?;
        let due = due_steps(&ScheduleInput {
            now,
            cal: &cal,
            cfg: &cfg.schedule,
            records: &records,
            last_reconcile,
            stale_after: Duration::minutes(STALE_AFTER_MINUTES),
        });

        let mut ran = Vec::with_capacity(due.len());
        for step in due {
            if step == DueStep::Reconcile && !self.reconcile_retry.ready(now) {
                continue;
            }
            let _job = self.job_lock.lock().await;
            self.run_step(&d, &cfg, &step).await?;
            ran.push(step);
        }
        Ok(TickReport {
            ran,
            needs_setup: Vec::new(),
        })
    }

    /// Тік одразу, далі кожні 30 с, доки `shutdown` не стане `true` (або відправника не
    /// закрито). Поточний тік завершується до виходу. Помилка чи паніка тіку не зупиняє цикл.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut every = tokio::time::interval(TICK_EVERY);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        while !*shutdown.borrow_and_update() {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    continue;
                }
                _ = every.tick() => {}
            }
            let engine = Arc::clone(&self);
            match tokio::spawn(async move { engine.tick().await }).await {
                Ok(Ok(report)) => self.tick_ok(&report),
                Ok(Err(e)) => self.tick_failed(e.to_string()).await,
                Err(e) => {
                    self.tick_failed(format!("тік аварійно перервано: {e}"))
                        .await
                }
            }
        }
        tracing::info!("цикл рушія зупинено");
    }

    /// Наступний `deps()` збере залежності наново (змінено секрети чи конфігурацію).
    pub fn invalidate_deps(&self) {
        let mut cache = lock(&self.cache);
        cache.generation = cache.generation.wrapping_add(1);
        cache.deps = None;
    }

    /// Кешовані залежності; без кешу — збірка з поточної конфігурації. Помилка не кешується.
    pub async fn deps(&self) -> Result<Deps, SetupError> {
        let generation = {
            let cache = lock(&self.cache);
            if let Some(d) = &cache.deps {
                return Ok(d.clone());
            }
            cache.generation
        };
        let cfg = self.config().await;
        let built = self.factory.build(&cfg)?;
        let mut cache = lock(&self.cache);
        if cache.generation == generation {
            cache.deps = Some(built.clone());
        }
        Ok(built)
    }

    pub async fn config(&self) -> EngineConfig {
        self.cfg.read().await.clone()
    }

    /// Перевірка → атомарний запис `config.toml` → заміна в пам'яті → подія `Config` у
    /// журналі → скидання залежностей (модель і effort судді могли змінитись). Невалідна
    /// конфігурація нічого не змінює й повертає всі порушення.
    pub async fn update_config(&self, new: EngineConfig) -> Result<(), Vec<String>> {
        new.validate()?;
        let _write = self.config_lock.lock().await;
        save_atomic(&self.cfg_path, &new).map_err(|e| match e {
            ConfigError::Invalid(errs) => errs,
            other => vec![other.to_string()],
        })?;
        let old = std::mem::replace(&mut *self.cfg.write().await, new.clone());
        self.invalidate_deps();

        let changed = changed_sections(&old, &new);
        let message = if changed.is_empty() {
            "Конфігурацію збережено без змін".to_string()
        } else {
            format!("Конфігурацію оновлено: {}", changed.join(", "))
        };
        tracing::info!(?changed, "конфігурацію оновлено");
        self.note(EventKind::Config, message, json!({ "changed": changed }))
            .await;
        self.bus.publish(EngineEventDto::StatusChanged);
        Ok(())
    }

    pub fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// Замок кроків циклу. Дії оператора (схвалення, пауза, аварійна зупинка, ручне
    /// закриття) тримають його, щоб не виконуватись одночасно з кроком.
    pub async fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.job_lock.lock().await
    }

    /// Календар зі сховища (±40 днів навколо сьогодні за ET) і записи кроків, які дивиться
    /// розклад: аналіз за `latest_analysis_date` та дедлайн і відправка сьогоднішньої сесії.
    pub async fn records_and_calendar(&self) -> Result<(TradingCalendar, StepRecords), CycleError> {
        let cfg = self.config().await;
        self.load_schedule(&cfg, self.clock.now()).await
    }

    async fn load_schedule(
        &self,
        cfg: &EngineConfig,
        now: DateTime<Utc>,
    ) -> Result<(TradingCalendar, StepRecords), CycleError> {
        let today = utc_to_et(now).date_naive();
        let span = Duration::days(CALENDAR_SPAN_DAYS);
        let cal = TradingCalendar::new(self.store.sessions(today - span, today + span).await?);

        let no_records = StepRecords::new();
        let probe = ScheduleInput {
            now,
            cal: &cal,
            cfg: &cfg.schedule,
            records: &no_records,
            last_reconcile: None,
            stale_after: Duration::minutes(STALE_AFTER_MINUTES),
        };
        let mut wanted = Vec::with_capacity(3);
        if let Some(date) = latest_analysis_date(&probe) {
            wanted.push((date, StepName::Analysis));
        }
        if let Some(s) = cal.session(today) {
            wanted.push((s.date, StepName::ApprovalDeadline));
            wanted.push((s.date, StepName::PreOpenSubmit));
        }

        let mut records = StepRecords::new();
        for (date, step) in wanted {
            if let Some((status, started_at)) = self.store.step_status(date, step).await? {
                records.insert((date, step), StepRecord { status, started_at });
            }
        }
        Ok((cal, records))
    }

    /// Немає жодної сесії на найближчі 10 днів — календар на ±40 днів від брокера. Збій
    /// записується в журнал (повтор — через паузу), тік працює з тим, що є у сховищі.
    async fn ensure_calendar(&self, d: &Deps, now: DateTime<Utc>) -> Result<(), CycleError> {
        let today = utc_to_et(now).date_naive();
        let ahead = today + Duration::days(CALENDAR_LOOKAHEAD_DAYS);
        if !self.store.sessions(today, ahead).await?.is_empty() || !self.calendar_retry.ready(now) {
            return Ok(());
        }
        let span = Duration::days(CALENDAR_SPAN_DAYS);
        match d.data.calendar(today - span, today + span).await {
            Ok(sessions) => {
                self.store.upsert_sessions(&sessions).await?;
                self.calendar_retry.succeeded();
            }
            Err(e) => {
                self.calendar_retry.failed(now);
                tracing::warn!(error = %e, "не вдалося оновити торговий календар");
                self.note(
                    EventKind::Error,
                    format!("Не вдалося оновити торговий календар: {e}"),
                    json!({ "error": e.to_string() }),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_step(
        &self,
        d: &Deps,
        cfg: &EngineConfig,
        step: &DueStep,
    ) -> Result<(), CycleError> {
        match *step {
            DueStep::Reconcile => {
                // Захисний крок: виконується в будь-якому режимі рушія.
                match reconcile(d).await {
                    Ok(_) => self.reconcile_retry.succeeded(),
                    Err(e) => {
                        self.reconcile_retry.failed(self.clock.now());
                        self.step_failed(step, &e, None).await;
                    }
                }
                Ok(())
            }
            DueStep::Analysis { date } => {
                let work = run_analysis(d, cfg, date);
                self.tracked(step, date, StepName::Analysis, None, work)
                    .await
            }
            DueStep::ApprovalDeadline { session } => {
                let work = expire_pending(d, session);
                self.tracked(step, session, StepName::ApprovalDeadline, None, work)
                    .await
            }
            DueStep::PreOpenSubmit { session } => {
                // Невдалу відправку повторює наступний тік, доки вікно ще відкрите.
                let window_end = et_to_utc(session, cfg.schedule.submit_window_end);
                let work = run_pre_open_submit(d, cfg, session);
                self.tracked(
                    step,
                    session,
                    StepName::PreOpenSubmit,
                    Some(window_end),
                    work,
                )
                .await
            }
            DueStep::MissedSubmit { session } => {
                let (status, error) = match mark_missed(d, session).await {
                    Ok(_) => (StepStatus::Missed, None),
                    Err(e) => {
                        self.step_failed(step, &e, None).await;
                        (StepStatus::Failed, Some(e.to_string()))
                    }
                };
                self.store
                    .mark_step(
                        session,
                        StepName::PreOpenSubmit,
                        status,
                        self.clock.now(),
                        error.as_deref(),
                    )
                    .await?;
                Ok(())
            }
        }
    }

    /// `Running` → робота → `Done`; помилка — `StepFailed`, журнал і `Failed`. Якщо задано
    /// `retry_until` і він ще не настав, запис кроку видаляється замість `Failed`, щоб
    /// наступний тік спробував знову.
    async fn tracked<T>(
        &self,
        step: &DueStep,
        date: NaiveDate,
        name: StepName,
        retry_until: Option<DateTime<Utc>>,
        work: impl Future<Output = Result<T, CycleError>>,
    ) -> Result<(), CycleError> {
        self.store
            .mark_step(date, name, StepStatus::Running, self.clock.now(), None)
            .await?;
        match work.await {
            Ok(_) => {
                self.store
                    .mark_step(date, name, StepStatus::Done, self.clock.now(), None)
                    .await?;
            }
            Err(e) => {
                let now = self.clock.now();
                match retry_until.filter(|end| now < *end) {
                    Some(end) => {
                        self.step_failed(step, &e, Some(end)).await;
                        self.store.clear_step(date, name).await?;
                    }
                    None => {
                        self.step_failed(step, &e, None).await;
                        let error = e.to_string();
                        self.store
                            .mark_step(date, name, StepStatus::Failed, now, Some(&error))
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn step_failed(
        &self,
        step: &DueStep,
        e: &CycleError,
        retry_until: Option<DateTime<Utc>>,
    ) {
        let (key, error) = (step_key(step), e.to_string());
        tracing::warn!(step = key, error = %error, "крок завершився помилкою");
        self.bus.publish(EngineEventDto::StepFailed {
            step: key.to_string(),
            error: error.clone(),
        });
        let retry = retry_until
            .map(|at| format!("; повторна спроба до {} ET", utc_to_et(at).format("%H:%M")))
            .unwrap_or_default();
        self.note(
            EventKind::Error,
            format!(
                "Крок «{}» завершився помилкою: {error}{retry}",
                step_title(step)
            ),
            json!({
                "step": key,
                "date": step_date(step),
                "error": error,
                "retry_until": retry_until,
            }),
        )
        .await;
    }

    fn tick_ok(&self, report: &TickReport) {
        *lock(&self.last_tick_error) = None;
        if !report.ran.is_empty() {
            tracing::info!(ran = ?report.ran, "тік: виконано кроки");
        }
        if !report.needs_setup.is_empty() {
            tracing::debug!(needs_setup = ?report.needs_setup, "тік: бракує секретів");
        }
    }

    async fn tick_failed(&self, error: String) {
        tracing::error!(error = %error, "тік завершився помилкою");
        let repeated = {
            let mut last = lock(&self.last_tick_error);
            let repeated = last.as_deref() == Some(error.as_str());
            *last = Some(error.clone());
            repeated
        };
        if !repeated {
            self.note(
                EventKind::Error,
                format!("Тік рушія завершився помилкою: {error}"),
                json!({ "error": error }),
            )
            .await;
        }
    }

    /// Запис у журнал і на шину без `Deps` (залежностей може не бути).
    pub(crate) async fn record_event(&self, e: &NewEvent) -> Result<(), StoreError> {
        let stored = self.store.append_event(self.clock.now(), e).await?;
        self.bus.publish(EngineEventDto::Journal {
            event: journal_dto(&stored),
        });
        Ok(())
    }

    /// Як [`Self::record_event`], але збій журналу лише логується.
    pub(crate) async fn note(&self, kind: EventKind, message: String, data: Value) {
        let e = NewEvent {
            kind,
            symbol: None,
            intent_id: None,
            message,
            data,
        };
        if let Err(e) = self.record_event(&e).await {
            tracing::error!(error = %e, "не вдалося записати подію в журнал");
        }
    }
}

#[cfg(test)]
mod tests;
