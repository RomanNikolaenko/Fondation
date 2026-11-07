//! Трейти портів: стратегія, брокер, ринкові дані, новини, суддя, годинник.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::{BrokerError, DataError, JudgeError};
use crate::orders::{BrokerOrder, OrderRequest};
use crate::portfolio::{Account, BrokerPosition, Position};
use crate::signal::{EntrySignal, ExitDecision, MarketContext};
use crate::types::{Bar, StrategyId, Symbol};

pub trait Strategy: Send + Sync {
    fn id(&self) -> StrategyId;
    fn warmup_bars(&self) -> usize;
    fn evaluate_entry(
        &self,
        ctx: &MarketContext,
        symbol: &Symbol,
        bars: &[Bar],
    ) -> Option<EntrySignal>;
    fn evaluate_exit(&self, ctx: &MarketContext, position: &Position, bars: &[Bar])
    -> ExitDecision;
}

#[async_trait]
pub trait Broker: Send + Sync {
    async fn account(&self) -> Result<Account, BrokerError>;
    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError>;
    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError>;
    async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError>;
    async fn order_by_client_id(
        &self,
        client_order_id: &str,
    ) -> Result<Option<BrokerOrder>, BrokerError>;
    async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError>;
    async fn cancel(&self, order_id: &str) -> Result<(), BrokerError>;
    async fn replace_stop(
        &self,
        order_id: &str,
        new_stop: Decimal,
    ) -> Result<BrokerOrder, BrokerError>;
}

/// Торгова сесія; час — America/New_York.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Session {
    pub date: NaiveDate,
    pub open: NaiveTime,
    pub close: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AssetInfo {
    pub symbol: Symbol,
    pub tradable: bool,
    pub active: bool,
}

#[async_trait]
pub trait MarketData: Send + Sync {
    async fn daily_bars(
        &self,
        symbols: &[Symbol],
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<HashMap<Symbol, Vec<Bar>>, DataError>;
    async fn calendar(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Session>, DataError>;
    async fn latest_trade(&self, symbol: &Symbol) -> Result<Option<f64>, DataError>;
    async fn assets(&self) -> Result<Vec<AssetInfo>, DataError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewsItem {
    pub headline: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub source: String,
}

#[async_trait]
pub trait NewsSource: Send + Sync {
    async fn news(
        &self,
        symbol: &Symbol,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<NewsItem>, DataError>;
}

#[async_trait]
pub trait EarningsCalendar: Send + Sync {
    async fn next_earnings(
        &self,
        symbol: &Symbol,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Option<NaiveDate>, DataError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Verdict {
    Allow,
    Caution,
    Block,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewsVerdict {
    pub verdict: Verdict,
    pub reasons: Vec<String>,
    pub key_events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewsJudgeRequest {
    pub symbol: Symbol,
    pub company: String,
    pub signal_summary: String,
    pub items: Vec<NewsItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewsJudgment {
    pub verdict: NewsVerdict,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[async_trait]
pub trait NewsJudge: Send + Sync {
    async fn judge(&self, req: &NewsJudgeRequest) -> Result<NewsJudgment, JudgeError>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn traits_are_object_safe_and_system_clock_works() {
        fn _assert(
            _: Option<Arc<dyn Broker>>,
            _: Option<Arc<dyn MarketData>>,
            _: Option<Arc<dyn NewsSource>>,
            _: Option<Arc<dyn EarningsCalendar>>,
            _: Option<Arc<dyn NewsJudge>>,
            _: Option<Arc<dyn Strategy>>,
        ) {
        }
        let c: Arc<dyn Clock> = Arc::new(SystemClock);
        assert!(c.now().timestamp() > 1_700_000_000);
    }

    #[test]
    fn verdict_serde() {
        assert_eq!(
            serde_json::to_string(&Verdict::Caution).unwrap(),
            "\"caution\""
        );
    }
}
