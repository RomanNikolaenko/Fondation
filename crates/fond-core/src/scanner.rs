//! Сканер універсуму: відсів за даними, ціною, історією та ліквідністю.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::indicators::avg_dollar_volume;
use crate::types::Bar;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScannerConfig {
    pub min_price: f64,
    pub min_avg_dollar_volume: f64,
    pub avg_window: usize,
    pub min_history_bars: usize,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            min_price: 5.0,
            min_avg_dollar_volume: 20_000_000.0,
            avg_window: 20,
            min_history_bars: 250,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ScanReject {
    NotTradable,
    TooLittleHistory,
    PriceBelowMin,
    LowLiquidity,
    BadData(String),
}

fn bad_price(x: f64) -> bool {
    !x.is_finite() || x <= 0.0
}

/// Перевіряє цілісність барів; `Err` містить опис першої проблеми.
pub fn validate_bars(bars: &[Bar], expected_last: NaiveDate) -> Result<(), String> {
    let Some(last) = bars.last() else {
        return Err("no bars".to_string());
    };
    for (i, b) in bars.iter().enumerate() {
        if i > 0 && b.date <= bars[i - 1].date {
            return Err(format!(
                "dates out of order: {} after {}",
                b.date,
                bars[i - 1].date
            ));
        }
        if bad_price(b.open) || bad_price(b.high) || bad_price(b.low) || bad_price(b.close) {
            return Err(format!("invalid price (NaN or <= 0) on {}", b.date));
        }
        if !b.volume.is_finite() || b.volume < 0.0 {
            return Err(format!("invalid volume on {}", b.date));
        }
        if b.high < b.low {
            return Err(format!("high below low on {}", b.date));
        }
    }
    if last.volume == 0.0 {
        return Err(format!("zero volume on last bar {}", last.date));
    }
    if last.date != expected_last {
        return Err(format!(
            "stale: last bar {}, expected {}",
            last.date, expected_last
        ));
    }
    Ok(())
}

/// Перша невдала перевірка: NotTradable, BadData, TooLittleHistory, PriceBelowMin, LowLiquidity.
pub fn scan_symbol(
    bars: &[Bar],
    tradable: bool,
    expected_last: NaiveDate,
    cfg: &ScannerConfig,
) -> Result<(), ScanReject> {
    if !tradable {
        return Err(ScanReject::NotTradable);
    }
    validate_bars(bars, expected_last).map_err(ScanReject::BadData)?;
    if bars.len() < cfg.min_history_bars {
        return Err(ScanReject::TooLittleHistory);
    }
    // validate_bars гарантує непорожність.
    if bars.last().is_some_and(|b| b.close < cfg.min_price) {
        return Err(ScanReject::PriceBelowMin);
    }
    match avg_dollar_volume(bars, cfg.avg_window) {
        Some(v) if v >= cfg.min_avg_dollar_volume => Ok(()),
        _ => Err(ScanReject::LowLiquidity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Duration};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// `n` будніх барів, що закінчуються `end` (будній день), з постійними ціною й обсягом.
    fn flat_bars(n: usize, end: NaiveDate, close: f64, volume: f64) -> Vec<Bar> {
        let mut dates = Vec::new();
        let mut x = end;
        while dates.len() < n {
            if x.weekday().num_days_from_monday() < 5 {
                dates.push(x);
            }
            x -= Duration::days(1);
        }
        dates.reverse();
        dates
            .into_iter()
            .map(|date| Bar {
                date,
                open: close,
                high: close * 1.01,
                low: close * 0.99,
                close,
                volume,
            })
            .collect()
    }

    fn end() -> NaiveDate {
        d(2026, 11, 4)
    }

    #[test]
    fn default_config_values() {
        let c = ScannerConfig::default();
        assert_eq!((c.min_price, c.min_avg_dollar_volume), (5.0, 20_000_000.0));
        assert_eq!((c.avg_window, c.min_history_bars), (20, 250));
    }

    #[test]
    fn scanner_accepts_good_symbol() {
        let bars = flat_bars(250, end(), 10.0, 3_000_000.0);
        assert_eq!(
            scan_symbol(&bars, true, end(), &ScannerConfig::default()),
            Ok(())
        );
    }

    #[test]
    fn scanner_rejections() {
        let cfg = ScannerConfig::default();
        let price = flat_bars(250, end(), 4.99, 10_000_000.0);
        assert_eq!(
            scan_symbol(&price, true, end(), &cfg),
            Err(ScanReject::PriceBelowMin)
        );
        // Оборот 19M: 10 * 1.9M.
        let liq = flat_bars(250, end(), 10.0, 1_900_000.0);
        assert_eq!(
            scan_symbol(&liq, true, end(), &cfg),
            Err(ScanReject::LowLiquidity)
        );
        let short = flat_bars(249, end(), 10.0, 3_000_000.0);
        assert_eq!(
            scan_symbol(&short, true, end(), &cfg),
            Err(ScanReject::TooLittleHistory)
        );
        let ok = flat_bars(250, end(), 10.0, 3_000_000.0);
        assert_eq!(
            scan_symbol(&ok, false, end(), &cfg),
            Err(ScanReject::NotTradable)
        );
        // Останній бар — учорашній (3 листопада), очікується 4-те.
        let stale = flat_bars(250, d(2026, 11, 3), 10.0, 3_000_000.0);
        match scan_symbol(&stale, true, end(), &cfg) {
            Err(ScanReject::BadData(m)) => assert!(m.starts_with("stale"), "{m}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn scan_order_not_tradable_before_bad_data() {
        let cfg = ScannerConfig::default();
        assert_eq!(
            scan_symbol(&[], false, end(), &cfg),
            Err(ScanReject::NotTradable)
        );
        assert!(matches!(
            scan_symbol(&[], true, end(), &cfg),
            Err(ScanReject::BadData(_))
        ));
    }

    #[test]
    fn validate_bars_catches_high_below_low_and_zero_volume() {
        let good = flat_bars(5, end(), 10.0, 1000.0);
        assert!(validate_bars(&good, end()).is_ok());

        let mut hl = good.clone();
        hl[2].high = 9.0;
        hl[2].low = 9.5;
        assert!(
            validate_bars(&hl, end())
                .unwrap_err()
                .contains("high below low")
        );

        let mut zv = good.clone();
        zv[4].volume = 0.0;
        assert!(
            validate_bars(&zv, end())
                .unwrap_err()
                .contains("zero volume")
        );

        let mut nan = good.clone();
        nan[1].close = f64::NAN;
        assert!(
            validate_bars(&nan, end())
                .unwrap_err()
                .contains("invalid price")
        );

        let mut neg = good.clone();
        neg[1].open = 0.0;
        assert!(
            validate_bars(&neg, end())
                .unwrap_err()
                .contains("invalid price")
        );

        let mut order = good.clone();
        order.swap(1, 2);
        assert!(
            validate_bars(&order, end())
                .unwrap_err()
                .contains("out of order")
        );

        assert!(validate_bars(&[], end()).is_err());
    }

    #[test]
    fn scan_reject_serde_shape() {
        let v = serde_json::to_value(ScanReject::BadData("x".into())).unwrap();
        assert_eq!(v, serde_json::json!({"type": "bad_data", "detail": "x"}));
        let v = serde_json::to_value(ScanReject::NotTradable).unwrap();
        assert_eq!(v, serde_json::json!({"type": "not_tradable"}));
    }
}
