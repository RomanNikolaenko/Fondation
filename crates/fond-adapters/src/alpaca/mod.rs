//! Клієнт Alpaca (paper): торгівля через `Broker`.

mod data;
mod models;
mod trading;

pub use trading::{AlpacaClient, AlpacaConfig};
