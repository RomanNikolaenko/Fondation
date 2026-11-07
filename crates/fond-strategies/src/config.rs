//! Параметри стратегій; у конфігу можна задавати лише частину полів.

use fond_core::types::Symbol;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TrendPullbackParams {
    pub enabled: bool,
    pub allocation: f64,
    pub sma_long: usize,
    pub sma_mid: usize,
    pub rsi_period: usize,
    pub rsi_entry: f64,
    pub atr_period: usize,
    pub atr_stop_mult: f64,
    pub exit_sma: usize,
    pub max_hold_days: usize,
}

impl Default for TrendPullbackParams {
    fn default() -> Self {
        Self {
            enabled: true,
            allocation: 0.5,
            sma_long: 200,
            sma_mid: 50,
            rsi_period: 2,
            rsi_entry: 10.0,
            atr_period: 14,
            atr_stop_mult: 2.0,
            exit_sma: 5,
            max_hold_days: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BreakoutParams {
    pub enabled: bool,
    pub allocation: f64,
    pub entry_lookback: usize,
    pub volume_mult: f64,
    pub volume_avg: usize,
    pub atr_period: usize,
    pub atr_stop_mult: f64,
    pub trail_lookback: usize,
}

impl Default for BreakoutParams {
    fn default() -> Self {
        Self {
            enabled: true,
            allocation: 0.5,
            entry_lookback: 55,
            volume_mult: 1.5,
            volume_avg: 20,
            atr_period: 14,
            atr_stop_mult: 2.0,
            trail_lookback: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MarketFilterParams {
    pub symbol: Symbol,
    pub sma: usize,
}

impl Default for MarketFilterParams {
    fn default() -> Self {
        Self {
            symbol: Symbol::new("SPY"),
            sma: 200,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StrategiesConfig {
    pub trend_pullback: TrendPullbackParams,
    pub breakout: BreakoutParams,
    pub market_filter: MarketFilterParams,
}

impl StrategiesConfig {
    /// Повертає по одному повідомленню на кожне порушення.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let tp = &self.trend_pullback;
        let bo = &self.breakout;
        let mut errs = Vec::new();

        for (name, a) in [
            ("trend_pullback", tp.allocation),
            ("breakout", bo.allocation),
        ] {
            if !(0.0..=1.0).contains(&a) {
                errs.push(format!("{name}.allocation має бути в [0, 1], зараз {a}"));
            }
        }
        let sum = tp.allocation + bo.allocation;
        if sum > 1.0 + 1e-9 {
            errs.push(format!(
                "сума allocation не може перевищувати 1, зараз {sum:.2}"
            ));
        }

        let periods = [
            ("trend_pullback.sma_long", tp.sma_long),
            ("trend_pullback.sma_mid", tp.sma_mid),
            ("trend_pullback.rsi_period", tp.rsi_period),
            ("trend_pullback.atr_period", tp.atr_period),
            ("trend_pullback.exit_sma", tp.exit_sma),
            ("trend_pullback.max_hold_days", tp.max_hold_days),
            ("breakout.entry_lookback", bo.entry_lookback),
            ("breakout.volume_avg", bo.volume_avg),
            ("breakout.atr_period", bo.atr_period),
            ("breakout.trail_lookback", bo.trail_lookback),
            ("market_filter.sma", self.market_filter.sma),
        ];
        for (name, v) in periods {
            if v == 0 {
                errs.push(format!("{name} має бути більше 0"));
            }
        }

        let mults = [
            ("trend_pullback.atr_stop_mult", tp.atr_stop_mult),
            ("breakout.atr_stop_mult", bo.atr_stop_mult),
            ("breakout.volume_mult", bo.volume_mult),
        ];
        for (name, v) in mults {
            if v.is_nan() || v <= 0.0 {
                errs.push(format!("{name} має бути більше 0, зараз {v}"));
            }
        }

        if !(tp.rsi_entry > 0.0 && tp.rsi_entry < 100.0) {
            errs.push(format!(
                "trend_pullback.rsi_entry має бути в (0, 100), зараз {}",
                tp.rsi_entry
            ));
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}
