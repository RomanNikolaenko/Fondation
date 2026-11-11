//! Доменні типи, трейти, індикатори, ризик-правила, розмір позиції. Без I/O.

pub mod ai;
pub mod calendar;
pub mod error;
pub mod filters;
pub mod ids;
pub mod indicators;
pub mod intent;
pub mod keys;
pub mod orders;
pub mod portfolio;
pub mod price;
pub mod rebalance;
pub mod risk;
pub mod scanner;
pub mod signal;
pub mod traits;
pub mod types;

#[cfg(feature = "testkit")]
pub mod testkit;
