//! Ринковий фільтр: close індексу вище своєї SMA.

use fond_core::indicators::sma_last;
use fond_core::types::Bar;

use crate::config::MarketFilterParams;

/// `true`, якщо останній close вище SMA(`sma`); при нестачі барів (`len <= sma`) — `false`.
pub fn market_filter_ok(bars: &[Bar], p: &MarketFilterParams) -> bool {
    if p.sma == 0 || bars.len() <= p.sma {
        return false;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    match (sma_last(&closes, p.sma), closes.last()) {
        (Some(sma), Some(&close)) => close > sma,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use fond_core::testkit::bars_from_closes;

    use super::*;

    fn series(n: usize, step: f64) -> Vec<Bar> {
        let closes: Vec<f64> = (0..n).map(|i| 100.0 + step * i as f64).collect();
        bars_from_closes(NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(), &closes, 1e6)
    }

    #[test]
    fn market_filter() {
        let p = MarketFilterParams::default();
        assert!(market_filter_ok(&series(260, 0.5), &p));
        assert!(!market_filter_ok(&series(260, -0.2), &p));
        assert!(!market_filter_ok(&series(200, 0.5), &p));
        assert!(market_filter_ok(&series(201, 0.5), &p));
        assert!(!market_filter_ok(&[], &p));
    }
}
