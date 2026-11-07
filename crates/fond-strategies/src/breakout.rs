//! Пробій максимуму з підвищеним обсягом; супровід стопом по мінімуму.

use fond_core::indicators::{atr_wilder_last, avg_volume, highest_high, lowest_low};
use fond_core::portfolio::Position;
use fond_core::signal::{EntrySignal, ExitDecision, MarketContext};
use fond_core::traits::Strategy;
use fond_core::types::{Bar, StrategyId, Symbol};

use crate::config::BreakoutParams;
use crate::{BREAKOUT, STRATEGY_WINDOW, clamp01, tail};

pub struct Breakout {
    params: BreakoutParams,
}

impl Breakout {
    pub fn new(params: BreakoutParams) -> Self {
        Self { params }
    }
}

impl Strategy for Breakout {
    fn id(&self) -> StrategyId {
        StrategyId::new(BREAKOUT)
    }

    fn warmup_bars(&self) -> usize {
        STRATEGY_WINDOW
    }

    fn evaluate_entry(
        &self,
        ctx: &MarketContext,
        symbol: &Symbol,
        bars: &[Bar],
    ) -> Option<EntrySignal> {
        if !ctx.market_filter_ok {
            return None;
        }
        let p = &self.params;
        let w = tail(bars, STRATEGY_WINDOW);
        let (last, prior) = w.split_last()?;
        let prev_high = highest_high(prior, p.entry_lookback)?;
        let avg_vol = avg_volume(prior, p.volume_avg)?;
        let atr = atr_wilder_last(w, p.atr_period)?;
        let (close, volume) = (last.close, last.volume);

        if !(close > prev_high && volume > p.volume_mult * avg_vol) {
            return None;
        }

        let stop = close - p.atr_stop_mult * atr;
        let vol_ratio = volume / avg_vol;
        let strength = 0.5 * clamp01((vol_ratio - p.volume_mult) / 1.5)
            + 0.5
                * if atr > 0.0 {
                    clamp01((close - prev_high) / atr)
                } else {
                    1.0
                };
        let explanation = format!(
            "Пробій {}-денного максимуму: закриття {:.2} вище {:.2}. Обсяг {:.2}× від \
             середнього за {} днів (потрібно > {:.2}×). Стоп {}×ATR({}): {:.2}.",
            p.entry_lookback,
            close,
            prev_high,
            vol_ratio,
            p.volume_avg,
            p.volume_mult,
            p.atr_stop_mult,
            p.atr_period,
            stop
        );
        Some(EntrySignal {
            strategy: self.id(),
            symbol: symbol.clone(),
            signal_date: last.date,
            reference_price: close,
            stop_price: stop,
            take_profit: None,
            strength,
            explanation,
        })
    }

    fn evaluate_exit(
        &self,
        _ctx: &MarketContext,
        position: &Position,
        bars: &[Bar],
    ) -> ExitDecision {
        let w = tail(bars, STRATEGY_WINDOW);
        let Some(trail) = lowest_low(w, self.params.trail_lookback) else {
            return ExitDecision::Hold;
        };
        if trail >= position.stop_price + 0.01 - 1e-9 {
            ExitDecision::MoveStop {
                new_stop: trail,
                reason: format!(
                    "Підтягування стопа до {}-денного мінімуму: {:.2}",
                    self.params.trail_lookback, trail
                ),
            }
        } else {
            ExitDecision::Hold
        }
    }
}
