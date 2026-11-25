//! Побудова ордерів з намірів і відправка без дублів (пошук за `client_order_id`).

use std::time::Duration;

use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind};
use fond_core::orders::{BrokerOrder, OrderClass, OrderRequest, OrderType, Side, TimeInForce};

use crate::cycle::reconcile::observe;
use crate::deps::Deps;

/// Паузи перед повторною відправкою після `Transient`, коли ордера у брокера не знайдено.
#[cfg(not(test))]
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
#[cfg(test)]
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(2)];

/// Buy Limit Day: OTO зі стопом або Bracket, якщо є ціль. Для наміру іншого виду `qty` = 0
/// (такий ордер брокер відхилить).
pub fn build_entry_order(i: &Intent) -> OrderRequest {
    let (qty, limit, stop, take_profit) = match &i.kind {
        IntentKind::Entry {
            qty,
            limit_price,
            stop_price,
            take_profit,
            ..
        } => (*qty, Some(*limit_price), Some(*stop_price), *take_profit),
        _ => (0, None, None, None),
    };
    OrderRequest {
        client_order_id: client_order_id(&i.id),
        symbol: i.symbol.clone(),
        side: Side::Buy,
        qty,
        order_type: OrderType::Limit,
        limit_price: limit,
        stop_price: None,
        tif: TimeInForce::Day,
        class: if take_profit.is_some() {
            OrderClass::Bracket
        } else {
            OrderClass::Oto
        },
        stop_loss: stop,
        take_profit,
    }
}

/// Sell Market Simple на обсяг наміру. Для наміру іншого виду `qty` = 0.
pub fn build_exit_order(i: &Intent, tif: TimeInForce) -> OrderRequest {
    let qty = match &i.kind {
        IntentKind::Exit { qty, .. } => *qty,
        _ => 0,
    };
    OrderRequest {
        client_order_id: client_order_id(&i.id),
        symbol: i.symbol.clone(),
        side: Side::Sell,
        qty,
        order_type: OrderType::Market,
        limit_price: None,
        stop_price: None,
        tif,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    }
}

/// Ребаланс: Buy/Sell Limit Day Simple без стоп-леги. Для наміру іншого виду `qty` = 0.
pub fn build_rebalance_order(i: &Intent) -> OrderRequest {
    let (side, qty, limit) = match &i.kind {
        IntentKind::Rebalance {
            side,
            qty,
            limit_price,
        } => (*side, *qty, Some(*limit_price)),
        _ => (Side::Buy, 0, None),
    };
    OrderRequest {
        client_order_id: client_order_id(&i.id),
        symbol: i.symbol.clone(),
        side,
        qty,
        order_type: OrderType::Limit,
        limit_price: limit,
        stop_price: None,
        tif: TimeInForce::Day,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    }
}

/// Відправка без дублів. Після `DuplicateClientOrderId` або `Transient` ордер шукається за
/// `client_order_id` (знайдено — його й повертаємо). `Transient` без знайденого ордера —
/// ще до двох спроб з паузами 1 с і 2 с. Кожен виклик брокера враховується в `BrokerHealth`.
pub async fn submit_idempotent(d: &Deps, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
    let mut delays = SUBMIT_RETRY_DELAYS.iter();
    loop {
        let err = match observe(d, d.broker.submit(req).await) {
            Ok(order) => return Ok(order),
            Err(e @ (BrokerError::DuplicateClientOrderId | BrokerError::Transient(_))) => e,
            Err(e) => return Err(e),
        };
        // Ордер міг дійти до брокера (відповідь втрачено) або вже існувати.
        if let Ok(Some(order)) = observe(d, d.broker.order_by_client_id(&req.client_order_id).await)
        {
            return Ok(order);
        }
        let delay = match (&err, delays.next()) {
            (BrokerError::Transient(_), Some(delay)) => *delay,
            _ => return Err(err),
        };
        tokio::time::sleep(delay).await;
    }
}
