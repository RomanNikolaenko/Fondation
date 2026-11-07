//! Єдине місце переходу з `f64` у `Decimal` (крок ціни 0.01).

use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum PriceError {
    #[error("price is not a finite number")]
    NotFinite,
    #[error("price must be positive after rounding to a cent")]
    NotPositive,
}

/// Округлює до центів (середина — від нуля); NaN/inf/<=0 — помилка.
pub fn to_price(x: f64) -> Result<Decimal, PriceError> {
    if !x.is_finite() {
        return Err(PriceError::NotFinite);
    }
    let d = Decimal::from_f64(x).ok_or(PriceError::NotFinite)?;
    let r = d.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    if r <= Decimal::ZERO {
        return Err(PriceError::NotPositive);
    }
    Ok(r)
}

pub fn to_f64(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn to_price_rounds_to_cent() {
        assert_eq!(to_price(101.236).unwrap(), dec!(101.24));
        assert_eq!(to_price(99.994).unwrap(), dec!(99.99));
        assert_eq!(to_price(0.125).unwrap(), dec!(0.13));
        assert!(to_price(f64::NAN).is_err());
        assert!(to_price(f64::INFINITY).is_err());
        assert!(to_price(0.0).is_err());
        assert!(to_price(-5.0).is_err());
        assert!(to_price(0.004).is_err());
    }

    #[test]
    fn to_f64_roundtrip() {
        assert_eq!(to_f64(dec!(12.34)), 12.34);
    }
}
