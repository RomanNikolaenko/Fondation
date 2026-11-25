//! Відправка ребалансу портфеля: перевірка ціни, Limit Day без стоп-леги.

use fond_api::config::EngineConfig;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::Side;
use fond_core::price::to_f64;
use fond_core::risk::price_sane;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::orders::{build_rebalance_order, submit_idempotent};
use super::{
    NO_LAST_PRICE, Outcome, fail, fail_with, last_price, percent, unanswered, unanswered_error,
};

/// Ребаланс: перевірка ціни, відправка, намір `Submitted` з id ордера. Купівля
/// проходить gate входів; продаж не обмежений gate — обсяг не більший за
/// позицію за побудовою (`compute_deltas`).
pub(super) async fn send_rebalance(
    d: &Deps,
    cfg: &EngineConfig,
    intent: &Intent,
) -> Result<Outcome, CycleError> {
    let IntentKind::Rebalance {
        side,
        qty,
        limit_price,
    } = &intent.kind
    else {
        return Ok(Outcome::Skipped);
    };
    let sym = &intent.symbol;
    let Some(last) = last_price(d, sym).await? else {
        return fail(d, intent, NO_LAST_PRICE).await;
    };
    let pct = cfg.risk.price_sanity_pct;
    if !price_sane(to_f64(*limit_price), last, pct) {
        let why = format!("Ціна поза межами ±{}% від останньої", percent(pct));
        let data = json!({ "limit_price": limit_price.to_string(), "last_price": last });
        return fail_with(d, intent, &why, data).await;
    }

    let req = build_rebalance_order(intent);
    let verb = match side {
        Side::Buy => "купівля",
        Side::Sell => "продаж",
    };
    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!("Відправляю ребаланс: {verb} {qty} шт. {sym} за лімітом {limit_price}"),
        json!({ "client_order_id": req.client_order_id, "last_price": last }),
    )
    .await?;
    match submit_idempotent(d, &req).await {
        Ok(order) => {
            let now = d.clock.now();
            d.store.upsert_order(&order, now).await?;
            d.store
                .update_intent(
                    &intent.id,
                    IntentStatus::Submitted,
                    now,
                    Some(&order.id),
                    None,
                )
                .await?;
            journal(
                d,
                EventKind::Order,
                Some(sym),
                Some(&intent.id),
                format!("Ребаланс відправлено: ордер {}", order.id),
                json!({
                    "order_id": order.id,
                    "client_order_id": order.client_order_id,
                    "status": order.status,
                }),
            )
            .await?;
            Ok(Outcome::Sent)
        }
        Err(e) if unanswered_error(&e) => unanswered(d, intent, &e).await,
        Err(e) => fail(d, intent, &format!("Помилка брокера: {e}")).await,
    }
}
