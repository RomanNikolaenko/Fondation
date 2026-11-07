//! Детерміновані ідентифікатори намірів і клієнтських ордерів.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::types::{StrategyId, Symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum IntentTag {
    Entry,
    Exit,
    Stop,
}

impl IntentTag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Exit => "exit",
            Self::Stop => "stop",
        }
    }
}

/// `YYYYMMDD-strategy-SYMBOL-tag`.
pub fn intent_id(
    session: NaiveDate,
    strategy: &StrategyId,
    symbol: &Symbol,
    tag: IntentTag,
) -> String {
    format!(
        "{}-{}-{}-{}",
        session.format("%Y%m%d"),
        strategy,
        symbol,
        tag.as_str()
    )
}

pub fn client_order_id(intent_id: &str) -> String {
    format!("fond-{intent_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn intent_id_is_stable_and_readable() {
        let id = intent_id(
            d(2026, 11, 5),
            &StrategyId::new("trend_pullback"),
            &Symbol::new("AAPL"),
            IntentTag::Entry,
        );
        assert_eq!(id, "20261105-trend_pullback-AAPL-entry");
        assert_eq!(
            client_order_id(&id),
            "fond-20261105-trend_pullback-AAPL-entry"
        );
    }

    #[test]
    fn intent_id_allows_dotted_symbols() {
        let id = intent_id(
            d(2026, 11, 5),
            &StrategyId::new("breakout"),
            &Symbol::new("BRK.B"),
            IntentTag::Stop,
        );
        assert_eq!(id, "20261105-breakout-BRK.B-stop");
    }
}
