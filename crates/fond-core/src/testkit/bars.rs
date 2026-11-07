//! Детерміновані генератори денних барів.

use chrono::{Datelike, Duration, NaiveDate};

use crate::types::Bar;

/// `n` послідовних будніх днів, починаючи зі `start` (або з найближчого будня).
pub fn weekdays_from(start: NaiveDate, n: usize) -> Vec<NaiveDate> {
    let mut out = Vec::with_capacity(n);
    let mut d = start;
    while out.len() < n {
        if d.weekday().num_days_from_monday() < 5 {
            out.push(d);
        }
        d += Duration::days(1);
    }
    out
}

/// open = попереднє close (для першого — його close), high/low ±0.5%.
pub fn bars_from_closes(start: NaiveDate, closes: &[f64], volume: f64) -> Vec<Bar> {
    let dates = weekdays_from(start, closes.len());
    let mut prev = closes.first().copied().unwrap_or(0.0);
    dates
        .into_iter()
        .zip(closes)
        .map(|(date, &close)| {
            let open = prev;
            prev = close;
            Bar {
                date,
                open,
                high: open.max(close) * 1.005,
                low: open.min(close) * 0.995,
                close,
                volume,
            }
        })
        .collect()
}

/// `n_up` барів росту (перший = 100, далі ×1.004), потім `n_down` барів ×0.985.
pub fn uptrend_then_pullback(start: NaiveDate, n_up: usize, n_down: usize) -> Vec<Bar> {
    let mut closes = Vec::with_capacity(n_up + n_down);
    let mut c = 100.0;
    for i in 0..n_up {
        if i > 0 {
            c *= 1.004;
        }
        closes.push(c);
    }
    for _ in 0..n_down {
        c *= 0.985;
        closes.push(c);
    }
    bars_from_closes(start, &closes, 1_000_000.0)
}

/// `n_flat` днів боковика `100 + 2 sin(i)`, потім проби вгору на подвоєному обсязі.
pub fn breakout_series(start: NaiveDate, n_flat: usize) -> Vec<Bar> {
    let mut closes: Vec<f64> = (0..n_flat)
        .map(|i| 100.0 + 2.0 * (i as f64).sin())
        .collect();
    closes.push(106.0);
    let mut bars = bars_from_closes(start, &closes, 1_000_000.0);
    if let Some(last) = bars.last_mut() {
        last.volume = 2_000_000.0;
    }
    bars
}

/// Детерміноване випадкове блукання на LCG.
pub fn synthetic_bars(
    seed: u64,
    start: NaiveDate,
    n: usize,
    start_price: f64,
    drift: f64,
    vol: f64,
) -> Vec<Bar> {
    const A: u64 = 6364136223846793005;
    const C: u64 = 1442695040888963407;
    let mut state = seed;
    let mut uniform = || {
        state = state.wrapping_mul(A).wrapping_add(C);
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut closes = Vec::with_capacity(n);
    let mut price = start_price;
    for i in 0..n {
        if i > 0 {
            let z: f64 = (0..4).map(|_| uniform()).sum::<f64>() - 2.0;
            price *= 1.0 + drift + vol * z;
            price = price.max(0.01);
        }
        closes.push(price);
    }
    bars_from_closes(start, &closes, 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn uptrend_then_pullback_shape() {
        let b = uptrend_then_pullback(d(2025, 1, 2), 260, 3);
        assert_eq!(b.len(), 263);
        assert!(b.windows(2).all(|w| w[0].date < w[1].date));
        assert!(
            b.iter()
                .all(|x| x.date.weekday().num_days_from_monday() < 5)
        );
        assert!(b[262].close < b[259].close && b[259].close > b[0].close);
        assert_eq!(b[0].close, 100.0);
    }

    #[test]
    fn weekdays_skip_weekends() {
        let w = weekdays_from(d(2025, 1, 3), 3); // Friday
        assert_eq!(w, vec![d(2025, 1, 3), d(2025, 1, 6), d(2025, 1, 7)]);
    }

    #[test]
    fn bars_from_closes_ohlc_rules() {
        let b = bars_from_closes(d(2025, 1, 2), &[100.0, 110.0], 5.0);
        assert_eq!(b[0].open, 100.0);
        assert_eq!(b[1].open, 100.0);
        assert!((b[1].high - 110.0 * 1.005).abs() < 1e-9);
        assert!((b[1].low - 100.0 * 0.995).abs() < 1e-9);
        assert_eq!(b[1].volume, 5.0);
    }

    #[test]
    fn breakout_series_last_bar() {
        let b = breakout_series(d(2025, 1, 2), 30);
        assert_eq!(b.len(), 31);
        assert_eq!(b[30].close, 106.0);
        assert_eq!(b[30].volume, 2_000_000.0);
        assert_eq!(b[29].volume, 1_000_000.0);
    }

    #[test]
    fn synthetic_bars_are_deterministic() {
        let a = synthetic_bars(7, d(2025, 1, 2), 50, 100.0, 0.0005, 0.01);
        let b = synthetic_bars(7, d(2025, 1, 2), 50, 100.0, 0.0005, 0.01);
        let c = synthetic_bars(8, d(2025, 1, 2), 50, 100.0, 0.0005, 0.01);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a[0].close, 100.0);
        assert!(a.iter().all(|x| x.close > 0.0));
    }
}
