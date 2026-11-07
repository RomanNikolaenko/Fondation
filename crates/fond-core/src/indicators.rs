//! Технічні індикатори: чисті функції без I/O. `None`, коли даних замало.

use crate::types::Bar;

#[allow(clippy::cast_precision_loss)]
fn mean_last(values: impl ExactSizeIterator<Item = f64>, period: usize) -> Option<f64> {
    let values_len = values.len();
    if period == 0 || values_len < period {
        return None;
    }
    let sum: f64 = values.skip(values_len - period).sum();
    Some(sum / period as f64)
}

/// Середнє останніх `period` значень.
pub fn sma_last(values: &[f64], period: usize) -> Option<f64> {
    mean_last(values.iter().copied(), period)
}

/// RSI за Вайлдером; потрібно щонайменше `period + 1` значень.
#[allow(clippy::cast_precision_loss)]
pub fn rsi_wilder_last(closes: &[f64], period: usize) -> Option<f64> {
    if period == 0 || closes.len() < period + 1 {
        return None;
    }
    let p = period as f64;
    let mut changes = closes.windows(2).map(|w| w[1] - w[0]);
    let (mut gain, mut loss) = (0.0, 0.0);
    for d in changes.by_ref().take(period) {
        gain += d.max(0.0);
        loss += (-d).max(0.0);
    }
    gain /= p;
    loss /= p;
    for d in changes {
        gain = (gain * (p - 1.0) + d.max(0.0)) / p;
        loss = (loss * (p - 1.0) + (-d).max(0.0)) / p;
    }
    Some(if loss == 0.0 {
        if gain == 0.0 { 50.0 } else { 100.0 }
    } else {
        100.0 - 100.0 / (1.0 + gain / loss)
    })
}

/// ATR за Вайлдером; потрібно щонайменше `period + 1` барів.
#[allow(clippy::cast_precision_loss)]
pub fn atr_wilder_last(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period + 1 {
        return None;
    }
    let p = period as f64;
    let mut trs = bars.windows(2).map(|w| {
        let prev_c = w[0].close;
        let b = &w[1];
        (b.high - b.low)
            .max((b.high - prev_c).abs())
            .max((b.low - prev_c).abs())
    });
    let seed: f64 = trs.by_ref().take(period).sum::<f64>() / p;
    Some(trs.fold(seed, |atr, tr| (atr * (p - 1.0) + tr) / p))
}

/// Максимум `high` по останніх `period` барах.
pub fn highest_high(bars: &[Bar], period: usize) -> Option<f64> {
    last_n(bars, period)?
        .iter()
        .map(|b| b.high)
        .reduce(f64::max)
}

/// Мінімум `low` по останніх `period` барах.
pub fn lowest_low(bars: &[Bar], period: usize) -> Option<f64> {
    last_n(bars, period)?.iter().map(|b| b.low).reduce(f64::min)
}

/// Середній обсяг за останні `period` барів.
pub fn avg_volume(bars: &[Bar], period: usize) -> Option<f64> {
    mean_last(bars.iter().map(|b| b.volume), period)
}

/// Середнє `close * volume` за останні `period` барів.
pub fn avg_dollar_volume(bars: &[Bar], period: usize) -> Option<f64> {
    mean_last(bars.iter().map(|b| b.close * b.volume), period)
}

fn last_n(bars: &[Bar], period: usize) -> Option<&[Bar]> {
    if period == 0 || bars.len() < period {
        return None;
    }
    Some(&bars[bars.len() - period..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn bar(h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar {
            date: NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            open: c,
            high: h,
            low: l,
            close: c,
            volume: v,
        }
    }

    fn bars_hlc(t: &[(f64, f64, f64)]) -> Vec<Bar> {
        t.iter().map(|&(h, l, c)| bar(h, l, c, 1000.0)).collect()
    }

    #[test]
    fn sma_basic() {
        assert_eq!(sma_last(&[1., 2., 3., 4., 5.], 3), Some(4.0));
        assert_eq!(sma_last(&[1., 2.], 3), None);
        assert_eq!(sma_last(&[1., 2.], 0), None);
    }

    #[test]
    fn rsi_reference() {
        assert!((rsi_wilder_last(&[1., 2., 3., 2., 1.], 2).unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(rsi_wilder_last(&[1., 2.], 2), None);
        assert_eq!(rsi_wilder_last(&[1., 2., 3.], 0), None);
    }

    #[test]
    fn rsi_flat_is_50_and_only_gains_is_100() {
        assert_eq!(rsi_wilder_last(&[5.; 6], 2), Some(50.0));
        assert_eq!(rsi_wilder_last(&[1., 2., 3., 4.], 2), Some(100.0));
    }

    #[test]
    fn atr_reference() {
        // TR 2,3,1 -> ATR(2): seed 2.5, then 1.75
        let b = bars_hlc(&[
            (10., 8., 9.),
            (11., 9., 10.),
            (12., 9., 11.),
            (11., 10., 10.),
        ]);
        assert!((atr_wilder_last(&b, 2).unwrap() - 1.75).abs() < 1e-9);
        assert_eq!(atr_wilder_last(&b[..2], 2), None);
        assert_eq!(atr_wilder_last(&b, 0), None);
    }

    #[test]
    fn extremes_and_volume() {
        let b = vec![
            bar(13., 5., 9., 100.),
            bar(12., 9., 11., 200.),
            bar(11., 7., 10., 300.),
        ];
        assert_eq!(highest_high(&b, 3), Some(13.));
        assert_eq!(highest_high(&b, 2), Some(12.));
        assert_eq!(lowest_low(&b, 3), Some(5.));
        assert_eq!(lowest_low(&b, 2), Some(7.));
        assert_eq!(avg_volume(&b, 3), Some(200.));
        assert_eq!(avg_volume(&b, 2), Some(250.));
        // close*volume: 900, 2200, 3000
        assert!((avg_dollar_volume(&b, 3).unwrap() - 6100. / 3.).abs() < 1e-9);
        assert_eq!(avg_dollar_volume(&b, 2), Some(2600.));
        assert_eq!(highest_high(&b, 4), None);
        assert_eq!(lowest_low(&b, 4), None);
        assert_eq!(avg_volume(&b, 4), None);
        assert_eq!(avg_dollar_volume(&b, 4), None);
        assert_eq!(highest_high(&b, 0), None);
    }
}
