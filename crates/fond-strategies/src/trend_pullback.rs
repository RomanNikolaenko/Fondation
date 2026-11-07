//! Трендовий відкат: купівля короткого відкату у висхідному тренді.

use fond_core::indicators::{atr_wilder_last, rsi_wilder_last, sma_last};
use fond_core::portfolio::Position;
use fond_core::signal::{EntrySignal, ExitDecision, MarketContext};
use fond_core::traits::Strategy;
use fond_core::types::{Bar, StrategyId, Symbol};

use crate::config::TrendPullbackParams;
use crate::{STRATEGY_WINDOW, TREND_PULLBACK, clamp01, tail};

pub struct TrendPullback {
    params: TrendPullbackParams,
}

impl TrendPullback {
    pub fn new(params: TrendPullbackParams) -> Self {
        Self { params }
    }
}

impl Strategy for TrendPullback {
    fn id(&self) -> StrategyId {
        StrategyId::new(TREND_PULLBACK)
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
        let last = w.last()?;
        let closes: Vec<f64> = w.iter().map(|b| b.close).collect();
        let sma_long = sma_last(&closes, p.sma_long)?;
        let sma_mid = sma_last(&closes, p.sma_mid)?;
        let rsi = rsi_wilder_last(&closes, p.rsi_period)?;
        let atr = atr_wilder_last(w, p.atr_period)?;
        let close = last.close;

        if !(close > sma_long && sma_mid > sma_long && rsi < p.rsi_entry) {
            return None;
        }

        let stop = close - p.atr_stop_mult * atr;
        let strength = 0.7 * clamp01((p.rsi_entry - rsi) / p.rsi_entry)
            + 0.3 * clamp01((sma_mid / sma_long - 1.0) / 0.10);
        let explanation = format!(
            "Висхідний тренд: ціна вище SMA{}, SMA{} вище SMA{}. RSI({}) = {:.2} < {:.2} — \
             короткий відкат. Стоп {}×ATR({}): {:.2}.",
            p.sma_long,
            p.sma_mid,
            p.sma_long,
            p.rsi_period,
            rsi,
            p.rsi_entry,
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
        let p = &self.params;
        let w = tail(bars, STRATEGY_WINDOW);
        let Some(last) = w.last() else {
            return ExitDecision::Hold;
        };
        let closes: Vec<f64> = w.iter().map(|b| b.close).collect();
        let Some(sma) = sma_last(&closes, p.exit_sma) else {
            return ExitDecision::Hold;
        };
        if last.close > sma {
            return ExitDecision::Exit {
                reason: format!(
                    "Закриття вище SMA{}: {:.2} > {:.2}",
                    p.exit_sma, last.close, sma
                ),
            };
        }
        let bars_held = w.iter().filter(|b| b.date > position.entry_date).count();
        if bars_held >= p.max_hold_days {
            return ExitDecision::Exit {
                reason: format!("Вихід за часом: {} днів", p.max_hold_days),
            };
        }
        ExitDecision::Hold
    }
}
