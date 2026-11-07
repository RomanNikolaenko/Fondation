//! Спільні тестові дані: синтетичний універсум, запит, обрізання історії, округлення метрик.
#![allow(dead_code)]

use std::collections::HashMap;

use chrono::NaiveDate;
use fond_backtest::{BacktestRequest, CostModel, HistoricalData, MetricsReport};
use fond_core::risk::RiskConfig;
use fond_core::scanner::ScannerConfig;
use fond_core::testkit::synthetic_bars;
use fond_core::types::{Bar, Symbol};
use fond_strategies::StrategiesConfig;
use serde_json::Value;

pub const MARKET: &str = "SPY";
/// Розігрів ~1.5 року перед `from()`: SMA200 і 250 барів історії сканера.
pub const WARMUP_BARS: usize = 378;
pub const BARS_PER_YEAR: usize = 252;
const SECTORS: [&str; 5] = ["Tech", "Health", "Energy", "Financials", "Industrials"];

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn sym(s: &str) -> Symbol {
    Symbol::new(s)
}

/// Перший бар синтетичних даних.
pub fn data_start() -> NaiveDate {
    d(2019, 7, 1)
}

/// Початок прогону для синтетичних даних (після розігріву).
pub fn from() -> NaiveDate {
    d(2021, 1, 4)
}

/// Детерміновані обсяги 0.5M..2M, щоб пробої з підвищеним обсягом теж траплялися.
fn vary_volume(bars: &mut [Bar], seed: u64) {
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    for b in bars {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let u = (state >> 11) as f64 / (1u64 << 53) as f64;
        b.volume = 1_000_000.0 * (0.5 + 1.5 * u);
    }
}

/// SPY + `n_symbols` тікерів `S01..`: розігрів `WARMUP_BARS` і далі `years` років барів.
pub fn synthetic_universe(n_symbols: usize, years: usize) -> HistoricalData {
    let n = WARMUP_BARS + years * BARS_PER_YEAR;
    let start = data_start();
    let mut bars = HashMap::new();
    let mut sectors = HashMap::new();
    bars.insert(
        sym(MARKET),
        synthetic_bars(42, start, n, 300.0, 0.0006, 0.016),
    );
    for i in 0..n_symbols {
        let seed = 1_000 + i as u64;
        let price = 40.0 + 9.0 * i as f64;
        let drift = 0.0004 + 0.0001 * (i % 4) as f64;
        let vol = 0.026 + 0.003 * (i % 3) as f64;
        let mut b = synthetic_bars(seed, start, n, price, drift, vol);
        vary_volume(&mut b, seed);
        let s = sym(&format!("S{:02}", i + 1));
        sectors.insert(s.clone(), SECTORS[i % SECTORS.len()].to_string());
        bars.insert(s, b);
    }
    HistoricalData {
        bars,
        sectors,
        earnings: None,
        earnings_days_ahead: 3,
    }
}

pub fn req(from: NaiveDate, to: NaiveDate) -> BacktestRequest {
    BacktestRequest {
        from,
        to,
        split: None,
        initial_equity: 100_000.0,
        strategies: StrategiesConfig::default(),
        risk: RiskConfig::default(),
        scanner: ScannerConfig::default(),
        costs: CostModel::default(),
    }
}

/// Дані, якими володів би прогін, що закінчується в `t`: лише бари з датою `<= t`.
pub fn truncate(data: &HistoricalData, t: NaiveDate) -> HistoricalData {
    let bars = data
        .bars
        .iter()
        .map(|(s, b)| {
            let kept: Vec<Bar> = b.iter().filter(|x| x.date <= t).cloned().collect();
            (s.clone(), kept)
        })
        .collect();
    HistoricalData {
        bars,
        sectors: data.sectors.clone(),
        earnings: data.earnings.clone(),
        earnings_days_ahead: data.earnings_days_ahead,
    }
}

/// Остання сесія SPY у даних.
pub fn last_session(data: &HistoricalData) -> NaiveDate {
    data.bars[&sym(MARKET)].last().unwrap().date
}

/// Середня сесія SPY у діапазоні `[from(), остання сесія]`.
pub fn midpoint_session(data: &HistoricalData) -> NaiveDate {
    let dates: Vec<NaiveDate> = data.bars[&sym(MARKET)]
        .iter()
        .map(|b| b.date)
        .filter(|x| *x >= from())
        .collect();
    dates[dates.len() / 2]
}

fn round_value(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            let x = n.as_f64().unwrap_or(0.0);
            let r = (x * 1e6).round() / 1e6;
            let r = if r == 0.0 { 0.0 } else { r };
            *v = serde_json::Number::from_f64(r).map_or(Value::Null, Value::Number);
        }
        Value::Array(items) => items.iter_mut().for_each(round_value),
        Value::Object(map) => map.values_mut().for_each(round_value),
        _ => {}
    }
}

/// Метрики як JSON з усіма `f64`, округленими до 6 знаків.
pub fn round6(m: &MetricsReport) -> Value {
    let mut v = serde_json::to_value(m).unwrap();
    round_value(&mut v);
    v
}
