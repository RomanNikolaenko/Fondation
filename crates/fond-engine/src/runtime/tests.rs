use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use fond_adapters::secrets::{MemorySecretStore, SecretStore};
use fond_api::config::EngineConfig;
use fond_api::events::EngineEventDto;
use fond_core::error::{BrokerError, DataError};
use fond_core::ids::IntentTag;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::keys::SecretKey;
use fond_core::orders::{BrokerOrder, OrderClass, OrderRequest, Side, TimeInForce};
use fond_core::portfolio::{Account, BrokerPosition};
use fond_core::price::to_f64;
use fond_core::testkit::{FakeBroker, FakeClock};
use fond_core::traits::Broker;
use fond_core::types::{EngineMode, ExecutionMode};
use fond_store::{EventKind, EventQuery, StepName, StepStatus};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::watch;

use super::*;
use crate::config_io::{load_or_create, save_atomic};
use crate::cycle::kill::KILL_REASON;
use crate::cycle::test_support::{
    Handles, d, drain, entry_kind, et, fixture, intent_with, last_date, session, submit_entry, sym,
};
use crate::deps::{Deps, DepsFactory, LiveDepsFactory, SetupError};
use crate::schedule::DueStep;

const ENTRY_ID: &str = "20261105-trend_pullback-AAPL-entry";

/// Фабрика тестів: повертає залежності стенда, рахує збірки й запам'ятовує модель судді.
struct TestFactory {
    deps: Deps,
    builds: AtomicUsize,
    models: Mutex<Vec<String>>,
}

impl TestFactory {
    fn builds(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }

    fn models(&self) -> Vec<String> {
        self.models.lock().unwrap().clone()
    }
}

impl DepsFactory for TestFactory {
    fn build(&self, cfg: &EngineConfig) -> Result<Deps, SetupError> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        self.models
            .lock()
            .unwrap()
            .push(cfg.news_filter.model.clone());
        Ok(self.deps.clone())
    }
}

struct Bench {
    engine: Arc<Engine>,
    d: Deps,
    h: Handles,
    factory: Arc<TestFactory>,
    dir: tempfile::TempDir,
}

impl Bench {
    fn cfg_path(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    fn at(&self, t: DateTime<Utc>) {
        self.h.clock.set(t);
    }

    async fn tick(&self) -> TickReport {
        self.engine.tick().await.unwrap()
    }

    async fn status(&self, date: NaiveDate, step: StepName) -> Option<StepStatus> {
        self.d
            .store
            .step_status(date, step)
            .await
            .unwrap()
            .map(|(s, _)| s)
    }

    async fn done(&self, date: NaiveDate, step: StepName) {
        self.d
            .store
            .mark_step(date, step, StepStatus::Done, self.d.clock.now(), None)
            .await
            .unwrap();
    }

    /// Дедлайн схвалення й відправку сесії `s` виконано.
    async fn settle_session(&self, s: NaiveDate) {
        self.done(s, StepName::ApprovalDeadline).await;
        self.done(s, StepName::PreOpenSubmit).await;
    }

    async fn intent(&self, id: &str) -> Intent {
        self.d.store.intent(id).await.unwrap().unwrap()
    }

    async fn journal(&self) -> Vec<fond_store::StoredEvent> {
        self.d.store.events(&EventQuery::default()).await.unwrap()
    }
}

async fn bench_with(cfg: EngineConfig, tweak: impl FnOnce(&mut Deps, &Handles)) -> Bench {
    let (mut d, h) = fixture().await;
    tweak(&mut d, &h);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    save_atomic(&path, &cfg).unwrap();
    let factory = Arc::new(TestFactory {
        deps: d.clone(),
        builds: AtomicUsize::new(0),
        models: Mutex::new(Vec::new()),
    });
    let engine = Engine::new(
        factory.clone(),
        d.store.clone(),
        d.clock.clone(),
        d.bus.clone(),
        cfg,
        path,
        Arc::new(MemorySecretStore::default()),
    );
    Bench {
        engine,
        d,
        h,
        factory,
        dir,
    }
}

async fn bench() -> Bench {
    bench_with(EngineConfig::default(), |_, _| {}).await
}

fn step_failed<'a>(events: &'a [EngineEventDto], step: &str) -> Option<&'a str> {
    events.iter().find_map(|e| match e {
        EngineEventDto::StepFailed { step: s, error } if s == step => Some(error.as_str()),
        _ => None,
    })
}

#[tokio::test]
async fn tick_runs_analysis_once_after_1630_et() {
    let b = bench().await;
    let wed = last_date();
    // 21:31Z = 16:31 ET (середа, після переходу на зимовий час).
    b.at(Utc.with_ymd_and_hms(2026, 11, 4, 21, 31, 0).unwrap());

    let first = b.tick().await;
    assert_eq!(
        first.ran,
        vec![
            DueStep::Reconcile,
            DueStep::Analysis { date: wed },
            DueStep::ApprovalDeadline { session: wed },
            DueStep::MissedSubmit { session: wed },
        ]
    );
    assert!(first.needs_setup.is_empty());

    let second = b.tick().await;
    assert_eq!(second, TickReport::default());

    assert_eq!(
        b.status(wed, StepName::Analysis).await,
        Some(StepStatus::Done)
    );
    assert_eq!(
        b.status(wed, StepName::ApprovalDeadline).await,
        Some(StepStatus::Done)
    );
    assert_eq!(
        b.status(wed, StepName::PreOpenSubmit).await,
        Some(StepStatus::Missed)
    );
    // Аналіз справді виконано один раз.
    assert_eq!(b.h.data.daily_bars_calls(), 1);
    assert_eq!(b.h.judge.calls(), 1);
    assert_eq!(
        b.intent(ENTRY_ID).await.status,
        IntentStatus::PendingApproval
    );
}

#[tokio::test]
async fn tick_catch_up_after_restart_runs_latest_analysis_only() {
    let b = bench().await;
    let (mon, tue, wed, thu) = (d(2026, 11, 2), d(2026, 11, 3), last_date(), session());
    // Рушій востаннє працював у понеділок і стояв до четверга 07:00 ET.
    b.d.store
        .mark_step(
            mon,
            StepName::Analysis,
            StepStatus::Done,
            et(mon, 16, 31),
            None,
        )
        .await
        .unwrap();
    b.at(et(thu, 7, 0));

    let r = b.tick().await;
    assert_eq!(
        r.ran,
        vec![DueStep::Reconcile, DueStep::Analysis { date: wed }]
    );
    assert_eq!(b.status(tue, StepName::Analysis).await, None);
    assert_eq!(
        b.status(wed, StepName::Analysis).await,
        Some(StepStatus::Done)
    );
    assert_eq!(b.h.data.daily_bars_calls(), 1);
    let intents = b.d.store.intents_for_session(thu).await.unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].id, ENTRY_ID);

    assert_eq!(b.tick().await, TickReport::default());
}

#[tokio::test]
async fn tick_without_secrets_reports_needs_setup() {
    let (d, h) = fixture().await;
    let mut cfg = EngineConfig::default();
    cfg.news_filter.enabled = false;
    cfg.earnings_filter.enabled = false;
    let secrets = Arc::new(MemorySecretStore::default());
    let factory = Arc::new(LiveDepsFactory {
        secrets: secrets.clone(),
        store: d.store.clone(),
        clock: d.clock.clone(),
        bus: d.bus.clone(),
        health: d.health.clone(),
    });
    let dir = tempfile::tempdir().unwrap();
    h.clock.set(et(last_date(), 16, 31));
    let engine = Engine::new(
        factory,
        d.store.clone(),
        d.clock.clone(),
        d.bus.clone(),
        cfg,
        dir.path().join("config.toml"),
        secrets.clone(),
    );

    let r = engine.tick().await.unwrap();
    assert_eq!(
        r,
        TickReport {
            ran: vec![],
            needs_setup: vec![SecretKey::AlpacaKeyId, SecretKey::AlpacaSecret],
        }
    );
    assert_eq!(
        d.store
            .step_status(last_date(), StepName::Analysis)
            .await
            .unwrap(),
        None
    );
    assert_eq!(d.store.last_reconcile_at().await.unwrap(), None);
    assert!(matches!(
        engine.deps().await,
        Err(SetupError::MissingSecrets(_))
    ));

    // Відсутність секретів не кешується: щойно ключі з'являються, залежності збираються.
    engine
        .secrets()
        .set(SecretKey::AlpacaKeyId, "PKTEST1234")
        .unwrap();
    engine
        .secrets()
        .set(SecretKey::AlpacaSecret, "secret-5678")
        .unwrap();
    assert!(secrets.get(SecretKey::AlpacaSecret).unwrap().is_some());
    assert!(engine.deps().await.is_ok());
}

#[tokio::test]
async fn stale_running_step_is_rerun_after_crash() {
    let b = bench().await;
    let wed = last_date();
    b.settle_session(wed).await;
    // Аналіз стартував о 16:40 ET, і рушій упав.
    b.d.store
        .mark_step(
            wed,
            StepName::Analysis,
            StepStatus::Running,
            et(wed, 16, 40),
            None,
        )
        .await
        .unwrap();
    b.d.store
        .set_last_reconcile_at(et(wed, 16, 40))
        .await
        .unwrap();

    // Через 10 хв запис ще не протух.
    b.at(et(wed, 16, 50));
    assert_eq!(b.tick().await.ran, vec![]);
    assert_eq!(b.h.judge.calls(), 0);

    // Через 16 хв — протух, аналіз виконується заново.
    b.at(et(wed, 16, 56));
    assert_eq!(b.tick().await.ran, vec![DueStep::Analysis { date: wed }]);
    assert_eq!(
        b.d.store
            .step_status(wed, StepName::Analysis)
            .await
            .unwrap(),
        Some((StepStatus::Done, et(wed, 16, 56)))
    );
    assert_eq!(b.h.judge.calls(), 1);
}

#[tokio::test]
async fn failed_step_emits_event_and_is_not_retried() {
    let b = bench().await;
    let wed = last_date();
    b.settle_session(wed).await;
    b.d.store
        .set_last_reconcile_at(et(wed, 16, 30))
        .await
        .unwrap();
    b.at(et(wed, 16, 31));
    let mut rx = b.d.bus.subscribe();
    b.h.data
        .fail_next(DataError::Transient("дані недоступні".into()));

    let r = b.tick().await;
    assert_eq!(r.ran, vec![DueStep::Analysis { date: wed }]);
    assert_eq!(
        b.status(wed, StepName::Analysis).await,
        Some(StepStatus::Failed)
    );
    let events = drain(&mut rx);
    let error = step_failed(&events, "analysis").expect("подія StepFailed");
    assert!(error.contains("дані недоступні"), "{error}");
    let journal = b.journal().await;
    assert!(
        journal.iter().any(|e| e.kind == EventKind::Error
            && e.message.contains("аналіз")
            && e.message.contains("дані недоступні")),
        "{:?}",
        journal.iter().map(|e| &e.message).collect::<Vec<_>>()
    );

    // Наступні тіки крок не повторюють.
    b.at(et(wed, 17, 0));
    assert_eq!(b.tick().await, TickReport::default());
    assert_eq!(b.h.data.daily_bars_calls(), 1);
    assert_eq!(
        b.status(wed, StepName::Analysis).await,
        Some(StepStatus::Failed)
    );
}

#[tokio::test]
async fn update_config_rejects_invalid_and_keeps_old() {
    let b = bench().await;
    let old = b.engine.config().await;
    assert_eq!(old, EngineConfig::default());
    let mut bad = old.clone();
    bad.risk.risk_per_trade = 0.0;
    bad.schedule.reconcile_every_minutes = 1;

    let errs = b.engine.update_config(bad).await.unwrap_err();
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs.iter().any(|e| e.contains("risk.risk_per_trade")));
    assert!(errs.iter().any(|e| e.contains("reconcile_every_minutes")));

    assert_eq!(b.engine.config().await, old);
    assert_eq!(load_or_create(&b.cfg_path()).unwrap(), old);
    assert!(!b.dir.path().join("config.toml.tmp").exists());
    assert!(
        b.journal()
            .await
            .iter()
            .all(|e| e.kind != EventKind::Config)
    );
}

#[tokio::test]
async fn update_config_saves_swaps_journals_and_invalidates_deps() {
    let b = bench().await;
    b.engine.deps().await.unwrap();
    b.engine.deps().await.unwrap();
    assert_eq!(b.factory.builds(), 1, "залежності кешуються");

    let mut new = b.engine.config().await;
    new.execution_mode = ExecutionMode::Auto;
    new.news_filter.model = "claude-sonnet-5-5".into();
    let mut rx = b.d.bus.subscribe();
    b.engine.update_config(new.clone()).await.unwrap();

    assert_eq!(b.engine.config().await, new);
    assert_eq!(load_or_create(&b.cfg_path()).unwrap(), new);
    b.engine.deps().await.unwrap();
    assert_eq!(b.factory.builds(), 2);
    assert_eq!(
        b.factory.models(),
        vec!["claude-haiku-5-5".to_string(), "claude-sonnet-5-5".into()]
    );

    let journal = b.journal().await;
    let cfg_event = journal
        .iter()
        .find(|e| e.kind == EventKind::Config)
        .expect("подія Config");
    assert_eq!(
        cfg_event.data["changed"],
        serde_json::json!(["execution_mode", "news_filter"])
    );
    let events = drain(&mut rx);
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEventDto::Journal { event } if event.kind == "config"
    )));
    assert!(events.contains(&EngineEventDto::StatusChanged));

    // Явне скидання теж змушує зібрати залежності наново.
    b.engine.invalidate_deps();
    b.engine.deps().await.unwrap();
    assert_eq!(b.factory.builds(), 3);
}

#[tokio::test]
async fn full_day_flow() {
    let b = bench().await;
    let (wed, thu) = (last_date(), session());

    // Середа 16:31 ET: аналіз створює намір на вхід у четвер.
    b.at(et(wed, 16, 31));
    assert!(
        b.tick()
            .await
            .ran
            .contains(&DueStep::Analysis { date: wed })
    );
    let pending = b.intent(ENTRY_ID).await;
    assert_eq!(pending.status, IntentStatus::PendingApproval);
    let IntentKind::Entry {
        qty,
        limit_price,
        stop_price,
        ..
    } = pending.kind
    else {
        panic!("очікувався вхід: {:?}", pending.kind);
    };

    // Схвалення (як це зробить API).
    b.d.store
        .update_intent(
            ENTRY_ID,
            IntentStatus::Approved,
            b.d.clock.now(),
            None,
            None,
        )
        .await
        .unwrap();

    // Четвер 08:56 ET: дедлайн схвалення не чіпає схвалений намір.
    b.at(et(thu, 8, 56));
    assert_eq!(
        b.tick().await.ran,
        vec![DueStep::ApprovalDeadline { session: thu }]
    );
    assert_eq!(b.intent(ENTRY_ID).await.status, IntentStatus::Approved);
    assert!(b.h.broker.submitted().is_empty());

    // 09:01 ET: брокер отримує рівно один OTO.
    b.at(et(thu, 9, 1));
    assert_eq!(
        b.tick().await.ran,
        vec![DueStep::PreOpenSubmit { session: thu }]
    );
    let sent = b.h.broker.submitted();
    assert_eq!(sent.len(), 1);
    let req = &sent[0];
    assert_eq!(
        (req.symbol.as_str(), req.side, req.class, req.tif),
        ("AAPL", Side::Buy, OrderClass::Oto, TimeInForce::Day)
    );
    assert_eq!(
        (req.qty, req.limit_price, req.stop_loss),
        (qty, Some(limit_price), Some(stop_price))
    );
    let submitted = b.intent(ENTRY_ID).await;
    assert_eq!(submitted.status, IntentStatus::Submitted);
    let order_id = submitted.broker_order_id.expect("id ордера");
    assert_eq!(
        b.status(thu, StepName::PreOpenSubmit).await,
        Some(StepStatus::Done)
    );

    // Виконання о 09:31 ET.
    b.h.broker
        .fill_at(&order_id, limit_price, qty, et(thu, 9, 31));

    // 10:31 ET: щогодинна звірка бере позицію на облік разом зі стоп-ногою.
    b.at(et(thu, 10, 31));
    assert_eq!(b.tick().await.ran, vec![DueStep::Reconcile]);
    let positions = b.d.store.positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    let p = &positions[0];
    assert_eq!((p.position.symbol.as_str(), p.position.qty), ("AAPL", qty));
    assert_eq!(p.position.stop_price, to_f64(stop_price));
    assert_eq!(p.intent_id, ENTRY_ID);
    assert_eq!(
        p.stop_order_id.as_deref(),
        Some(format!("{order_id}-stop").as_str())
    );
    assert_eq!(b.intent(ENTRY_ID).await.status, IntentStatus::Filled);
    assert_eq!(
        b.d.store.last_reconcile_at().await.unwrap(),
        Some(et(thu, 10, 31))
    );
}

#[tokio::test]
async fn reconcile_runs_while_paused_or_killed() {
    for mode in [
        EngineMode::Paused {
            reason: "тест".into(),
        },
        EngineMode::Killed {
            reason: KILL_REASON.into(),
        },
    ] {
        let b = bench().await;
        let thu = session();
        b.done(last_date(), StepName::Analysis).await;
        b.settle_session(thu).await;
        let (_, order) = submit_entry(&b.d, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        b.h.broker
            .fill_at(&order.id, dec!(101.00), 10, et(thu, 9, 31));
        b.d.store
            .set_engine_mode(&mode, et(thu, 9, 35))
            .await
            .unwrap();
        b.d.store
            .set_last_reconcile_at(et(thu, 9, 0))
            .await
            .unwrap();

        b.at(et(thu, 10, 1));
        assert_eq!(b.tick().await.ran, vec![DueStep::Reconcile], "{mode:?}");
        let positions = b.d.store.positions().await.unwrap();
        assert_eq!(positions.len(), 1, "{mode:?}");
        assert_eq!(
            positions[0].stop_order_id.as_deref(),
            Some(format!("{}-stop", order.id).as_str())
        );
        assert_eq!(b.d.store.engine_mode().await.unwrap(), mode);
        assert_eq!(
            b.d.store.last_reconcile_at().await.unwrap(),
            Some(et(thu, 10, 1))
        );
    }
}

/// Схвалений вхід AAPL на сесію `session()`; остання ціна поруч із лімітом.
async fn approved_entry(b: &Bench) {
    let intent = intent_with(
        "AAPL",
        IntentTag::Entry,
        entry_kind(10, dec!(101.00), dec!(95.50), None),
        IntentStatus::Approved,
        None,
    );
    assert!(b.d.store.insert_intent(&intent).await.unwrap());
    b.h.data.set_latest_trade(&sym("AAPL"), 100.50);
}

#[tokio::test]
async fn failed_pre_open_submit_is_retried_inside_window() {
    let b = bench().await;
    let (wed, thu) = (last_date(), session());
    b.done(wed, StepName::Analysis).await;
    b.done(thu, StepName::ApprovalDeadline).await;
    b.d.store
        .set_last_reconcile_at(et(wed, 16, 31))
        .await
        .unwrap();
    approved_entry(&b).await;
    let mut rx = b.d.bus.subscribe();

    // 09:01 ET: брокер недоступний (звірка на початку кроку).
    b.at(et(thu, 9, 1));
    b.h.broker
        .fail_next(BrokerError::Transient("брокер недоступний".into()));
    assert_eq!(
        b.tick().await.ran,
        vec![DueStep::PreOpenSubmit { session: thu }]
    );
    assert_eq!(b.status(thu, StepName::PreOpenSubmit).await, None);
    let events = drain(&mut rx);
    let error = step_failed(&events, "pre_open_submit").expect("подія StepFailed");
    assert!(error.contains("брокер недоступний"), "{error}");
    assert!(b.h.broker.submitted().is_empty());
    assert!(
        b.journal().await.iter().any(
            |e| e.kind == EventKind::Error && e.message.contains("повторна спроба до 09:25 ET")
        )
    );

    // 09:02 ET: наступний тік повторює крок, вхід відправлено.
    b.at(et(thu, 9, 2));
    assert_eq!(
        b.tick().await.ran,
        vec![DueStep::PreOpenSubmit { session: thu }]
    );
    assert_eq!(
        b.status(thu, StepName::PreOpenSubmit).await,
        Some(StepStatus::Done)
    );
    assert_eq!(b.h.broker.submitted().len(), 1);
    assert_eq!(b.intent(ENTRY_ID).await.status, IntentStatus::Submitted);
}

/// Брокер, у якого `positions()` переводить годинник на `jump_to` і завершується помилкою:
/// крок почався у вікні відправки, а збій стався вже після його кінця.
struct LateFailingBroker {
    inner: Arc<FakeBroker>,
    clock: Arc<FakeClock>,
    jump_to: DateTime<Utc>,
}

#[async_trait]
impl Broker for LateFailingBroker {
    async fn account(&self) -> Result<Account, BrokerError> {
        self.inner.account().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        self.clock.set(self.jump_to);
        Err(BrokerError::Transient("таймаут".into()))
    }

    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError> {
        self.inner.open_orders().await
    }

    async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError> {
        self.inner.order(id).await
    }

    async fn order_by_client_id(&self, cid: &str) -> Result<Option<BrokerOrder>, BrokerError> {
        self.inner.order_by_client_id(cid).await
    }

    async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
        self.inner.submit(req).await
    }

    async fn cancel(&self, order_id: &str) -> Result<(), BrokerError> {
        self.inner.cancel(order_id).await
    }

    async fn replace_stop(
        &self,
        order_id: &str,
        new_stop: Decimal,
    ) -> Result<BrokerOrder, BrokerError> {
        self.inner.replace_stop(order_id, new_stop).await
    }
}

#[tokio::test]
async fn failed_pre_open_submit_after_window_end_is_marked_failed() {
    let (wed, thu) = (last_date(), session());
    let b = bench_with(EngineConfig::default(), |d, h| {
        d.broker = Arc::new(LateFailingBroker {
            inner: h.broker.clone(),
            clock: h.clock.clone(),
            jump_to: et(thu, 9, 25),
        });
    })
    .await;
    b.done(wed, StepName::Analysis).await;
    b.done(thu, StepName::ApprovalDeadline).await;
    b.d.store
        .set_last_reconcile_at(et(wed, 16, 31))
        .await
        .unwrap();
    approved_entry(&b).await;
    let mut rx = b.d.bus.subscribe();

    b.at(et(thu, 9, 24));
    assert_eq!(
        b.tick().await.ran,
        vec![DueStep::PreOpenSubmit { session: thu }]
    );
    assert_eq!(
        b.status(thu, StepName::PreOpenSubmit).await,
        Some(StepStatus::Failed)
    );
    assert!(step_failed(&drain(&mut rx), "pre_open_submit").is_some());

    // Вікно минуло: ні повтору, ні «пропущеного вікна» поверх Failed.
    b.at(et(thu, 9, 26));
    assert_eq!(b.tick().await, TickReport::default());
}

#[tokio::test]
async fn failed_reconcile_is_retried_after_backoff() {
    let b = bench().await;
    let thu = session();
    b.done(last_date(), StepName::Analysis).await;
    b.settle_session(thu).await;
    let mut rx = b.d.bus.subscribe();

    b.at(et(thu, 10, 1));
    b.h.broker
        .fail_next(BrokerError::Transient("брокер недоступний".into()));
    assert_eq!(b.tick().await.ran, vec![DueStep::Reconcile]);
    assert!(step_failed(&drain(&mut rx), "reconcile").is_some());
    assert_eq!(b.d.store.last_reconcile_at().await.unwrap(), None);

    // Без паузи між спробами журнал засипало б подіями кожні 30 с.
    b.at(et(thu, 10, 3));
    assert_eq!(b.tick().await, TickReport::default());

    b.at(et(thu, 10, 6));
    assert_eq!(b.tick().await.ran, vec![DueStep::Reconcile]);
    assert_eq!(
        b.d.store.last_reconcile_at().await.unwrap(),
        Some(et(thu, 10, 6))
    );
}

#[tokio::test]
async fn missing_calendar_is_fetched_before_scheduling() {
    let b = bench().await;
    let wed = last_date();
    sqlx::query("DELETE FROM calendar_sessions")
        .execute(b.d.store.pool())
        .await
        .unwrap();
    b.at(et(wed, 16, 31));

    // Збій джерела календаря: подія в журналі, тік триває без сесійних кроків.
    b.h.data
        .fail_next(DataError::Transient("календар недоступний".into()));
    assert_eq!(b.tick().await.ran, vec![DueStep::Reconcile]);
    assert!(
        b.journal()
            .await
            .iter()
            .any(|e| e.kind == EventKind::Error && e.message.contains("календар недоступний"))
    );
    assert!(
        b.d.store
            .sessions(wed, wed + chrono::Duration::days(10))
            .await
            .unwrap()
            .is_empty()
    );

    // Після паузи календар завантажено, і аналіз за середу виконується.
    b.at(et(wed, 16, 37));
    let r = b.tick().await;
    assert!(r.ran.contains(&DueStep::Analysis { date: wed }), "{r:?}");
    assert!(
        !b.d.store
            .sessions(wed, wed + chrono::Duration::days(10))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn records_and_calendar_cover_latest_analysis_and_today_session() {
    let b = bench().await;
    let (tue, wed, thu) = (d(2026, 11, 3), last_date(), session());
    b.at(et(thu, 9, 10));
    for (date, step) in [
        (tue, StepName::Analysis),
        (wed, StepName::Analysis),
        (thu, StepName::ApprovalDeadline),
    ] {
        b.done(date, step).await;
    }

    let (cal, records) = b.engine.records_and_calendar().await.unwrap();
    assert!(cal.session(thu).is_some());
    assert_eq!(cal.next_session_after(thu), Some(d(2026, 11, 6)));
    let mut keys: Vec<_> = records.keys().copied().collect();
    keys.sort_by_key(|(date, step)| (*date, step.as_str()));
    assert_eq!(
        keys,
        vec![(wed, StepName::Analysis), (thu, StepName::ApprovalDeadline)]
    );
    assert_eq!(records[&(wed, StepName::Analysis)].status, StepStatus::Done);
}

#[tokio::test]
async fn tick_steps_wait_for_exclusive_guard() {
    let b = bench().await;
    let wed = last_date();
    b.at(et(wed, 16, 31));

    let guard = b.engine.exclusive().await;
    let engine = b.engine.clone();
    let tick = tokio::spawn(async move { engine.tick().await });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    assert!(!tick.is_finished());
    assert_eq!(b.status(wed, StepName::Analysis).await, None);
    assert_eq!(b.d.store.last_reconcile_at().await.unwrap(), None);

    drop(guard);
    let r = tick.await.unwrap().unwrap();
    assert!(r.ran.contains(&DueStep::Analysis { date: wed }));
    assert_eq!(
        b.status(wed, StepName::Analysis).await,
        Some(StepStatus::Done)
    );
}

#[tokio::test]
async fn run_ticks_immediately_and_stops_on_shutdown() {
    let b = bench().await;
    let wed = last_date();
    b.at(et(wed, 16, 31));
    let (tx, rx) = watch::channel(false);
    let handle = tokio::spawn(b.engine.clone().run(rx));

    tokio::time::timeout(StdDuration::from_secs(20), async {
        while b.status(wed, StepName::Analysis).await != Some(StepStatus::Done) {
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    })
    .await
    .expect("перший тік одразу після старту");

    tx.send(true).unwrap();
    tokio::time::timeout(StdDuration::from_secs(20), handle)
        .await
        .expect("цикл зупиняється за сигналом")
        .unwrap();
    assert_eq!(b.h.judge.calls(), 1);
}
