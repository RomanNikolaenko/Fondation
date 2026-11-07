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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum OrderType {
    Market,
    Limit,
    Stop,
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
