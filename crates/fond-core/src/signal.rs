//! Сигнали стратегій.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::types::{StrategyId, Symbol};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MarketContext {
    pub date: NaiveDate,
    pub market_filter_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntrySignal {
    pub strategy: StrategyId,
    pub symbol: Symbol,
    pub signal_date: NaiveDate,
    pub reference_price: f64,
    pub stop_price: f64,
    pub take_profit: Option<f64>,
    pub strength: f64,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExitDecision {
    Hold,
    Exit { reason: String },
    MoveStop { new_stop: f64, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_decision_is_internally_tagged() {
        assert_eq!(
            serde_json::to_string(&ExitDecision::Hold).unwrap(),
            r#"{"type":"hold"}"#
        );
        let v = serde_json::to_value(ExitDecision::MoveStop {
            new_stop: 9.5,
            reason: "r".into(),
        })
        .unwrap();
        assert_eq!(v["type"], "move_stop");
        assert_eq!(v["new_stop"], 9.5);
    }
}
