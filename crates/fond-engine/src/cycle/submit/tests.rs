//! Тести відправки: спільні фікстури й скриптований брокер; самі тести — у підмодулях.

mod entry;
mod exit;
mod expire;
mod gate;
mod manual;
mod orders;
mod rebalance;
mod stop;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fond_api::events::EngineEventDto;
use fond_core::ids::{IntentTag, client_order_id};
use fond_core::intent::{IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side};
use fond_core::portfolio::{Account, BrokerPosition, Position, TrackedPosition};
use fond_core::risk::RiskConfig;
use fond_core::testkit::FakeBroker;
use fond_core::traits::Broker;
use fond_core::types::EngineMode;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::super::reconcile::{flatten, reconcile};
use super::super::test_support::{
    Handles, N_BARS, d, drain, entry_kind, et, fixture, flat_bars, held_aapl, intent_with,
    journal_messages, session, start_date, submit_entry, sym, tracked,
};
use super::*;

/// Стенд із годинником на 09:00 ET сесії (вікно відправки).
async fn setup() -> (Deps, Handles) {
    let (dp, h) = fixture().await;
    h.clock.set(et(session(), 9, 0));
    (dp, h)
}

async fn intent(dp: &Deps, id: &str) -> Intent {
    dp.store.intent(id).await.unwrap().unwrap()
}

async fn insert(dp: &Deps, intents: &[&Intent]) {
    for i in intents {
        assert!(dp.store.insert_intent(i).await.unwrap(), "{}", i.id);
    }
}

async fn order_status(h: &Handles, id: &str) -> OrderStatus {
    h.broker.order(id).await.unwrap().status
}

async fn stored_order_status(dp: &Deps, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM orders WHERE id = ?")
        .bind(id)
        .fetch_optional(dp.store.pool())
        .await
        .unwrap()
}

async fn run(dp: &Deps) -> SubmitSummary {
    run_with(dp, &EngineConfig::default()).await
}

async fn run_with(dp: &Deps, cfg: &EngineConfig) -> SubmitSummary {
    run_pre_open_submit(dp, cfg, session()).await.unwrap()
}

fn approved_entry(symbol: &str, take_profit: Option<Decimal>) -> Intent {
    intent_with(
        symbol,
        IntentTag::Entry,
        entry_kind(10, dec!(101.00), dec!(95.50), take_profit),
        IntentStatus::Approved,
        None,
    )
}

fn approved_exit(symbol: &str, qty: u32) -> Intent {
    let kind = IntentKind::Exit {
        qty,
        reason: "сигнал виходу".into(),
    };
    intent_with(symbol, IntentTag::Exit, kind, IntentStatus::Approved, None)
}

fn approved_move(symbol: &str, new_stop: Decimal, stop_order_id: &str) -> Intent {
    let kind = IntentKind::MoveStop {
        new_stop,
        stop_order_id: stop_order_id.into(),
    };
    intent_with(symbol, IntentTag::Stop, kind, IntentStatus::Approved, None)
}

/// Той самий намір, але на сесію `date` (id і `session_date`).
fn on(date: NaiveDate, i: Intent) -> Intent {
    Intent {
        id: i
            .id
            .replacen("20261105", &date.format("%Y%m%d").to_string(), 1),
        session_date: date,
        ..i
    }
}

/// AAPL (10 шт., стоп 95.50, `ord-1-stop`) і MSFT (20 шт., стоп 47.00, `ord-2-stop`),
/// виконані й звірені.
async fn held_aapl_msft(dp: &Deps, h: &Handles) {
    let (_, aapl) = submit_entry(dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
    let (_, msft) = submit_entry(dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
    h.broker.fill(&aapl.id, dec!(101.00), 10);
    h.broker.fill(&msft.id, dec!(50.00), 20);
    reconcile(dp).await.unwrap();
    assert_eq!(dp.store.positions().await.unwrap().len(), 2);
}

fn killed() -> EngineMode {
    EngineMode::Killed {
        reason: "тест".into(),
    }
}

/// Що зробить черговий `submit` обгортки.
enum Script {
    /// Помилка; ордер до брокера не дійшов.
    Fail(BrokerError),
    /// Ордер подано, але відповідь втрачено: повертається помилка.
    Lose(BrokerError),
}

/// Обгортка над `FakeBroker`: виконує сценарій для `submit` і веде журнал змінних
/// викликів (`submit`, `cancel`, `replace_stop`); решта методів — без змін.
struct ScriptedBroker {
    inner: Arc<FakeBroker>,
    script: Mutex<VecDeque<Script>>,
    submits: AtomicUsize,
    log: Mutex<Vec<String>>,
    /// Ордер, який устигає виконатись (`qty` шт. за `price`) перед своїм скасуванням.
    fill_on_cancel: Mutex<Option<(String, Decimal, u32)>>,
}

impl ScriptedBroker {
    fn submits(&self) -> usize {
        self.submits.load(Ordering::SeqCst)
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn note(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

#[async_trait]
impl Broker for ScriptedBroker {
    async fn account(&self) -> Result<Account, BrokerError> {
        self.inner.account().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        self.inner.positions().await
    }

    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError> {
        self.inner.open_orders().await
    }

    async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError> {
        self.inner.order(id).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: &str,
    ) -> Result<Option<BrokerOrder>, BrokerError> {
        self.inner.order_by_client_id(client_order_id).await
    }

    async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        self.note(format!("submit {}", req.client_order_id));
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Script::Fail(e)) => Err(e),
            Some(Script::Lose(e)) => {
                self.inner.submit(req).await?;
                Err(e)
            }
            None => self.inner.submit(req).await,
        }
    }

    async fn cancel(&self, order_id: &str) -> Result<(), BrokerError> {
        self.note(format!("cancel {order_id}"));
        let race = self.fill_on_cancel.lock().unwrap().take();
        match race {
            Some((id, price, qty)) if id == order_id => self.inner.fill(&id, price, qty),
            other => *self.fill_on_cancel.lock().unwrap() = other,
        }
        self.inner.cancel(order_id).await
    }

    async fn replace_stop(
        &self,
        order_id: &str,
        new_stop: Decimal,
    ) -> Result<BrokerOrder, BrokerError> {
        self.note(format!("replace {order_id}"));
        self.inner.replace_stop(order_id, new_stop).await
    }
}

/// Залежності з `ScriptedBroker` замість брокера стенда (стан брокера спільний).
fn scripted(dp: &Deps, h: &Handles, script: Vec<Script>) -> (Deps, Arc<ScriptedBroker>) {
    let broker = Arc::new(ScriptedBroker {
        inner: h.broker.clone(),
        script: Mutex::new(script.into()),
        submits: AtomicUsize::new(0),
        log: Mutex::new(Vec::new()),
        fill_on_cancel: Mutex::new(None),
    });
    let deps = Deps {
        broker: broker.clone(),
        ..dp.clone()
    };
    (deps, broker)
}

fn transient() -> BrokerError {
    BrokerError::Transient("timeout".into())
}

// ---------------------------------------------------------------- побудова ордерів
