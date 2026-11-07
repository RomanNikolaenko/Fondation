//! Симулятор брокера, прогін історії, метрики.

pub mod metrics;
pub mod request;

pub use metrics::{Metrics, MetricsReport, compute_metrics, report};
pub use request::{
    BacktestError, BacktestRequest, BacktestResult, BtDecision, BtTrade, CostModel, EquityPoint,
    ExitReason, HistoricalData, Limitation,
};
