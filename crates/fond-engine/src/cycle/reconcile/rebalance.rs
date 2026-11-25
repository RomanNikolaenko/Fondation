//! Звірка намірів ребалансу: купівля — позиція `portfolio` без стопа, продаж — зменшення обсягу.

use fond_api::events::EngineEventDto;
use fond_core::calendar::utc_to_et;
use fond_core::intent::{Intent, IntentStatus};
use fond_core::orders::{BrokerOrder, Side};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::to_f64;
use fond_store::EventKind;
use rust_decimal::Decimal;
use serde_json::json;

use crate::cycle::analysis::sector_of;
use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::positions::cancel_open_sells;
use super::{Pass, bump, price_text, side_name, status_text, with_qty};

/// Крок 1 для ребалансу. Купівля: створює/поповнює позицію стратегії `portfolio`
/// без стопа (захист не потрібен — це довгострокова цільова вага). Продаж:
/// зменшує кількість (нуль → позицію знято), re-protect не потрібен.
/// Позиція swing-стратегії не торкається: розбіжність поставить паузу.
pub(super) async fn rebalance_order(
    d: &Deps,
    intent: &Intent,
    side: Side,
    _qty: u32,
    limit_price: Decimal,
    order: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &intent.symbol;
    let filled = i64::from(order.filled_qty);
    match side {
        Side::Buy => bump(&mut pass.bought, sym, filled),
        Side::Sell => bump(&mut pass.sold, sym, filled),
    }
    if !order.status.is_terminal() {
        pass.skip_stop_check.insert(sym.clone());
        bump(
            &mut pass.inflight,
            sym,
            if side == Side::Buy { filled } else { -filled },
        );
        return Ok(());
    }
    let now = d.clock.now();
    if order.filled_qty == 0 {
        let why = format!("Ребаланс не виконано: ордер {}", status_text(&order.status));
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
        pass.skip_stop_check.insert(sym.clone());
        return Ok(());
    }

    let price = order.filled_avg_price.unwrap_or(limit_price);
    let filled_at = order.filled_at.unwrap_or(now);
    let previous = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    if let Some(tp) = previous
        .as_ref()
        .filter(|tp| tp.position.strategy.as_str() != fond_core::rebalance::PORTFOLIO_STRATEGY)
    {
        let why = format!(
            "Позиція {sym} належить стратегії {}; виконання ребалансу не об'єднано",
            tp.position.strategy
        );
        journal(
            d,
            EventKind::Error,
            Some(sym),
            Some(&intent.id),
            why,
            json!({ "order_id": order.id, "strategy": tp.position.strategy }),
        )
        .await?;
        pass.skip_stop_check.insert(sym.clone());
        d.store
            .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
            .await?;
        return Ok(());
    }

    match side {
        Side::Buy => {
            let sector = {
                let universe = d.store.universe(true).await?;
                sector_of(universe.iter().find(|e| e.symbol == *sym))
            };
            let tp = match previous {
                // Поповнення наявної позиції портфеля: кількість додається.
                Some(mut tp) => {
                    tp.position.qty += order.filled_qty;
                    tp
                }
                None => TrackedPosition {
                    position: Position {
                        symbol: sym.clone(),
                        strategy: intent.strategy.clone(),
                        qty: order.filled_qty,
                        entry_price: to_f64(price),
                        entry_date: utc_to_et(filled_at).date_naive(),
                        stop_price: 0.0,
                        take_profit: None,
                        sector,
                    },
                    stop_order_id: None,
                    intent_id: intent.id.clone(),
                },
            };
            // Позиція зберігається раніше за позначку наміру: збій посередині
            // не губить виконання.
            d.store.upsert_position(&tp, now).await?;
            pass.skip_stop_check.insert(sym.clone());
            d.store
                .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
                .await?;
        }
        Side::Sell => {
            pass.skip_stop_check.insert(sym.clone());
            if let Some(tp) = &previous {
                let remaining = tp.position.qty.saturating_sub(order.filled_qty);
                if remaining == 0 {
                    d.store.remove_position(sym).await?;
                    cancel_open_sells(d, sym).await?;
                } else {
                    d.store
                        .upsert_position(&with_qty(tp, remaining), now)
                        .await?;
                }
            }
            d.store
                .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
                .await?;
        }
    }
    let price_text = price_text(Some(price));
    let partial = if order.filled_qty < order.qty {
        format!(" (частково: {} з {})", order.filled_qty, order.qty)
    } else {
        String::new()
    };
    let verb = match side {
        Side::Buy => "Купівля",
        Side::Sell => "Продаж",
    };
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&intent.id),
        format!(
            "Ребаланс виконано, {verb}: {} шт. за {price_text}{partial}",
            order.filled_qty
        ),
        json!({
            "order_id": order.id,
            "qty": order.filled_qty,
            "ordered": order.qty,
            "price": price_text,
        }),
    )
    .await?;
    d.bus.publish(EngineEventDto::OrderFilled {
        symbol: sym.to_string(),
        side: side_name(side).into(),
        qty: order.filled_qty,
        price: price_text,
    });
    pass.report.rebalances_filled.push(sym.clone());
    Ok(())
}
