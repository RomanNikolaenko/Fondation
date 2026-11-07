//! Конкретні стратегії і ринковий фільтр.

pub mod breakout;
pub mod config;
pub mod market_filter;
pub mod trend_pullback;

use std::collections::{BTreeMap, HashMap};

use fond_core::signal::EntrySignal;
use fond_core::traits::Strategy;
use fond_core::types::{Bar, StrategyId, Symbol};

pub use breakout::Breakout;
pub use config::{BreakoutParams, MarketFilterParams, StrategiesConfig, TrendPullbackParams};
pub use market_filter::market_filter_ok;
pub use trend_pullback::TrendPullback;

pub const TREND_PULLBACK: &str = "trend_pullback";
pub const BREAKOUT: &str = "breakout";
/// Стратегії рахують лише по останніх стількох барах переданого зрізу.
pub const STRATEGY_WINDOW: usize = 300;

/// Останні `n` барів зрізу (або весь зріз, якщо він коротший).
pub fn tail(bars: &[Bar], n: usize) -> &[Bar] {
    &bars[bars.len().saturating_sub(n)..]
}

pub(crate) fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Обидві стратегії незалежно від `enabled`: вони потрібні для виходів.
pub fn build_all(cfg: &StrategiesConfig) -> Vec<Box<dyn Strategy>> {
    vec![
        Box::new(TrendPullback::new(cfg.trend_pullback.clone())),
        Box::new(Breakout::new(cfg.breakout.clone())),
    ]
}

pub fn entries_enabled(cfg: &StrategiesConfig, id: &StrategyId) -> bool {
    match id.as_str() {
        TREND_PULLBACK => cfg.trend_pullback.enabled,
        BREAKOUT => cfg.breakout.enabled,
        _ => false,
    }
}

pub fn allocations(cfg: &StrategiesConfig) -> HashMap<StrategyId, f64> {
    HashMap::from([
        (
            StrategyId::new(TREND_PULLBACK),
            cfg.trend_pullback.allocation,
        ),
        (StrategyId::new(BREAKOUT), cfg.breakout.allocation),
    ])
}

/// Лишає для кожного тікера найсильніший сигнал (при рівності — менший `StrategyId`).
/// Результат відсортований за тікером.
pub fn pick_best_per_symbol(signals: Vec<EntrySignal>) -> Vec<EntrySignal> {
    let mut best: BTreeMap<Symbol, EntrySignal> = BTreeMap::new();
    for s in signals {
        match best.get(&s.symbol) {
            Some(cur)
                if cur.strength > s.strength
                    || (cur.strength == s.strength && cur.strategy <= s.strategy) => {}
            _ => {
                best.insert(s.symbol.clone(), s);
            }
        }
    }
    best.into_values().collect()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use fond_core::indicators::lowest_low;
    use fond_core::portfolio::Position;
    use fond_core::signal::{ExitDecision, MarketContext};
    use fond_core::testkit::{bars_from_closes, breakout_series, uptrend_then_pullback};

    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn ctx(ok: bool) -> MarketContext {
        MarketContext {
            date: d(2026, 1, 2),
            market_filter_ok: ok,
        }
    }

    fn pos(strategy: &str, entry_date: NaiveDate, stop: f64) -> Position {
        Position {
            symbol: Symbol::new("AAPL"),
            strategy: StrategyId::new(strategy),
            qty: 10,
            entry_price: 100.0,
            entry_date,
            stop_price: stop,
            take_profit: None,
            sector: "Tech".into(),
        }
    }

    fn sig(sym: &str, strat: &str, strength: f64) -> EntrySignal {
        EntrySignal {
            strategy: StrategyId::new(strat),
            symbol: Symbol::new(sym),
            signal_date: d(2026, 1, 2),
            reference_price: 100.0,
            stop_price: 95.0,
            take_profit: None,
            strength,
            explanation: String::new(),
        }
    }

    fn closes_of(bars: &[Bar]) -> Vec<f64> {
        bars.iter().map(|b| b.close).collect()
    }

    #[test]
    fn trend_pullback_signals_after_pullback() {
        let bars = uptrend_then_pullback(d(2025, 1, 2), 260, 3);
        let s = TrendPullback::new(TrendPullbackParams::default());
        let sig = s
            .evaluate_entry(&ctx(true), &Symbol::new("AAPL"), &bars)
            .expect("signal");
        assert!(sig.stop_price < sig.reference_price && sig.strength > 0.0 && sig.strength <= 1.0);
        assert_eq!(sig.strategy.as_str(), TREND_PULLBACK);
        assert!(sig.take_profit.is_none());
        assert!(sig.explanation.contains("RSI(2)"));
    }

    #[test]
    fn trend_pullback_silent_in_pure_uptrend() {
        let bars = uptrend_then_pullback(d(2025, 1, 2), 260, 0);
        let s = TrendPullback::new(TrendPullbackParams::default());
        assert!(
            s.evaluate_entry(&ctx(true), &Symbol::new("AAPL"), &bars)
                .is_none()
        );
    }

    #[test]
    fn no_entries_when_market_filter_off() {
        let tp = TrendPullback::new(TrendPullbackParams::default());
        let bo = Breakout::new(BreakoutParams::default());
        let a = uptrend_then_pullback(d(2025, 1, 2), 260, 3);
        let b = breakout_series(d(2025, 1, 2), 260);
        assert!(
            tp.evaluate_entry(&ctx(false), &Symbol::new("A"), &a)
                .is_none()
        );
        assert!(
            bo.evaluate_entry(&ctx(false), &Symbol::new("B"), &b)
                .is_none()
        );
    }

    #[test]
    fn trend_pullback_exits_above_sma5() {
        let base = uptrend_then_pullback(d(2025, 1, 2), 260, 3);
        let entry_date = base.last().unwrap().date;
        let mut closes = closes_of(&base);
        for _ in 0..3 {
            let c = *closes.last().unwrap() * 1.03;
            closes.push(c);
        }
        let bars = bars_from_closes(d(2025, 1, 2), &closes, 1_000_000.0);
        let s = TrendPullback::new(TrendPullbackParams::default());
        match s.evaluate_exit(&ctx(true), &pos(TREND_PULLBACK, entry_date, 200.0), &bars) {
            ExitDecision::Exit { reason } => assert!(reason.contains("SMA5"), "{reason}"),
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn trend_pullback_time_stop_after_10_bars() {
        let base = uptrend_then_pullback(d(2025, 1, 2), 260, 3);
        let entry_date = base.last().unwrap().date;
        let mut closes = closes_of(&base);
        let s = TrendPullback::new(TrendPullbackParams::default());
        let p = pos(TREND_PULLBACK, entry_date, 200.0);
        for i in 1..=10 {
            let c = *closes.last().unwrap() * 0.995;
            closes.push(c);
            let bars = bars_from_closes(d(2025, 1, 2), &closes, 1_000_000.0);
            let dec = s.evaluate_exit(&ctx(true), &p, &bars);
            if i < 10 {
                assert_eq!(dec, ExitDecision::Hold, "bar {i}");
            } else {
                match dec {
                    ExitDecision::Exit { reason } => {
                        assert!(reason.contains("за часом"), "{reason}");
                    }
                    other => panic!("expected exit, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn breakout_signals_on_volume_breakout() {
        let bars = breakout_series(d(2025, 1, 2), 260);
        let sig = Breakout::new(Default::default())
            .evaluate_entry(&ctx(true), &Symbol::new("XYZ"), &bars)
            .expect("signal");
        assert_eq!(sig.strategy.as_str(), BREAKOUT);
        assert!(sig.stop_price < sig.reference_price);
        assert!(sig.strength > 0.0 && sig.strength <= 1.0);
    }

    #[test]
    fn breakout_requires_volume() {
        let mut bars = breakout_series(d(2025, 1, 2), 260);
        bars.last_mut().unwrap().volume = 1_200_000.0;
        assert!(
            Breakout::new(Default::default())
                .evaluate_entry(&ctx(true), &Symbol::new("XYZ"), &bars)
                .is_none()
        );
    }

    #[test]
    fn breakout_moves_stop_up_only() {
        let bars = breakout_series(d(2025, 1, 2), 260);
        let entry = bars.last().unwrap().date;
        let ll = lowest_low(&bars, 20).unwrap();
        let s = Breakout::new(Default::default());
        match s.evaluate_exit(&ctx(true), &pos(BREAKOUT, entry, ll - 5.0), &bars) {
            ExitDecision::MoveStop { new_stop, .. } => assert_eq!(new_stop, ll),
            other => panic!("expected MoveStop, got {other:?}"),
        }
        assert_eq!(
            s.evaluate_exit(&ctx(true), &pos(BREAKOUT, entry, ll + 1.0), &bars),
            ExitDecision::Hold
        );
        assert_eq!(
            s.evaluate_exit(&ctx(true), &pos(BREAKOUT, entry, ll), &bars),
            ExitDecision::Hold
        );
    }

    #[test]
    fn result_independent_of_extra_history() {
        let sym = Symbol::new("AAPL");
        let tp = TrendPullback::new(TrendPullbackParams::default());
        let full = uptrend_then_pullback(d(2024, 1, 2), 500, 3);
        let a = tp.evaluate_entry(&ctx(true), &sym, &full);
        assert!(a.is_some());
        assert_eq!(a, tp.evaluate_entry(&ctx(true), &sym, tail(&full, 300)));

        let bo = Breakout::new(BreakoutParams::default());
        let full = breakout_series(d(2024, 1, 2), 400);
        let a = bo.evaluate_entry(&ctx(true), &sym, &full);
        assert!(a.is_some());
        assert_eq!(a, bo.evaluate_entry(&ctx(true), &sym, tail(&full, 300)));
    }

    #[test]
    fn pick_best_per_symbol_keeps_strongest() {
        let out = pick_best_per_symbol(vec![
            sig("MSFT", TREND_PULLBACK, 0.5),
            sig("AAPL", TREND_PULLBACK, 0.4),
            sig("AAPL", BREAKOUT, 0.7),
            sig("MSFT", BREAKOUT, 0.5),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].symbol.as_str(), "AAPL");
        assert_eq!(out[0].strategy.as_str(), BREAKOUT);
        assert_eq!(out[1].strategy.as_str(), BREAKOUT); // tie: smaller id
    }

    #[test]
    fn validate_rejects_allocation_sum_over_one() {
        let mut cfg = StrategiesConfig::default();
        assert!(cfg.validate().is_ok());
        cfg.trend_pullback.allocation = 0.6;
        cfg.breakout.allocation = 0.6;
        assert!(cfg.validate().is_err());
        cfg.breakout.allocation = 0.4;
        cfg.trend_pullback.sma_long = 0;
        assert_eq!(cfg.validate().unwrap_err().len(), 1);
    }

    #[test]
    fn build_all_and_helpers() {
        let mut cfg = StrategiesConfig::default();
        cfg.breakout.enabled = false;
        assert_eq!(build_all(&cfg).len(), 2);
        assert!(entries_enabled(&cfg, &StrategyId::new(TREND_PULLBACK)));
        assert!(!entries_enabled(&cfg, &StrategyId::new(BREAKOUT)));
        assert_eq!(allocations(&cfg).len(), 2);
        assert_eq!(build_all(&cfg)[0].warmup_bars(), STRATEGY_WINDOW);
    }
}
