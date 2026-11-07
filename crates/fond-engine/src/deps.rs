//! Залежності кроків циклу: порти, сховище, годинник, шина подій.

use std::sync::Arc;

use async_trait::async_trait;
use fond_core::error::DataError;
use fond_core::traits::{Broker, Clock, EarningsCalendar, MarketData, NewsJudge, NewsSource};
use fond_core::types::Symbol;
use fond_store::Store;

use crate::events::EventBus;
use crate::health::BrokerHealth;

/// Галузь компанії (сектор для тікерів, доданих вручну).
#[async_trait]
pub trait IndustryLookup: Send + Sync {
    async fn industry(&self, s: &Symbol) -> Result<Option<String>, DataError>;
}

#[derive(Clone)]
pub struct Deps {
    pub broker: Arc<dyn Broker>,
    pub data: Arc<dyn MarketData>,
    pub news: Arc<dyn NewsSource>,
    pub earnings: Arc<dyn EarningsCalendar>,
    pub judge: Arc<dyn NewsJudge>,
    pub industry: Arc<dyn IndustryLookup>,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub bus: EventBus,
    pub health: Arc<BrokerHealth>,
}
