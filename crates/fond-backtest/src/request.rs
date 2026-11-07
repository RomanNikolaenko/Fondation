//! Типи запиту та результату бектесту.

use std::collections::HashMap;

use chrono::NaiveDate;
use fond_core::risk::RiskConfig;
use fond_core::scanner::ScannerConfig;
use fond_core::types::{Bar, StrategyId, Symbol};
use fond_strategies::StrategiesConfig;
use serde::{Deserialize, Serialize};

use crate::metrics::MetricsReport;

/// Модель витрат виконання.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CostModel {
    pub slippage_bps: f64,
    pub commission_per_share: f64,
    pub commission_min: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            slippage_bps: 5.0,
            commission_per_share: 0.0,
            commission_min: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BacktestRequest {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub split: Option<NaiveDate>,
    pub initial_equity: f64,
    pub strategies: StrategiesConfig,
    pub risk: RiskConfig,
    pub scanner: ScannerConfig,
    pub costs: CostModel,
}

/// Історичні дані для прогону (внутрішній тип, не серіалізується).
#[derive(Debug, Clone)]
pub struct HistoricalData {
    pub bars: HashMap<Symbol, Vec<Bar>>,
    pub sectors: HashMap<Symbol, String>,
    pub earnings: Option<HashMap<Symbol, Vec<NaiveDate>>>,
    pub earnings_days_ahead: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExitReason {
    Stop,
    Target,
    Signal(String),
    EndOfTest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BtTrade {
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub entry_date: NaiveDate,
    pub entry_price: f64,
    pub exit_date: NaiveDate,
    pub exit_price: f64,
    pub qty: u32,
    pub pnl: f64,
    pub exit_reason: ExitReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub equity: f64,
    pub benchmark: f64,
    pub in_market: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BtDecision {
    pub date: NaiveDate,
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub accepted: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Limitation {
    SurvivorshipBias,
    NewsFilterNotSimulated,
    EarningsFilterNotApplied,
    WeeklyLossApproximation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BacktestResult {
    pub trades: Vec<BtTrade>,
    pub equity: Vec<EquityPoint>,
    pub decisions: Vec<BtDecision>,
    pub metrics: MetricsReport,
    pub limitations: Vec<Limitation>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BacktestError {
    #[error("market symbol is missing from the historical data")]
    MissingMarketSymbol,
    #[error("the requested date range is empty")]
    EmptyRange,
    #[error("invalid configuration: {}", .0.join("; "))]
    InvalidConfig(Vec<String>),
}
