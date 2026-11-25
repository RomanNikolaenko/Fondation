//! Крок 2 звірки: відстежувані позиції — спрацьовані стопи, закриття, скасування залишків.

use fond_api::events::EngineEventDto;
use fond_core::orders::{BrokerOrder, OrderStatus, Side};
use fond_core::portfolio::TrackedPosition;
use fond_core::types::Symbol;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::protect::{current_stop, reprotect};
use super::{
    Pass, bump, flatten, observe, price_text, remaining_after_sale, target_qty, with_qty, working,
};

/// Крок 2 для однієї позиції: стан її стоп-ордера й позиції у брокера.
pub(super) async fn check_stop(
    d: &Deps,
    tp: &TrackedPosition,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let stop = match current_stop(d, tp, None).await {
        Ok(stop) => stop,
        Err(e) => {
            let id = tp.stop_order_id.as_deref().unwrap_or_default();
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&tp.intent_id),
                format!("Звірка: не вдалося перевірити стоп-ордер {id} ({sym}): {e}"),
                json!({ "stop_order_id": id, "error": e.to_string() }),
            )
            .await?;
            pass.uncertain.insert(sym.clone());
            return Ok(());
        }
    };
    if let Some(stop) = &stop {
        d.store.upsert_order(stop, d.clock.now()).await?;
        pass.report.orders_updated += 1;
        if stop.status.is_terminal() && stop.filled_qty > 0 {
            return stop_filled(d, tp, stop, pass).await;
        }
    }
    let held = pass.held(sym);
    if held <= 0 {
        // Позицію закрито у брокера (ціль, продаж повз рушій): захищати нічого.
        return close_flat(d, tp, pass).await;
    }
    let Some(stop) = stop else {
        return reprotect(d, tp, None, pass).await;
    };
    if stop.status.is_open() {
        if stop.filled_qty > 0 {
            // Стоп саме виконується: позицію уточнить наступна звірка.
            let filled = i64::from(stop.filled_qty);
            bump(&mut pass.inflight, sym, -filled);
            bump(&mut pass.sold, sym, filled);
            return Ok(());
        }
        if working(&stop, sym, target_qty(tp.position.qty, held, 0)) {
            return Ok(());
        }
        return reprotect(d, tp, Some(&stop), pass).await;
    }
    if !stop.status.is_terminal() {
        // Невідомий стан (наприклад, `pending_cancel`): нічого не змінюється до наступної
        // звірки — ні нового стопа, ні порівняння.
        pass.uncertain.insert(sym.clone());
        return Ok(());
    }
    reprotect(d, tp, Some(&stop), pass).await
}

/// Стоп виконано (повністю або частково, далі не виконується).
async fn stop_filled(
    d: &Deps,
    tp: &TrackedPosition,
    stop: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let sold = stop.filled_qty;
    bump(&mut pass.sold, sym, i64::from(sold));
    let remaining = remaining_after_sale(tp.position.qty, sold, stop.qty);
    let closed = remaining == 0 || pass.held(sym) <= 0;
    let rest = with_qty(tp, remaining);
    if closed {
        d.store.remove_position(sym).await?;
    } else {
        d.store.upsert_position(&rest, d.clock.now()).await?;
    }
    let price = price_text(stop.filled_avg_price);
    let tail = if closed {
        String::new()
    } else {
        format!(", залишок {remaining} шт.")
    };
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&tp.intent_id),
        format!("Спрацював стоп: {sold} шт. за {price}{tail}"),
        json!({ "order_id": stop.id, "qty": sold, "price": price, "remaining": remaining }),
    )
    .await?;
    if stop.status == OrderStatus::Filled {
        d.bus.publish(EngineEventDto::StopHit {
            symbol: sym.to_string(),
            price,
        });
        pass.report.stops_hit.push(sym.clone());
    }
    if closed {
        cancel_open_sells(d, sym).await
    } else {
        reprotect(d, &rest, Some(stop), pass).await
    }
}

/// Позиції у брокера більше немає: відстеження знімається, решта продажів тікера
/// скасовується, щоб жоден не відкрив шорт.
pub(super) async fn close_flat(
    d: &Deps,
    tp: &TrackedPosition,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    d.store.remove_position(sym).await?;
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&tp.intent_id),
        format!(
            "Позицію {sym} закрито у брокера ({} шт.): відстеження знято",
            tp.position.qty
        ),
        json!({ "qty": tp.position.qty, "stop_order_id": tp.stop_order_id }),
    )
    .await?;
    cancel_open_sells(d, sym).await?;
    pass.report.exits_filled.push(sym.clone());
    Ok(())
}

/// Скасовує відкриті продажі тікера, чия позиція закрита. Ноги ще відкритого батьківського
/// ордера (новий вхід) не чіпаються. Невдача — подія в журналі, не помилка.
pub(super) async fn cancel_open_sells(d: &Deps, sym: &Symbol) -> Result<(), CycleError> {
    let open = match observe(d, d.broker.open_orders().await) {
        Ok(o) => o,
        Err(e) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                None,
                format!("Не вдалося перевірити відкриті продажі {sym}: {e}"),
                json!({ "error": e.to_string() }),
            )
            .await?;
            return Ok(());
        }
    };
    for (o, parent) in flatten(&open) {
        if o.symbol != *sym
            || o.side != Side::Sell
            || !o.status.is_open()
            || parent.is_some_and(|p| p.status.is_open())
        {
            continue;
        }
        let (kind, message) = match observe(d, d.broker.cancel(&o.id).await) {
            Ok(()) => (
                EventKind::Order,
                format!("Скасовано продаж {} ({sym}): позицію закрито", o.id),
            ),
            Err(e) => (
                EventKind::Error,
                format!("Не вдалося скасувати продаж {} ({sym}): {e}", o.id),
            ),
        };
        journal(
            d,
            kind,
            Some(sym),
            None,
            message,
            json!({ "order_id": o.id }),
        )
        .await?;
    }
    Ok(())
}
