//! Дротові типи Alpaca (числа приходять рядками) і перетворення у доменні.

use chrono::{DateTime, Utc};
use fond_core::error::BrokerError;
use fond_core::orders::{BrokerOrder, OrderStatus, OrderType, Side};
use fond_core::portfolio::{Account, BrokerPosition};
use fond_core::types::Symbol;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;

fn bad(what: &str, v: &str) -> BrokerError {
    BrokerError::Other(format!("unexpected {what} from broker: {v:?}"))
}

fn qty_u32(s: &str) -> Result<u32, BrokerError> {
    s.parse::<Decimal>()
        .ok()
        .and_then(|d| d.trunc().to_u32())
        .ok_or_else(|| bad("quantity", s))
}

fn qty_i64(s: &str) -> Result<i64, BrokerError> {
    s.parse::<Decimal>()
        .ok()
        .and_then(|d| d.trunc().to_i64())
        .ok_or_else(|| bad("quantity", s))
}

#[derive(Debug, Deserialize)]
pub struct WireAccount {
    pub equity: Decimal,
    pub cash: Decimal,
    pub buying_power: Decimal,
    pub last_equity: Decimal,
}

impl From<WireAccount> for Account {
    fn from(w: WireAccount) -> Self {
        Self {
            equity: w.equity,
            cash: w.cash,
            buying_power: w.buying_power,
            last_equity: w.last_equity,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WirePosition {
    pub symbol: String,
    pub qty: String,
    pub avg_entry_price: Decimal,
    pub market_value: Decimal,
    pub current_price: Decimal,
}

impl TryFrom<WirePosition> for BrokerPosition {
    type Error = BrokerError;

    fn try_from(w: WirePosition) -> Result<Self, BrokerError> {
        Ok(Self {
            symbol: Symbol::new(&w.symbol),
            qty: qty_i64(&w.qty)?,
            avg_entry_price: w.avg_entry_price,
            market_value: w.market_value,
            current_price: w.current_price,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct WireOrder {
    pub id: String,
    pub client_order_id: String,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    #[serde(default)]
    pub qty: Option<String>,
    #[serde(default)]
    pub filled_qty: Option<String>,
    #[serde(default)]
    pub filled_avg_price: Option<Decimal>,
    #[serde(default)]
    pub limit_price: Option<Decimal>,
    #[serde(default)]
    pub stop_price: Option<Decimal>,
    pub status: String,
    #[serde(default)]
    pub legs: Option<Vec<WireOrder>>,
    #[serde(default)]
    pub filled_at: Option<DateTime<Utc>>,
}

pub fn parse_status(s: &str) -> OrderStatus {
    match s {
        "new" => OrderStatus::New,
        "accepted" => OrderStatus::Accepted,
        "pending_new" => OrderStatus::PendingNew,
        "held" => OrderStatus::Held,
        "partially_filled" => OrderStatus::PartiallyFilled,
        "filled" => OrderStatus::Filled,
        "done_for_day" => OrderStatus::DoneForDay,
        "canceled" => OrderStatus::Canceled,
        "expired" => OrderStatus::Expired,
        "rejected" => OrderStatus::Rejected,
        "replaced" => OrderStatus::Replaced,
        other => OrderStatus::Other(other.to_string()),
    }
}

impl TryFrom<WireOrder> for BrokerOrder {
    type Error = BrokerError;

    fn try_from(w: WireOrder) -> Result<Self, BrokerError> {
        let side = match w.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            other => return Err(bad("order side", other)),
        };
        let order_type = match w.order_type.as_str() {
            "market" => OrderType::Market,
            "limit" => OrderType::Limit,
            "stop" => OrderType::Stop,
            other => OrderType::Other(other.to_string()),
        };
        let legs = w
            .legs
            .unwrap_or_default()
            .into_iter()
            .map(BrokerOrder::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            id: w.id,
            client_order_id: w.client_order_id,
            symbol: Symbol::new(&w.symbol),
            side,
            order_type,
            // Notional-ордери приходять без qty: трактуємо як 0.
            qty: match w.qty.as_deref() {
                Some(s) => qty_u32(s)?,
                None => 0,
            },
            filled_qty: match w.filled_qty.as_deref() {
                Some(s) => qty_u32(s)?,
                None => 0,
            },
            filled_avg_price: w.filled_avg_price,
            limit_price: w.limit_price,
            stop_price: w.stop_price,
            status: parse_status(&w.status),
            legs,
            filled_at: w.filled_at,
        })
    }
}
