//! Портфель: позиції та рахунок.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::{StrategyId, Symbol};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Position {
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub qty: u32,
    pub entry_price: f64,
    pub entry_date: NaiveDate,
    pub stop_price: f64,
    pub take_profit: Option<f64>,
    pub sector: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrackedPosition {
    pub position: Position,
    pub stop_order_id: Option<String>,
    pub intent_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Account {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub equity: Decimal,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub cash: Decimal,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub buying_power: Decimal,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub last_equity: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BrokerPosition {
    pub symbol: Symbol,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub qty: i64,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub avg_entry_price: Decimal,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub market_value: Decimal,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub current_price: Decimal,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn account_decimals_serialize_as_strings() {
        let a = Account {
            equity: dec!(100000.50),
            cash: dec!(1),
            buying_power: dec!(2),
            last_equity: dec!(3),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["equity"], "100000.50");
        let back: Account = serde_json::from_value(v).unwrap();
        assert_eq!(back, a);
    }
}
