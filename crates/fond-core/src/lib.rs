//! Доменні типи, трейти, індикатори, ризик-правила, розмір позиції. Без I/O.

pub mod error;
pub mod ids;
pub mod indicators;
pub mod intent;
pub mod orders;
pub mod portfolio;
pub mod price;
pub mod signal;
pub mod traits;
pub mod types;

#[cfg(feature = "testkit")]
pub mod testkit;
