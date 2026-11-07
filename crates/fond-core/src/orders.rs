//! Запити та стани ордерів.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::Symbol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Side {
    Buy,
    Sell,
}

/// Тип ордера. Невідомі брокерські типи (`stop_limit`, `trailing_stop` …) зберігаються в `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    Other(String),
}

impl From<String> for OrderType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "market" => Self::Market,
            "limit" => Self::Limit,
            "stop" => Self::Stop,
            _ => Self::Other(s),
        }
    }
}

impl From<OrderType> for String {
    fn from(t: OrderType) -> Self {
        match t {
            OrderType::Market => "market".into(),
            OrderType::Limit => "limit".into(),
            OrderType::Stop => "stop".into(),
            OrderType::Other(s) => s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum TimeInForce {
    Day,
    Gtc,
    Opg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum OrderClass {
    Simple,
    Oto,
    Bracket,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OrderRequest {
    pub client_order_id: String,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: u32,
    pub order_type: OrderType,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub limit_price: Option<Decimal>,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub stop_price: Option<Decimal>,
    pub tif: TimeInForce,
    pub class: OrderClass,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub stop_loss: Option<Decimal>,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub take_profit: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum OrderStatus {
    New,
    Accepted,
    PendingNew,
    Held,
    PartiallyFilled,
    Filled,
    DoneForDay,
    Canceled,
    Expired,
    Rejected,
    Replaced,
    Other(String),
}

impl OrderStatus {
    /// Ордер ще може виконатись.
    pub fn is_open(&self) -> bool {
        matches!(
            self,
            Self::New | Self::Accepted | Self::PendingNew | Self::Held | Self::PartiallyFilled
        )
    }

    /// Остаточний стан. `Other` не вважається ні відкритим, ні остаточним.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Filled
                | Self::DoneForDay
                | Self::Canceled
                | Self::Expired
                | Self::Rejected
                | Self::Replaced
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BrokerOrder {
    pub id: String,
    pub client_order_id: String,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub qty: u32,
    pub filled_qty: u32,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub filled_avg_price: Option<Decimal>,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub limit_price: Option<Decimal>,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub stop_price: Option<Decimal>,
    pub status: OrderStatus,
    pub legs: Vec<BrokerOrder>,
    pub filled_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_type_serde_roundtrip() {
        for (t, raw) in [
            (OrderType::Market, "\"market\""),
            (OrderType::Limit, "\"limit\""),
            (OrderType::Stop, "\"stop\""),
            (OrderType::Other("stop_limit".into()), "\"stop_limit\""),
        ] {
            assert_eq!(serde_json::to_string(&t).unwrap(), raw);
            assert_eq!(serde_json::from_str::<OrderType>(raw).unwrap(), t);
        }
    }

    #[test]
    fn status_open_and_terminal() {
        assert!(OrderStatus::Held.is_open());
        assert!(OrderStatus::PartiallyFilled.is_open());
        assert!(!OrderStatus::Filled.is_open());
        assert!(OrderStatus::Replaced.is_terminal());
        assert!(OrderStatus::Canceled.is_terminal());
        assert!(!OrderStatus::New.is_terminal());
        let o = OrderStatus::Other("calculated".into());
        assert!(!o.is_open() && !o.is_terminal());
    }

    #[test]
    fn status_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&OrderStatus::PendingNew).unwrap(),
            "\"pending_new\""
        );
        assert_eq!(serde_json::to_string(&TimeInForce::Gtc).unwrap(), "\"gtc\"");
    }
}
