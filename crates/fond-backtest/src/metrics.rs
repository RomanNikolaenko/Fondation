//! Метрики ефективності бектесту.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::request::{BtTrade, EquityPoint};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Metrics {
    pub cagr: f64,
    pub max_drawdown: f64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_drawdown_days: i64,
    pub sharpe: f64,
    pub sortino: f64,
    pub win_rate: f64,
    pub profit_factor: Option<f64>,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub avg_hold_days: f64,
    pub trades: usize,
    pub exposure: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MetricsReport {
    pub full: Metrics,
    pub in_sample: Option<Metrics>,
    pub out_of_sample: Option<Metrics>,
    pub benchmark: Metrics,
}

const TRADING_DAYS: f64 = 252.0;

/// Нескінченні й NaN значення ніколи не потрапляють у `Metrics`.
fn finite(x: f64) -> f64 {
    if x.is_finite() { x } else { 0.0 }
}

struct CurveStats {
    cagr: f64,
    max_drawdown: f64,
    max_drawdown_days: i64,
    sharpe: f64,
    sortino: f64,
}

impl CurveStats {
    const ZERO: Self = Self {
        cagr: 0.0,
        max_drawdown: 0.0,
        max_drawdown_days: 0,
        sharpe: 0.0,
        sortino: 0.0,
    };
}

fn curve_stats(points: &[(NaiveDate, f64)]) -> CurveStats {
    let (Some(&(first_date, first)), Some(&(last_date, last))) = (points.first(), points.last())
    else {
        return CurveStats::ZERO;
    };
    if points.len() < 2 {
        return CurveStats::ZERO;
    }

    let days = (last_date - first_date).num_days();
    let cagr = if days <= 0 || first <= 0.0 {
        0.0
    } else {
        (last / first).powf(365.25 / days as f64) - 1.0
    };

    // Просадка: пік, глибина та тривалість від піку до відновлення.
    let mut peak = first;
    let mut peak_date = first_date;
    let mut underwater = false;
    let mut max_dd = 0.0_f64;
    let mut max_dd_days = 0_i64;
    for &(date, e) in points {
        if e >= peak {
            if underwater {
                max_dd_days = max_dd_days.max((date - peak_date).num_days());
                underwater = false;
            }
            peak = e;
            peak_date = date;
        } else {
            underwater = true;
            if peak > 0.0 {
                max_dd = max_dd.max((peak - e) / peak);
            }
        }
    }
    if underwater {
        max_dd_days = max_dd_days.max((last_date - peak_date).num_days());
    }

    let returns: Vec<f64> = points
        .windows(2)
        .map(|w| {
            if w[0].1 > 0.0 {
                w[1].1 / w[0].1 - 1.0
            } else {
                0.0
            }
        })
        .collect();
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let sharpe = if returns.len() < 2 {
        0.0
    } else {
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std = var.sqrt();
        if std > 0.0 {
            mean / std * TRADING_DAYS.sqrt()
        } else {
            0.0
        }
    };
    let downside = (returns.iter().map(|r| r.min(0.0).powi(2)).sum::<f64>() / n).sqrt();
    let sortino = if downside > 0.0 {
        mean / downside * TRADING_DAYS.sqrt()
    } else {
        0.0
    };

    CurveStats {
        cagr: finite(cagr),
        max_drawdown: finite(max_dd),
        max_drawdown_days: max_dd_days,
        sharpe: finite(sharpe),
        sortino: finite(sortino),
    }
}

fn build(points: &[(NaiveDate, f64)], trades: &[&BtTrade], exposure: f64) -> Metrics {
    let c = curve_stats(points);

    let wins: Vec<f64> = trades.iter().map(|t| t.pnl).filter(|p| *p > 0.0).collect();
    let losses: Vec<f64> = trades.iter().map(|t| t.pnl).filter(|p| *p < 0.0).collect();
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let gross_loss = losses.iter().sum::<f64>().abs();
    let profit_factor = if losses.is_empty() || gross_loss == 0.0 {
        None
    } else {
        Some(finite(wins.iter().sum::<f64>() / gross_loss))
    };
    let (win_rate, avg_hold_days) = if trades.is_empty() {
        (0.0, 0.0)
    } else {
        let n = trades.len() as f64;
        let hold: i64 = trades
            .iter()
            .map(|t| (t.exit_date - t.entry_date).num_days())
            .sum();
        (wins.len() as f64 / n, hold as f64 / n)
    };

    Metrics {
        cagr: c.cagr,
        max_drawdown: c.max_drawdown,
        max_drawdown_days: c.max_drawdown_days,
        sharpe: c.sharpe,
        sortino: c.sortino,
        win_rate,
        profit_factor,
        avg_win: finite(mean(&wins)),
        avg_loss: finite(mean(&losses)),
        avg_hold_days,
        trades: trades.len(),
        exposure: finite(exposure),
    }
}

fn exposure_of(equity: &[EquityPoint]) -> f64 {
    if equity.is_empty() {
        0.0
    } else {
        equity.iter().filter(|p| p.in_market).count() as f64 / equity.len() as f64
    }
}

fn strategy_metrics(equity: &[EquityPoint], trades: &[&BtTrade]) -> Metrics {
    let points: Vec<_> = equity.iter().map(|p| (p.date, p.equity)).collect();
    build(&points, trades, exposure_of(equity))
}

pub fn compute_metrics(equity: &[EquityPoint], trades: &[BtTrade]) -> Metrics {
    let refs: Vec<&BtTrade> = trades.iter().collect();
    strategy_metrics(equity, &refs)
}

pub fn report(
    equity: &[EquityPoint],
    trades: &[BtTrade],
    split: Option<NaiveDate>,
) -> MetricsReport {
    let bench: Vec<_> = equity.iter().map(|p| (p.date, p.benchmark)).collect();
    let (in_sample, out_of_sample) = match split {
        None => (None, None),
        Some(s) => {
            let before: Vec<EquityPoint> = equity.iter().filter(|p| p.date < s).cloned().collect();
            let mut after: Vec<EquityPoint> = Vec::new();
            if let Some(base) = before.last() {
                after.push(base.clone());
            }
            after.extend(equity.iter().filter(|p| p.date >= s).cloned());
            let is_trades: Vec<&BtTrade> = trades.iter().filter(|t| t.entry_date < s).collect();
            let oos_trades: Vec<&BtTrade> = trades.iter().filter(|t| t.entry_date >= s).collect();
            (
                Some(strategy_metrics(&before, &is_trades)),
                Some(strategy_metrics(&after, &oos_trades)),
            )
        }
    };
    MetricsReport {
        full: compute_metrics(equity, trades),
        in_sample,
        out_of_sample,
        benchmark: build(&bench, &[], 1.0),
    }
}

#[cfg(test)]
mod tests {
    use fond_core::types::{StrategyId, Symbol};

    use super::*;
    use crate::request::ExitReason;

    fn d(offset: i64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap() + chrono::Duration::days(offset)
    }

    fn pt(offset: i64, equity: f64, benchmark: f64, in_market: bool) -> EquityPoint {
        EquityPoint {
            date: d(offset),
            equity,
            benchmark,
            in_market,
        }
    }

    fn curve(values: &[(i64, f64)]) -> Vec<EquityPoint> {
        values.iter().map(|&(o, e)| pt(o, e, e, true)).collect()
    }

    fn trade(entry: i64, exit: i64, pnl: f64) -> BtTrade {
        BtTrade {
            symbol: Symbol::new("AAPL"),
            strategy: StrategyId::new("trend_pullback"),
            entry_date: d(entry),
            entry_price: 100.0,
            exit_date: d(exit),
            exit_price: 100.0,
            qty: 1,
            pnl,
            exit_reason: ExitReason::Stop,
        }
    }

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} != {b} (tol {tol})");
    }

    #[test]
    fn cagr_of_doubling_in_one_year() {
        let m = compute_metrics(&curve(&[(0, 100.0), (365, 200.0)]), &[]);
        // 2^(365.25/365) - 1 ≈ 1.00095
        approx(m.cagr, 0.998, 0.01);
    }

    #[test]
    fn drawdown_and_duration() {
        let m = compute_metrics(
            &curve(&[(0, 100.0), (1, 120.0), (2, 90.0), (3, 130.0)]),
            &[],
        );
        approx(m.max_drawdown, 0.25, 1e-12);
        assert_eq!(m.max_drawdown_days, 2); // from 120 (day 1) to 130 (day 3)
    }

    #[test]
    fn drawdown_never_regained_runs_to_last_point() {
        let m = compute_metrics(
            &curve(&[(0, 100.0), (1, 120.0), (2, 90.0), (6, 100.0)]),
            &[],
        );
        assert_eq!(m.max_drawdown_days, 5); // day 1 -> day 6
    }

    #[test]
    fn flat_curve_has_zero_sharpe_and_sortino() {
        let m = compute_metrics(
            &curve(&[(0, 100.0), (1, 100.0), (2, 100.0), (3, 100.0)]),
            &[],
        );
        assert_eq!(m.sharpe, 0.0);
        assert_eq!(m.sortino, 0.0);
        assert_eq!(m.max_drawdown, 0.0);
        assert_eq!(m.max_drawdown_days, 0);
    }

    #[test]
    fn sharpe_and_sortino_hand_computed() {
        // r = [0.2, -0.05]; mean .075; sample std .1767767; downside rms .0353553
        let m = compute_metrics(&curve(&[(0, 100.0), (1, 120.0), (2, 114.0)]), &[]);
        approx(m.sharpe, 0.075 / 0.176_776_695 * 252f64.sqrt(), 1e-6);
        approx(m.sharpe, 6.735, 1e-3);
        approx(m.sortino, 0.075 / 0.035_355_339 * 252f64.sqrt(), 1e-6);
        approx(m.sortino, 33.675, 1e-3);
    }

    #[test]
    fn profit_factor_none_without_losses() {
        let m = compute_metrics(&curve(&[(0, 100.0), (5, 110.0)]), &[trade(0, 2, 10.0)]);
        assert_eq!(m.profit_factor, None);
        assert_eq!(m.win_rate, 1.0);
    }

    #[test]
    fn trade_statistics() {
        let trades = [
            trade(0, 2, 30.0),
            trade(1, 5, -10.0),
            trade(2, 3, 10.0),
            trade(3, 4, -20.0),
        ];
        let m = compute_metrics(&curve(&[(0, 100.0), (5, 110.0)]), &trades);
        assert_eq!(m.trades, 4);
        approx(m.win_rate, 0.5, 1e-12);
        approx(m.profit_factor.unwrap(), 40.0 / 30.0, 1e-12);
        approx(m.avg_win, 20.0, 1e-12);
        approx(m.avg_loss, -15.0, 1e-12);
        approx(m.avg_hold_days, (2.0 + 4.0 + 1.0 + 1.0) / 4.0, 1e-12);
    }

    #[test]
    fn exposure_is_share_of_in_market_points() {
        let eq = vec![
            pt(0, 100.0, 100.0, true),
            pt(1, 100.0, 100.0, false),
            pt(2, 100.0, 100.0, false),
            pt(3, 100.0, 100.0, true),
        ];
        approx(compute_metrics(&eq, &[]).exposure, 0.5, 1e-12);
    }

    #[test]
    fn fewer_than_two_points_and_no_trades_are_zeroed() {
        for eq in [vec![], curve(&[(0, 100.0)])] {
            let m = compute_metrics(&eq, &[]);
            assert_eq!(m.cagr, 0.0);
            assert_eq!(m.max_drawdown, 0.0);
            assert_eq!(m.max_drawdown_days, 0);
            assert_eq!(m.sharpe, 0.0);
            assert_eq!(m.sortino, 0.0);
            assert_eq!(m.win_rate, 0.0);
            assert_eq!(m.avg_win, 0.0);
            assert_eq!(m.avg_loss, 0.0);
            assert_eq!(m.avg_hold_days, 0.0);
            assert_eq!(m.profit_factor, None);
            assert_eq!(m.trades, 0);
        }
        assert_eq!(compute_metrics(&[], &[]).exposure, 0.0);
    }

    #[test]
    fn split_partitions_trades_and_curve() {
        let eq = curve(&[(0, 100.0), (1, 110.0), (2, 121.0), (3, 100.0), (4, 110.0)]);
        let trades = [trade(0, 1, 10.0), trade(1, 2, -5.0), trade(3, 4, 7.0)];
        let r = report(&eq, &trades, Some(d(3)));
        let is = r.in_sample.unwrap();
        let oos = r.out_of_sample.unwrap();
        assert_eq!(is.trades, 2);
        assert_eq!(oos.trades, 1);
        assert_eq!(r.full.trades, 3);
        // in-sample curve: days 0..=2 -> 100,110,121 -> no drawdown
        assert_eq!(is.max_drawdown, 0.0);
        // OOS curve: base day 2 (121) + days 3,4 -> 121,100,110
        approx(oos.max_drawdown, (121.0 - 100.0) / 121.0, 1e-12);
    }

    #[test]
    fn no_split_leaves_samples_none() {
        let r = report(&curve(&[(0, 100.0), (1, 101.0)]), &[], None);
        assert!(r.in_sample.is_none() && r.out_of_sample.is_none());
    }

    #[test]
    fn split_with_thin_side_is_still_some_and_zeroed() {
        let eq = curve(&[(0, 100.0), (1, 110.0)]);
        let r = report(&eq, &[], Some(d(1)));
        // in-sample has a single point
        let is = r.in_sample.unwrap();
        assert_eq!(is.cagr, 0.0);
        assert_eq!(is.sharpe, 0.0);
        assert!(r.out_of_sample.is_some());
    }

    #[test]
    fn benchmark_metrics_use_benchmark_series() {
        let eq = vec![
            pt(0, 100.0, 100.0, true),
            pt(1, 100.0, 120.0, false),
            pt(2, 100.0, 90.0, false),
            pt(3, 100.0, 130.0, false),
        ];
        let r = report(&eq, &[trade(0, 1, 5.0)], None);
        approx(r.benchmark.max_drawdown, 0.25, 1e-12);
        assert_eq!(r.benchmark.max_drawdown_days, 2);
        assert_eq!(r.benchmark.trades, 0);
        assert_eq!(r.benchmark.exposure, 1.0);
        assert_eq!(r.full.max_drawdown, 0.0);
    }
}
