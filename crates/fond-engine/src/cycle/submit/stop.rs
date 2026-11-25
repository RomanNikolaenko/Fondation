//! Підтягування захисного стопа (намір `MoveStop`).

use fond_core::intent::{Intent, IntentStatus};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::{to_f64, to_price};
use fond_store::EventKind;
use rust_decimal::Decimal;
use serde_json::json;

use crate::cycle::reconcile::observe;
use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::{Outcome, fail, tracked};

/// Підтягування стопа: `replace_stop` поточного стоп-ордера відстежуваної позиції (не того, що
/// записано в намірі — позицію могли перезахистити), лише вгору.
pub(super) async fn move_stop(
    d: &Deps,
    intent: &Intent,
    new_stop: Decimal,
) -> Result<Outcome, CycleError> {
    let sym = &intent.symbol;
    let Some(tp) = tracked(d, sym).await? else {
        return fail(d, intent, "Позиція не відстежується").await;
    };
    let Some(stop_id) = tp.stop_order_id.clone() else {
        return fail(d, intent, "У позиції немає стоп-ордера").await;
    };
    if to_price(tp.position.stop_price).is_ok_and(|current| new_stop <= current) {
        let why = format!(
            "Стоп не підтягнуто: новий рівень {new_stop:.2} не вище поточного {:.2}",
            tp.position.stop_price
        );
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                d.clock.now(),
                None,
                Some(&why),
            )
            .await?;
        journal(
            d,
            EventKind::Intent,
            Some(sym),
            Some(&intent.id),
            why,
            json!({ "new_stop": new_stop.to_string(), "stop": tp.position.stop_price }),
        )
        .await?;
        return Ok(Outcome::Skipped);
    }

    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!("Відправляю: підтягування стопа {sym} до {new_stop} (ордер {stop_id})"),
        json!({ "stop_order_id": stop_id, "new_stop": new_stop.to_string() }),
    )
    .await?;
    let order = match observe(d, d.broker.replace_stop(&stop_id, new_stop).await) {
        Ok(o) => o,
        Err(e) => return fail(d, intent, &format!("Не вдалося підтягнути стоп: {e}")).await,
    };
    let now = d.clock.now();
    d.store.upsert_order(&order, now).await?;
    let updated = TrackedPosition {
        position: Position {
            stop_price: to_f64(new_stop),
            ..tp.position.clone()
        },
        stop_order_id: Some(order.id.clone()),
        ..tp.clone()
    };
    d.store.upsert_position(&updated, now).await?;
    d.store
        .update_intent(
            &intent.id,
            IntentStatus::Submitted,
            now,
            Some(&order.id),
            None,
        )
        .await?;
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, None, None)
        .await?;
    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!(
            "Стоп підтягнуто до {new_stop}: ордер {} (замість {stop_id})",
            order.id
        ),
        json!({
            "order_id": order.id,
            "previous_stop_order_id": stop_id,
            "new_stop": new_stop.to_string(),
        }),
    )
    .await?;
    Ok(Outcome::Sent)
}
