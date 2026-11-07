//! Розмір позиції.

use rust_decimal::Decimal;

use crate::price::{to_f64, to_price};

use super::RiskConfig;

/// Розраховані ціни та кількість.
pub(crate) struct Sized {
    pub qty: u32,
    pub limit: Decimal,
    pub stop: Decimal,
}

pub(crate) enum SizeError {
    InvalidStop,
    QtyBelowOne,
}

fn floor_qty(x: f64) -> f64 {
    if x.is_finite() {
        x.floor().max(0.0)
    } else {
        0.0
    }
}

pub(crate) fn size_position(
    reference: f64,
    stop_price: f64,
    equity: f64,
    cash: f64,
    cfg: &RiskConfig,
) -> Result<Sized, SizeError> {
    let limit = to_price(reference * (1.0 + cfg.max_gap)).map_err(|_| SizeError::InvalidStop)?;
    let stop = to_price(stop_price).map_err(|_| SizeError::InvalidStop)?;
    if limit <= stop {
        return Err(SizeError::InvalidStop);
    }
    let limit_f = to_f64(limit);
    let per_share = to_f64(limit - stop);
    let by_risk = floor_qty(equity * cfg.risk_per_trade / per_share);
    let by_pos = floor_qty(equity * cfg.max_position_pct / limit_f);
    let by_cash = floor_qty(cash / limit_f);
    let qty = by_risk.min(by_pos).min(by_cash).min(f64::from(u32::MAX));
    if qty < 1.0 {
        return Err(SizeError::QtyBelowOne);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let qty = qty as u32;
    Ok(Sized { qty, limit, stop })
}
