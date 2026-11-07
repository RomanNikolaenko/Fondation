//! Базові типи: тікер, ідентифікатор стратегії, бар, режими.

use std::fmt;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Symbol(String);

impl Symbol {
    /// Тікер завжди у верхньому регістрі.
    pub fn new(s: &str) -> Self {
        Self(s.trim().to_uppercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StrategyId(String);

impl StrategyId {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Денний бар.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Bar {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AccountMode {
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExecutionMode {
    Approval,
    Auto,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_is_uppercased() {
        assert_eq!(Symbol::new("brk.b").as_str(), "BRK.B");
        assert_eq!(Symbol::new("aapl").to_string(), "AAPL");
    }

    #[test]
    fn newtypes_are_transparent_in_json() {
        assert_eq!(
            serde_json::to_string(&Symbol::new("msft")).unwrap(),
            "\"MSFT\""
        );
        let s: StrategyId = serde_json::from_str("\"breakout\"").unwrap();
        assert_eq!(s.as_str(), "breakout");
    }

    #[test]
    fn execution_mode_serde() {
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Approval).unwrap(),
            "\"approval\""
        );
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&AccountMode::Paper).unwrap(),
            "\"paper\""
        );
    }
}
