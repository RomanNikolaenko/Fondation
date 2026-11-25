//! Тести звірки: спільні фікстури; самі тести — у підмодулях.

mod fills;
mod legs;
mod mismatch;
mod protect;
mod rebalance;

use chrono::{DateTime, Duration, Utc};
use fond_api::events::EngineEventDto;
use fond_core::error::BrokerError;
use fond_core::ids::{IntentTag, client_order_id};
use fond_core::intent::{IntentKind, IntentStatus};
use fond_core::orders::{OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce};
use fond_core::traits::Broker;
use fond_core::types::{EngineMode, StrategyId};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::super::test_support::{
    Handles, d, drain, entry_kind, et, fixture, held_aapl, intent_with, journal_messages, session,
    submit_entry, submit_exit, submit_rebalance, sym, tracked,
};
use super::*;

/// Стенд із годинником на 10:30 ET сесії (після відкриття).
async fn setup() -> (Deps, Handles) {
    let (dp, h) = fixture().await;
    h.clock.set(et(session(), 10, 30));
    (dp, h)
}

async fn status(dp: &Deps, id: &str) -> IntentStatus {
    dp.store.intent(id).await.unwrap().unwrap().status
}

async fn equity_rows(dp: &Deps) -> Vec<(String, String, f64)> {
    sqlx::query_as("SELECT kind, session_date, equity FROM equity_snapshots ORDER BY id")
        .fetch_all(dp.store.pool())
        .await
        .unwrap()
}

async fn stored_order_status(dp: &Deps, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM orders WHERE id = ?")
        .bind(id)
        .fetch_optional(dp.store.pool())
        .await
        .unwrap()
}

fn price_of(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn filled_events(events: &[EngineEventDto]) -> Vec<(String, String, u32, Decimal)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEventDto::OrderFilled {
                symbol,
                side,
                qty,
                price,
            } => Some((symbol.clone(), side.clone(), *qty, price_of(price))),
            _ => None,
        })
        .collect()
}

fn protect_request(req: &OrderRequest, symbol: &str, qty: u32, stop: Decimal) {
    assert_eq!(req.symbol, sym(symbol));
    assert_eq!(req.side, Side::Sell);
    assert_eq!(req.order_type, OrderType::Stop);
    assert_eq!(req.class, OrderClass::Simple);
    assert_eq!(req.tif, TimeInForce::Gtc);
    assert_eq!(req.qty, qty);
    assert_eq!(req.stop_price, Some(stop));
    assert_eq!(req.limit_price, None);
    assert_eq!(req.stop_loss, None);
}

fn protect_cid(intent_id: &str, at: DateTime<Utc>) -> String {
    client_order_id(&format!("{intent_id}-protect-{}", at.timestamp()))
}

/// Робочі (відкриті, не `Held`) Sell Stop тікера у брокера: (id, qty).
async fn live_sell_stops(h: &Handles, symbol: &str) -> Vec<(String, u32)> {
    let open = h.broker.open_orders().await.unwrap();
    flatten(&open)
        .into_iter()
        .map(|(o, _)| o)
        .filter(|o| {
            o.symbol == sym(symbol)
                && o.side == Side::Sell
                && o.order_type == OrderType::Stop
                && o.status.is_open()
                && o.status != OrderStatus::Held
        })
        .map(|o| (o.id.clone(), o.qty))
        .collect()
}

fn stop_request(cid: &str, symbol: &str, qty: u32, stop: Decimal) -> OrderRequest {
    OrderRequest {
        client_order_id: cid.into(),
        symbol: sym(symbol),
        side: Side::Sell,
        qty,
        order_type: OrderType::Stop,
        limit_price: None,
        stop_price: Some(stop),
        tif: TimeInForce::Gtc,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    }
}

async fn order_status(h: &Handles, id: &str) -> OrderStatus {
    h.broker.order(id).await.unwrap().status
}

fn pending_cancel() -> OrderStatus {
    OrderStatus::Other("pending_cancel".into())
}

/// Bracket AAPL 10 шт. (стоп 95.50, ціль 115.25), виконаний і звірений:
/// стоп-нога `ord-1-stop`, ціль `ord-1-tp`.
async fn held_aapl_bracket(dp: &Deps, h: &Handles) {
    let (_, order) = submit_entry(
        dp,
        "AAPL",
        10,
        dec!(101.00),
        dec!(95.50),
        Some(dec!(115.25)),
    )
    .await;
    h.broker.fill(&order.id, dec!(101.00), 10);
    reconcile(dp).await.unwrap();
    assert_eq!(dp.store.positions().await.unwrap().len(), 1);
}
