//! Пауза, відновлення роботи й аварійна зупинка.

use std::collections::HashSet;

use fond_api::events::EngineEventDto;
use fond_core::calendar::utc_to_et;
use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{IntentKind, IntentStatus};
use fond_core::orders::{
    BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce,
};
use fond_core::portfolio::TrackedPosition;
use fond_core::types::{EngineMode, Symbol};
use fond_store::{EventKind, StoreError};
use serde_json::json;

use super::reconcile::{broker_failure, ensure_stop, flatten, observe, reconcile};
use super::{CycleError, journal};
use crate::deps::Deps;

/// Причина режиму `Killed`, коли зупинку ввімкнув користувач.
pub const KILL_REASON: &str = "Аварійна зупинка користувачем";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillReport {
    pub orders_cancelled: usize,
    pub positions_closed: usize,
    pub intents_cancelled: usize,
    /// Звірка після зупинки без закриття позицій не вдалася (текст помилки; подія в журналі).
    pub reconcile_error: Option<String>,
}

/// Ставить паузу, лише якщо рушій працює: аварійна зупинка чи інша пауза не змінюються.
/// Подія `AutoPaused` і запис у журналі — лише коли режим справді змінено.
pub async fn pause(d: &Deps, reason: &str) -> Result<(), CycleError> {
    pause_changed(d, reason).await.map(drop)
}

/// Як `pause`, але повертає, чи змінено режим. Рішення приймає один умовний запит до
/// сховища, тож аварійна зупинка, що встигла між будь-якими кроками, не «понижується».
pub async fn pause_changed(d: &Deps, reason: &str) -> Result<bool, CycleError> {
    if !d.store.pause_if_running(reason, d.clock.now()).await? {
        return Ok(false);
    }
    journal(
        d,
        EventKind::Pause,
        None,
        None,
        format!("Автопауза: {reason}"),
        json!({ "reason": reason }),
    )
    .await?;
    d.bus.publish(EngineEventDto::AutoPaused {
        reason: reason.to_string(),
    });
    Ok(true)
}

/// `Paused` або `Killed` -> `Running` (у журналі — з якого режиму); у `Running` нічого не робить.
pub async fn resume(d: &Deps) -> Result<(), CycleError> {
    let previous = d.store.engine_mode().await?;
    let was = match &previous {
        EngineMode::Running => return Ok(()),
        EngineMode::Paused { reason } => format!("пауза: {reason}"),
        EngineMode::Killed { reason } => format!("аварійна зупинка: {reason}"),
    };
    d.store
        .set_engine_mode(&EngineMode::Running, d.clock.now())
        .await?;
    journal(
        d,
        EventKind::Pause,
        None,
        None,
        format!("Роботу відновлено (було — {was})"),
        json!({ "previous": previous }),
    )
    .await?;
    d.bus.publish(EngineEventDto::StatusChanged);
    Ok(())
}

/// Аварійна зупинка: режим `Killed` (безумовно), скасування намірів `PendingApproval` і
/// `Approved`, скасування відкритих ордерів (батьківських і ніг).
///
/// Без закриття позицій лишаються робочі захисні стопи: стоп-ордери відстежуваних позицій і
/// Sell Stop, чий батьківський ордер уже не відкритий (нога виконаного входу, який ще не
/// звірено, або окремий захисний стоп). Далі одразу виконується звірка: вона добирає часткові
/// виконання скасованих входів і відновлює стопи позицій, чиї виходи скасовано (пауза під
/// `Killed` не діє); її помилка — у журналі й у `KillReport::reconcile_error`.
///
/// Із закриттям скасовується все, крім власних продажів аварійного закриття цього дня, і на
/// кожну позицію брокера подається Sell Market Day з
/// `client_order_id = "fond-kill-{yyyymmdd ET}-{symbol}"`. Відправлений намір скасовується,
/// лише коли його ордер завершився без виконання й позиції за тікером не було; інакше він
/// лишається `Submitted`, щоб звірка взяла на облік і захистила те, що не продано.
/// Невдале скасування чи продаж — подія в журналі, а не помилка.
pub async fn kill(d: &Deps, close_positions: bool) -> Result<KillReport, CycleError> {
    let mut report = KillReport::default();
    let mode = EngineMode::Killed {
        reason: KILL_REASON.into(),
    };
    d.store.set_engine_mode(&mode, d.clock.now()).await?;
    let what = if close_positions {
        "нові ордери заборонено, позиції закриваються"
    } else {
        "нові ордери заборонено, позиції лишаються під стопами"
    };
    journal(
        d,
        EventKind::Kill,
        None,
        None,
        format!("{KILL_REASON}: {what}"),
        json!({ "close_positions": close_positions }),
    )
    .await?;
    d.bus.publish(EngineEventDto::Killed {
        reason: KILL_REASON.into(),
    });

    let waiting = d
        .store
        .intents_with_status(&[IntentStatus::PendingApproval, IntentStatus::Approved])
        .await?;
    for i in &waiting {
        if cancel_intent(d, &i.id, "Скасовано аварійною зупинкою").await?
        {
            report.intents_cancelled += 1;
        }
    }

    let open = match observe(d, d.broker.open_orders().await) {
        Ok(o) => o,
        Err(e) => {
            return broker_failure(d, "Аварійна зупинка: не вдалося отримати ордери", e).await;
        }
    };
    let tracked = d.store.positions().await?;
    let kill_prefix = format!("fond-kill-{}-", utc_to_et(d.clock.now()).format("%Y%m%d"));
    let tracked_stops: HashSet<&str> = tracked
        .iter()
        .filter_map(|p| p.stop_order_id.as_deref())
        .collect();
    let mut failures = 0;
    for (o, parent) in flatten(&open) {
        if !o.status.is_open() {
            continue;
        }
        let keep = if close_positions {
            o.client_order_id.starts_with(&kill_prefix)
        } else {
            tracked_stops.contains(o.id.as_str()) || live_protection(o, parent)
        };
        if keep {
            continue;
        }
        match cancel(d, o).await? {
            Cancel::Done => report.orders_cancelled += 1,
            Cancel::NotOpen => {}
            Cancel::Failed => failures += 1,
        }
    }

    if close_positions {
        let (closed, had_position) = close_all(d, &tracked, &kill_prefix, &mut failures).await?;
        report.positions_closed = closed;
        report.intents_cancelled += settle_submitted(d, &had_position).await?;
    } else if let Err(e) = reconcile(d).await {
        journal(
            d,
            EventKind::Error,
            None,
            None,
            format!("Аварійна зупинка: звірку після скасування ордерів не завершено: {e}"),
            json!({ "error": e.to_string() }),
        )
        .await?;
        report.reconcile_error = Some(e.to_string());
    }

    journal(
        d,
        EventKind::Kill,
        None,
        None,
        format!(
            "Аварійна зупинка: скасовано ордерів {}, закрито позицій {}, скасовано намірів {}, \
             помилок {failures}",
            report.orders_cancelled, report.positions_closed, report.intents_cancelled
        ),
        json!({
            "orders_cancelled": report.orders_cancelled,
            "positions_closed": report.positions_closed,
            "intents_cancelled": report.intents_cancelled,
            "failures": failures,
            "reconcile_error": report.reconcile_error,
        }),
    )
    .await?;
    Ok(report)
}

/// Робочий захисний стоп без прив'язки до відстежуваної позиції: Sell Stop, що не чекає
/// виконання свого батьківського ордера.
fn live_protection(o: &BrokerOrder, parent: Option<&BrokerOrder>) -> bool {
    o.side == Side::Sell
        && o.order_type == OrderType::Stop
        && o.status != OrderStatus::Held
        && parent.is_none_or(|p| !p.status.is_open())
}

/// `false`, якщо статус наміру щойно змінився (перехід уже недопустимий).
async fn cancel_intent(d: &Deps, id: &str, note: &str) -> Result<bool, CycleError> {
    match d
        .store
        .update_intent(id, IntentStatus::Cancelled, d.clock.now(), None, Some(note))
        .await
    {
        Ok(_) => Ok(true),
        Err(StoreError::InvalidTransition { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

enum Cancel {
    /// Скасовано (зокрема разом із батьківським ордером).
    Done,
    /// Ордер устиг завершитись інакше (наприклад, виконався).
    NotOpen,
    Failed,
}

async fn cancel(d: &Deps, o: &BrokerOrder) -> Result<Cancel, CycleError> {
    let err = match observe(d, d.broker.cancel(&o.id).await) {
        Ok(()) => return Ok(Cancel::Done),
        Err(e) => e,
    };
    match observe(d, d.broker.order(&o.id).await) {
        Ok(now) if now.status == OrderStatus::Canceled => return Ok(Cancel::Done),
        // Лише остаточний стан: `pending_cancel` та інші невідомі ще можуть виконатись.
        Ok(now) if now.status.is_terminal() => return Ok(Cancel::NotOpen),
        _ => {}
    }
    journal(
        d,
        EventKind::Error,
        Some(&o.symbol),
        None,
        format!(
            "Аварійна зупинка: не вдалося скасувати ордер {}: {err}",
            o.id
        ),
        json!({ "order_id": o.id, "error": err.to_string() }),
    )
    .await?;
    Ok(Cancel::Failed)
}

/// Відправлені входи й виходи після аварійного закриття. Намір скасовується, лише коли його
/// ордер завершився без жодного виконання й позиції за тікером у брокера не було (або ордера
/// немає взагалі); інакше він лишається `Submitted`, щоб звірка взяла виконання на облік і
/// захистила те, що не вдалося продати. Причина — у журналі.
async fn settle_submitted(d: &Deps, had_position: &HashSet<Symbol>) -> Result<usize, CycleError> {
    let submitted = d
        .store
        .intents_with_status(&[IntentStatus::Submitted])
        .await?;
    let mut cancelled = 0;
    for i in submitted
        .iter()
        .filter(|i| matches!(i.kind, IntentKind::Entry { .. } | IntentKind::Exit { .. }))
    {
        let cid = client_order_id(&i.id);
        let keep = match observe(d, d.broker.order_by_client_id(&cid).await) {
            Err(e) => Some(format!("не вдалося перевірити ордер: {e}")),
            Ok(None) => None,
            Ok(Some(o)) if !o.status.is_terminal() => Some("ордер ще не скасовано".to_string()),
            Ok(Some(o)) if o.filled_qty > 0 => {
                Some(format!("ордер виконано: {} шт.", o.filled_qty))
            }
            Ok(Some(_)) if had_position.contains(&i.symbol) => {
                Some(format!("у брокера є позиція {}", i.symbol))
            }
            Ok(Some(_)) => None,
        };
        match keep {
            Some(why) => {
                journal(
                    d,
                    EventKind::Intent,
                    Some(&i.symbol),
                    Some(&i.id),
                    format!("Намір лишається відправленим ({why}): позицію добере звірка"),
                    json!({ "reason": why }),
                )
                .await?;
            }
            None => {
                let note = "Скасовано аварійною зупинкою: ордер не виконано, позиції немає";
                if cancel_intent(d, &i.id, note).await? {
                    cancelled += 1;
                }
            }
        }
    }
    Ok(cancelled)
}

/// Продає за ринком кожну довгу позицію брокера. Повертає кількість поданих продажів і
/// тікери, за якими у брокера була позиція.
async fn close_all(
    d: &Deps,
    tracked: &[TrackedPosition],
    kill_prefix: &str,
    failures: &mut usize,
) -> Result<(usize, HashSet<Symbol>), CycleError> {
    let positions = match observe(d, d.broker.positions().await) {
        Ok(p) => p,
        Err(e) => {
            return broker_failure(d, "Аварійна зупинка: не вдалося отримати позиції", e).await;
        }
    };
    let mut closed = 0;
    let had_position: HashSet<Symbol> = positions
        .iter()
        .filter(|p| p.qty != 0)
        .map(|p| p.symbol.clone())
        .collect();
    for bp in positions.iter().filter(|p| p.qty != 0) {
        let sym = &bp.symbol;
        let Some(qty) = u32::try_from(bp.qty).ok().filter(|q| *q > 0) else {
            *failures += 1;
            journal(
                d,
                EventKind::Error,
                Some(sym),
                None,
                format!(
                    "Аварійна зупинка: позицію {sym} ({} шт.) не закрито — закрийте вручну",
                    bp.qty
                ),
                json!({ "qty": bp.qty }),
            )
            .await?;
            continue;
        };
        let cid = format!("{kill_prefix}{sym}");
        let req = OrderRequest {
            client_order_id: cid.clone(),
            symbol: sym.clone(),
            side: Side::Sell,
            qty,
            order_type: OrderType::Market,
            limit_price: None,
            stop_price: None,
            tif: TimeInForce::Day,
            class: OrderClass::Simple,
            stop_loss: None,
            take_profit: None,
        };
        let sent = match observe(d, d.broker.submit(&req).await) {
            Ok(o) => Ok(o),
            // Повторна зупинка того ж дня: продаж уже подано.
            Err(BrokerError::DuplicateClientOrderId) => {
                match observe(d, d.broker.order_by_client_id(&cid).await) {
                    Ok(Some(o)) if o.status.is_open() || o.status == OrderStatus::Filled => Ok(o),
                    Ok(_) => {
                        Err("продаж з цим client_order_id уже завершився без виконання".into())
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        let held = tracked.iter().find(|p| p.position.symbol == *sym);
        match sent {
            Ok(o) => {
                d.store.upsert_order(&o, d.clock.now()).await?;
                if held.is_some() {
                    d.store.remove_position(sym).await?;
                }
                journal(
                    d,
                    EventKind::Order,
                    Some(sym),
                    held.map(|p| p.intent_id.as_str()),
                    format!(
                        "Аварійне закриття: продаж {qty} шт. за ринком, ордер {}",
                        o.id
                    ),
                    json!({ "order_id": o.id, "client_order_id": cid, "qty": qty }),
                )
                .await?;
                closed += 1;
            }
            Err(why) => {
                *failures += 1;
                journal(
                    d,
                    EventKind::Error,
                    Some(sym),
                    None,
                    format!("Аварійна зупинка: не вдалося продати {qty} шт. {sym}: {why}"),
                    json!({ "client_order_id": cid, "error": why }),
                )
                .await?;
                // Стопи вже скасовано: позиція, що лишилась, знову під захистом.
                if let Some(p) = held {
                    reprotect_after_failed_sale(d, p).await?;
                }
            }
        }
    }
    Ok((closed, had_position))
}

async fn reprotect_after_failed_sale(d: &Deps, p: &TrackedPosition) -> Result<(), CycleError> {
    match ensure_stop(d, p).await {
        Ok(_) => Ok(()),
        Err(CycleError::Store(e)) => Err(CycleError::Store(e)),
        Err(e) => {
            let sym = &p.position.symbol;
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&p.intent_id),
                format!("Не вдалося захистити позицію {sym} стопом: {e}"),
                json!({ "error": e.to_string() }),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Utc};
    use fond_api::events::EngineEventDto;
    use fond_core::ids::IntentTag;
    use fond_core::intent::{IntentKind, IntentStatus};
    use fond_core::orders::{OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce};
    use fond_core::testkit::FakeClock;
    use fond_core::traits::{Broker, Clock};
    use fond_core::types::EngineMode;
    use rust_decimal_macros::dec;

    use super::super::reconcile::reconcile;
    use super::super::test_support::{
        Handles, drain, entry_kind, et, fail_inserts, fixture, held_aapl, intent_with,
        journal_messages, session, submit_entry, submit_exit, sym,
    };
    use super::*;

    async fn setup() -> (Deps, Handles) {
        let (dp, h) = fixture().await;
        h.clock.set(et(session(), 10, 30));
        (dp, h)
    }

    fn killed() -> EngineMode {
        EngineMode::Killed {
            reason: KILL_REASON.into(),
        }
    }

    async fn status(dp: &Deps, id: &str) -> IntentStatus {
        dp.store.intent(id).await.unwrap().unwrap().status
    }

    async fn order_status(h: &Handles, id: &str) -> OrderStatus {
        h.broker.order(id).await.unwrap().status
    }

    #[tokio::test]
    async fn pause_from_running_publishes_once() {
        let (dp, _h) = setup().await;
        let mut rx = dp.bus.subscribe();

        pause(&dp, "Ліміт ордерів на день").await.unwrap();

        let paused = EngineMode::Paused {
            reason: "Ліміт ордерів на день".into(),
        };
        assert_eq!(dp.store.engine_mode().await.unwrap(), paused);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], EngineEventDto::Journal { .. }));
        assert_eq!(
            events[1],
            EngineEventDto::AutoPaused {
                reason: "Ліміт ордерів на день".into()
            }
        );
        assert_eq!(
            journal_messages(&dp).await,
            vec!["Автопауза: Ліміт ордерів на день".to_string()]
        );

        // Уже на паузі: причина не перезаписується, подій немає.
        pause(&dp, "Розбіжність з брокером: x").await.unwrap();
        assert_eq!(dp.store.engine_mode().await.unwrap(), paused);
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn pause_does_not_override_killed() {
        let (dp, _h) = setup().await;
        kill(&dp, false).await.unwrap();
        let mut rx = dp.bus.subscribe();
        let before = journal_messages(&dp).await;

        pause(&dp, "Розбіжність з брокером: TSLA").await.unwrap();

        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        assert!(drain(&mut rx).is_empty());
        assert_eq!(journal_messages(&dp).await, before);
    }

    /// Годинник, що один раз зупиняє виклик `now()`, доки тест не відпустить його.
    /// `block_in_place` віддає чергу задач робочого потоку іншим (зокрема повернення
    /// з'єднання в пул), тож решта рушія під час очікування працює.
    struct GateClock {
        inner: Arc<FakeClock>,
        gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    }

    impl Clock for GateClock {
        fn now(&self) -> DateTime<Utc> {
            let gate = self.gate.lock().unwrap().take();
            if let Some((reached, release)) = gate {
                reached.send(()).unwrap();
                tokio::task::block_in_place(|| release.recv().unwrap());
            }
            self.inner.now()
        }
    }

    /// Аварійна зупинка, що встигла між рішенням паузи й записом, не «понижується»:
    /// пауза зупиняється на першому `clock.now()`, тоді виконується `kill`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_kill_is_not_downgraded_by_pause() {
        let (dp, h) = setup().await;
        let mut rx = dp.bus.subscribe();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let gated = Deps {
            clock: Arc::new(GateClock {
                inner: h.clock.clone(),
                gate: Mutex::new(Some((reached_tx, release_rx))),
            }),
            ..dp.clone()
        };
        let pauser = tokio::spawn(async move {
            pause(&gated, "Розбіжність з брокером: TSLA").await
        });
        tokio::task::spawn_blocking(move || reached_rx.recv().unwrap())
            .await
            .unwrap();

        kill(&dp, false).await.unwrap();
        release_tx.send(()).unwrap();
        pauser.await.unwrap().unwrap();

        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        let events = drain(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, EngineEventDto::AutoPaused { .. })),
            "{events:?}"
        );
        assert!(
            !journal_messages(&dp)
                .await
                .iter()
                .any(|m| m.starts_with("Автопауза"))
        );
    }

    #[tokio::test]
    async fn kill_without_close_keeps_stop_legs() {
        let (dp, h) = setup().await;
        let (_, held) = held_aapl(&dp, &h).await;
        assert_eq!(held.stop_order_id.as_deref(), Some("ord-1-stop"));
        let (msft, _) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        let mut rx = dp.bus.subscribe();

        let report = kill(&dp, false).await.unwrap();

        let cancels = h.broker.cancels();
        assert!(!cancels.contains(&"ord-1-stop".to_string()), "{cancels:?}");
        assert!(cancels.contains(&"ord-2".to_string()), "{cancels:?}");
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
        assert_eq!(order_status(&h, "ord-2").await, OrderStatus::Canceled);
        assert_eq!(order_status(&h, "ord-2-stop").await, OrderStatus::Canceled);
        assert_eq!(
            report,
            KillReport {
                orders_cancelled: 2,
                positions_closed: 0,
                intents_cancelled: 0,
                reconcile_error: None,
            }
        );
        // Без закриття: продажів немає, позиція лишається; звірка після скасування
        // позначила невиконаний вхід MSFT скасованим.
        assert_eq!(h.broker.submitted().len(), 2);
        assert_eq!(dp.store.positions().await.unwrap(), vec![held]);
        assert_eq!(status(&dp, &msft.id).await, IntentStatus::Cancelled);
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        assert!(drain(&mut rx).contains(&EngineEventDto::Killed {
            reason: KILL_REASON.into()
        }));
        assert!(
            journal_messages(&dp)
                .await
                .iter()
                .any(|m| m.starts_with(KILL_REASON))
        );
    }

    #[tokio::test]
    async fn kill_without_close_keeps_stop_of_unreconciled_fill() {
        let (dp, h) = setup().await;
        // Вхід виконано, але звірки ще не було: нога вже захищає позицію.
        let (_, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        h.broker.fill(&order.id, dec!(101.00), 10);

        let report = kill(&dp, false).await.unwrap();

        assert!(h.broker.cancels().is_empty());
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
        assert_eq!(report.orders_cancelled, 0);
        // Звірка після зупинки взяла позицію на облік зі стоп-ногою.
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    }

    #[tokio::test]
    async fn kill_without_close_reprotects_cancelled_exit() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Крок відправки скасував стоп і подав вихід; зупинка скасовує вихід.
        h.broker.cancel("ord-1-stop").await.unwrap();
        let (exit, _) = submit_exit(&dp, "AAPL", 10).await;

        let report = kill(&dp, false).await.unwrap();

        assert_eq!(report.orders_cancelled, 1);
        assert_eq!(report.reconcile_error, None);
        assert_eq!(status(&dp, &exit.id).await, IntentStatus::Cancelled);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        let stop_id = held[0].stop_order_id.clone().unwrap();
        assert_eq!(stop_id, "ord-3");
        let stop = h.broker.order(&stop_id).await.unwrap();
        assert!(stop.status.is_open());
        assert_eq!(
            (stop.side, stop.order_type, stop.qty),
            (Side::Sell, OrderType::Stop, 10)
        );
    }

    #[tokio::test]
    async fn kill_without_close_protects_partial_open_entry() {
        let (dp, h) = setup().await;
        let (entry, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
        h.broker.fill(&order.id, dec!(101.00), 50);

        let report = kill(&dp, false).await.unwrap();

        assert_eq!(order_status(&h, "ord-1").await, OrderStatus::Canceled);
        assert_eq!(report.reconcile_error, None);
        assert_eq!(status(&dp, &entry.id).await, IntentStatus::Filled);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].position.qty, 50);
        let stop = h
            .broker
            .order(held[0].stop_order_id.as_deref().unwrap())
            .await
            .unwrap();
        assert!(stop.status.is_open());
        assert_eq!((stop.side, stop.qty), (Side::Sell, 50));
    }

    #[tokio::test]
    async fn kill_without_close_reports_reconcile_failure() {
        let (dp, _h) = setup().await;
        // Звірка після зупинки падає на записі знімка капіталу.
        fail_inserts(&dp.store, "equity_snapshots").await;

        let report = kill(&dp, false).await.unwrap();

        let err = report.reconcile_error.expect("помилка звірки у звіті");
        assert!(err.contains("inserts into equity_snapshots"), "{err}");
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.starts_with("Аварійна зупинка: звірку після скасування")),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn kill_cancel_failure_is_journaled_not_fatal() {
        let (dp, h) = setup().await;
        submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        h.broker.fail_cancel("ord-1");
        h.broker.fail_cancel("ord-1-stop");

        let report = kill(&dp, false).await.unwrap();

        assert_eq!(report.orders_cancelled, 0);
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.contains("не вдалося скасувати ордер ord-1:")),
            "{journal:?}"
        );
        assert!(
            journal[0].starts_with("Аварійна зупинка: скасовано ордерів 0")
                && journal[0].ends_with("помилок 2"),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn kill_with_close_cancels_all_and_sells() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let (msft, _) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        h.broker.set_position(&sym("TSLA"), 5, dec!(250));
        // 21:00 ET 5 листопада = 02:00 UTC 6 листопада: дата в client id — за ET.
        h.clock.set(et(session(), 21, 0));

        let report = kill(&dp, true).await.unwrap();

        for id in ["ord-1-stop", "ord-2", "ord-2-stop"] {
            assert_eq!(order_status(&h, id).await, OrderStatus::Canceled, "{id}");
        }
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 4);
        for (req, (symbol, qty)) in submitted[2..].iter().zip([("AAPL", 10), ("TSLA", 5)]) {
            assert_eq!(req.symbol, sym(symbol));
            assert_eq!(req.side, Side::Sell);
            assert_eq!(req.order_type, OrderType::Market);
            assert_eq!(req.tif, TimeInForce::Day);
            assert_eq!(req.class, OrderClass::Simple);
            assert_eq!(req.qty, qty);
            assert_eq!(req.limit_price, None);
            assert_eq!(req.stop_price, None);
            assert_eq!(req.client_order_id, format!("fond-kill-20261105-{symbol}"));
        }
        assert_eq!(
            report,
            KillReport {
                orders_cancelled: 3,
                positions_closed: 2,
                intents_cancelled: 1,
                reconcile_error: None,
            }
        );
        assert!(dp.store.positions().await.unwrap().is_empty());
        // Відправлений вхід скасовано: звірка не відкриє позицію, яку щойно закрили.
        assert_eq!(status(&dp, &msft.id).await, IntentStatus::Cancelled);
        let open: Vec<String> = h
            .broker
            .open_orders()
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.client_order_id)
            .collect();
        assert_eq!(
            open,
            vec!["fond-kill-20261105-AAPL", "fond-kill-20261105-TSLA"]
        );
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
    }

    #[tokio::test]
    async fn repeated_kill_with_close_is_idempotent() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        kill(&dp, true).await.unwrap();
        assert_eq!(h.broker.submitted().len(), 2);

        // Продаж ще не виконано: повторна зупинка того ж дня не скасовує його й не дублює.
        let report = kill(&dp, true).await.unwrap();

        assert_eq!(h.broker.submitted().len(), 2);
        assert_eq!(order_status(&h, "ord-2").await, OrderStatus::Accepted);
        assert_eq!(
            report,
            KillReport {
                orders_cancelled: 0,
                positions_closed: 1,
                intents_cancelled: 0,
                reconcile_error: None,
            }
        );
    }

    #[tokio::test]
    async fn failed_kill_sale_on_unreconciled_fill_is_protected_by_reconcile() {
        let (dp, h) = setup().await;
        // Вхід виконано, звірки ще не було.
        let (entry, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        h.broker.fill(&order.id, dec!(101.00), 10);
        // Продаж аварійного закриття не пройде: ордер із тим самим id уже завершився.
        let dead = OrderRequest {
            client_order_id: "fond-kill-20261105-AAPL".into(),
            symbol: sym("AAPL"),
            side: Side::Sell,
            qty: 10,
            order_type: OrderType::Market,
            limit_price: None,
            stop_price: None,
            tif: TimeInForce::Day,
            class: OrderClass::Simple,
            stop_loss: None,
            take_profit: None,
        };
        let dead = dp.broker.submit(&dead).await.unwrap();
        h.broker.cancel(&dead.id).await.unwrap();

        let report = kill(&dp, true).await.unwrap();

        assert_eq!(report.positions_closed, 0);
        assert_eq!(report.intents_cancelled, 0);
        // Нога-стоп скасована, продажу немає: намір лишається відправленим для звірки.
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        assert_eq!(status(&dp, &entry.id).await, IntentStatus::Submitted);
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.starts_with("Намір лишається відправленим")),
            "{journal:?}"
        );

        reconcile(&dp).await.unwrap();

        assert_eq!(status(&dp, &entry.id).await, IntentStatus::Filled);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].position.qty, 10);
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-3"));
        let stop = h.broker.order("ord-3").await.unwrap();
        assert!(stop.status.is_open());
        assert_eq!(
            (stop.side, stop.order_type, stop.qty),
            (Side::Sell, OrderType::Stop, 10)
        );
    }

    #[tokio::test]
    async fn kill_sale_on_unreconciled_fill_gets_no_extra_stop() {
        let (dp, h) = setup().await;
        let (entry, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        h.broker.fill(&order.id, dec!(101.00), 10);

        let report = kill(&dp, true).await.unwrap();

        assert_eq!(report.positions_closed, 1);
        assert_eq!(status(&dp, &entry.id).await, IntentStatus::Submitted);

        // Звірка бере виконання на облік, але стоп поверх відкритого продажу не подає.
        reconcile(&dp).await.unwrap();
        assert_eq!(status(&dp, &entry.id).await, IntentStatus::Filled);
        assert_eq!(dp.store.positions().await.unwrap().len(), 1);
        assert_eq!(h.broker.submitted().len(), 2);
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());

        // Продаж виконано: позицію знято з обліку, нових ордерів немає.
        h.broker.fill("ord-2", dec!(100.00), 10);
        reconcile(&dp).await.unwrap();
        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(h.broker.submitted().len(), 2);
    }

    #[tokio::test]
    async fn kill_cancels_pending_and_approved_intents() {
        let (dp, _h) = setup().await;
        let entry = || entry_kind(10, dec!(101.00), dec!(95.50), None);
        let exit = IntentKind::Exit {
            qty: 5,
            reason: "вихід".into(),
        };
        let pending = intent_with(
            "MSFT",
            IntentTag::Entry,
            entry(),
            IntentStatus::PendingApproval,
            None,
        );
        let approved = intent_with(
            "AAPL",
            IntentTag::Entry,
            entry(),
            IntentStatus::Approved,
            None,
        );
        let approved_exit = intent_with("SPY", IntentTag::Exit, exit, IntentStatus::Approved, None);
        let submitted = intent_with(
            "NVDA",
            IntentTag::Entry,
            entry(),
            IntentStatus::Submitted,
            Some("ord-9"),
        );
        let rejected = intent_with(
            "XOM",
            IntentTag::Entry,
            entry(),
            IntentStatus::Rejected,
            None,
        );
        for i in [&pending, &approved, &approved_exit, &submitted, &rejected] {
            dp.store.insert_intent(i).await.unwrap();
        }

        let report = kill(&dp, false).await.unwrap();

        assert_eq!(report.intents_cancelled, 3);
        for i in [&pending, &approved, &approved_exit] {
            assert_eq!(
                status(&dp, &i.id).await,
                IntentStatus::Cancelled,
                "{}",
                i.id
            );
        }
        // Відправлений намір аварійна зупинка не скасовує; звірка після неї не знайшла
        // його ордера у брокера.
        assert_eq!(status(&dp, &submitted.id).await, IntentStatus::Failed);
        assert_eq!(status(&dp, &rejected.id).await, IntentStatus::Rejected);
    }

    #[tokio::test]
    async fn resume_from_killed_sets_running() {
        let (dp, _h) = setup().await;
        kill(&dp, false).await.unwrap();
        let mut rx = dp.bus.subscribe();

        resume(&dp).await.unwrap();

        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
        let events = drain(&mut rx);
        assert!(
            events.contains(&EngineEventDto::StatusChanged),
            "{events:?}"
        );
        assert!(
            journal_messages(&dp).await[0].starts_with("Роботу відновлено"),
            "{:?}",
            journal_messages(&dp).await
        );

        // У Running відновлення нічого не робить.
        resume(&dp).await.unwrap();
        assert!(drain(&mut rx).is_empty());

        // Із паузи — так само в Running.
        pause(&dp, "Ліміт ордерів на день").await.unwrap();
        resume(&dp).await.unwrap();
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }
}
