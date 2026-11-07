//! Симулятор брокера, прогін історії, метрики.

pub mod metrics;
pub mod request;
pub mod runner;
pub mod sim;

pub use metrics::{Metrics, MetricsReport, compute_metrics, report};
pub use request::{
    BacktestError, BacktestRequest, BacktestResult, BtDecision, BtTrade, CostModel, EquityPoint,
    ExitReason, HistoricalData, Limitation,
};
pub use runner::run_backtest;
