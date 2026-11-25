//! Крок 1 звірки: стан відправлених намірів входу й виходу за ордерами брокера.

use fond_api::events::EngineEventDto;
use fond_core::calendar::utc_to_et;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderType, Side};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::to_f64;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::analysis::sector_of;
use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::positions::cancel_open_sells;
use super::protect::reprotect;
use super::rebalance::rebalance_order;
use super::{
    Pass, bump, flatten, observe, price_text, remaining_after_sale, side_name, status_text,
    with_qty,
};

/// Крок 1 для одного наміру: ордер за `client_order_id(intent.id)` і його наслідки.
pub(super) async fn submitted_intent(
    d: &Deps,
    intent: &Intent,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &intent.symbol;
    let cid = client_order_id(&intent.id);
    let order = match observe(d, d.broker.order_by_client_id(&cid).await) {
        Ok(Some(o)) => o,
        Ok(None) => {
            let why = "Ордер не знайдено у брокера";
            d.store
                .update_intent(
                    &intent.id,
                    IntentStatus::Failed,
                    d.clock.now(),
                    None,
                    Some(why),
                )
                .await?;
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!("{why}: {cid}"),
                json!({ "client_order_id": cid }),
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!("Звірка: не вдалося перевірити ордер {cid}: {e}"),
                json!({ "client_order_id": cid, "error": e.to_string() }),
            )
            .await?;
            pass.skip_stop_check.insert(sym.clone());
            pass.uncertain.insert(sym.clone());
            return Ok(());
        }
    };
    d.store.upsert_order(&order, d.clock.now()).await?;
    pass.report.orders_updated += 1;
    match intent.kind {
        IntentKind::Entry { .. } => entry_order(d, intent, &order, pass).await,
        IntentKind::Exit { qty, .. } => exit_order(d, intent, qty, &order, pass).await,
        IntentKind::MoveStop { .. } => Ok(()),
        IntentKind::Rebalance {
            side,
            qty,
            limit_price,
        } => rebalance_order(d, intent, side, qty, limit_price, &order, pass).await,
    }
}

/// Перша (у глибину) нога-стоп ордера.
fn stop_leg(o: &BrokerOrder) -> Option<&BrokerOrder> {
    o.legs.iter().find_map(|l| {
        if l.order_type == OrderType::Stop {
            Some(l)
        } else {
            stop_leg(l)
        }
    })
}

async fn entry_order(
    d: &Deps,
    intent: &Intent,
    order: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let IntentKind::Entry {
        limit_price,
        stop_price,
        take_profit,
        ..
    } = &intent.kind
    else {
        return Ok(());
    };
    let sym = &intent.symbol;
    let filled = i64::from(order.filled_qty);
    bump(&mut pass.bought, sym, filled);
    if !order.status.is_terminal() {
        bump(&mut pass.inflight, sym, filled);
        return Ok(());
    }
    let now = d.clock.now();
    if order.filled_qty == 0 {
        let why = format!("Вхід не виконано: ордер {}", status_text(&order.status));
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                now,
                Some(&order.id),
                Some(&why),
            )
            .await?;
        journal(
            d,
            EventKind::Order,
            Some(sym),
            Some(&intent.id),
            format!("{why} ({})", order.id),
            json!({ "order_id": order.id, "status": order.status }),
        )
        .await?;
        return Ok(());
    }

    // Виконано повністю або частково (залишок скасовано чи прострочено).
    let price = order.filled_avg_price.unwrap_or(*limit_price);
    let filled_at = order.filled_at.unwrap_or(now);
    let universe = d.store.universe(true).await?;
    let sector = sector_of(universe.iter().find(|e| e.symbol == *sym));
    let leg = stop_leg(order);
    let previous = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    // Повторна обробка того самого наміру (збій до позначки Filled) зберігає відновлений стоп.
    let stop_order_id = match previous {
        Some(p) if p.intent_id == intent.id => p.stop_order_id,
        Some(p) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!(
                    "Позиція {sym} уже відстежувалась (намір {}); її замінено новим входом",
                    p.intent_id
                ),
                json!({ "previous_intent": p.intent_id, "previous_qty": p.position.qty }),
            )
            .await?;
            leg.map(|l| l.id.clone())
        }
        None => leg.map(|l| l.id.clone()),
    };
    let tp = TrackedPosition {
        position: Position {
            symbol: sym.clone(),
            strategy: intent.strategy.clone(),
            qty: order.filled_qty,
            entry_price: to_f64(price),
            entry_date: utc_to_et(filled_at).date_naive(),
            stop_price: to_f64(*stop_price),
            take_profit: take_profit.map(to_f64),
            sector,
        },
        stop_order_id,
        intent_id: intent.id.clone(),
    };
    // Позиція зберігається раніше за позначку наміру: збій посередині не губить виконання.
    d.store.upsert_position(&tp, now).await?;
    pass.skip_stop_check.insert(sym.clone());
    if order.filled_qty < order.qty {
        // Ноги розраховані на весь ордер: ціль (OCO зі стопом) продала б більше, ніж куплено.
        cancel_target_legs(d, order, &intent.id).await?;
    }
    reprotect(d, &tp, leg, pass).await?;
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
        .await?;
    let price = price_text(Some(price));
    let partial = if order.filled_qty < order.qty {
        format!(" (частково: {} з {})", order.filled_qty, order.qty)
    } else {
        String::new()
    };
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&intent.id),
        format!(
            "Вхід виконано: {} шт. за {price}{partial}",
            order.filled_qty
        ),
        json!({
            "order_id": order.id,
            "qty": order.filled_qty,
            "ordered": order.qty,
            "price": price,
        }),
    )
    .await?;
    d.bus.publish(EngineEventDto::OrderFilled {
        symbol: sym.to_string(),
        side: side_name(order.side).into(),
        qty: order.filled_qty,
        price,
    });
    pass.report.entries_filled.push(sym.clone());
    Ok(())
}

/// Скасовує відкриті ноги-продажі входу, крім стопа (стоп замінює `ensure_stop`).
async fn cancel_target_legs(
    d: &Deps,
    order: &BrokerOrder,
    intent_id: &str,
) -> Result<(), CycleError> {
    let legs = flatten(std::slice::from_ref(order));
    for (leg, _) in legs.into_iter().skip(1) {
        if leg.side != Side::Sell || leg.order_type == OrderType::Stop || !leg.status.is_open() {
            continue;
        }
        let (kind, message) = match observe(d, d.broker.cancel(&leg.id).await) {
            Ok(()) => (
                EventKind::Order,
                format!(
                    "Ціль {} скасовано: вхід виконано частково ({} з {} шт.)",
                    leg.id, order.filled_qty, order.qty
                ),
            ),
            Err(e) => (
                EventKind::Error,
                format!("Не вдалося скасувати ціль {}: {e}", leg.id),
            ),
        };
        journal(
            d,
            kind,
            Some(&leg.symbol),
            Some(intent_id),
            message,
            json!({ "order_id": leg.id }),
        )
        .await?;
    }
    Ok(())
}

async fn exit_order(
    d: &Deps,
    intent: &Intent,
    planned: u32,
    order: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &intent.symbol;
    let sold = order.filled_qty;
    bump(&mut pass.sold, sym, i64::from(sold));
    if !order.status.is_terminal() {
        // Вихід ще виконується: стоп не відновлюється, щоб не задвоїти продаж.
        pass.skip_stop_check.insert(sym.clone());
        bump(&mut pass.inflight, sym, -i64::from(sold));
        return Ok(());
    }
    let now = d.clock.now();
    let held = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    if sold == 0 {
        let why = "Вихід не виконано";
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                now,
                Some(&order.id),
                Some(why),
            )
            .await?;
        journal(
            d,
            EventKind::Order,
            Some(sym),
            Some(&intent.id),
            format!("{why}: ордер {} {}", order.id, status_text(&order.status)),
            json!({ "order_id": order.id, "status": order.status }),
        )
        .await?;
        if let Some(tp) = held {
            pass.skip_stop_check.insert(sym.clone());
            reprotect(d, &tp, None, pass).await?;
        }
        return Ok(());
    }

    if let Some(tp) = held {
        pass.skip_stop_check.insert(sym.clone());
        let remaining = remaining_after_sale(tp.position.qty, sold, planned);
        if remaining == 0 {
            d.store.remove_position(sym).await?;
            cancel_open_sells(d, sym).await?;
        } else {
            let rest = with_qty(&tp, remaining);
            d.store.upsert_position(&rest, now).await?;
            reprotect(d, &rest, None, pass).await?;
        }
    }
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
        .await?;
    let price = price_text(order.filled_avg_price);
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&intent.id),
        format!("Вихід виконано: {sold} шт. за {price}"),
        json!({ "order_id": order.id, "qty": sold, "price": price }),
    )
    .await?;
    d.bus.publish(EngineEventDto::OrderFilled {
        symbol: sym.to_string(),
        side: side_name(order.side).into(),
        qty: sold,
        price,
    });
    pass.report.exits_filled.push(sym.clone());
    Ok(())
}
