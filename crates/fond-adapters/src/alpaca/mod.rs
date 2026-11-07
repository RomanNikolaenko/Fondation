//! Клієнт Alpaca (paper): торгівля через `Broker`.

mod models;
mod trading;

pub use trading::{AlpacaClient, AlpacaConfig};
