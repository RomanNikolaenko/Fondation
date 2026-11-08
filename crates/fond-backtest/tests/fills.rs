//! Виконання ордерів симулятором на ручних даних: AAA і BBB + SPY.
//!
//! AAA має сигнал `trend_pullback` у четвер T0 = 06.03.2025 з close рівно 100.0, тож
//! limit = 101.00, а кількість обмежена `max_position_pct`: floor(100000 · 0.15 / 101) = 148.

mod common;

use std::collections::HashMap;

use chrono::{Datelike, Duration, NaiveDate};
use fond_backtest::sim::{commission, entry_fill_price, intraday_exit};
use fond_backtest::{
    BacktestError, BacktestResult, BtDecision, CostModel, ExitReason, HistoricalData, Limitation,
    run_backtest,
};
use fond_core::testkit::{bars_from_closes, breakout_series, weekdays_from};
use fond_core::types::{Bar, StrategyId};

use common::{d, req, sym};

/// Барів історії до T0 включно.
const HIST: usize = 262;
/// 5 б.п. за замовчуванням.
const SLIP: f64 = 5.0 / 10_000.0;
/// Звичайний день після T0: open нижче limit, low вище стопу, close нижче SMA5.
const NORMAL: [f64; 4] = [100.5, 100.8, 100.2, 100.6];

fn t0() -> NaiveDate {
    d(2025, 3, 6)
}

fn weekdays_back(end: NaiveDate, n: usize) -> Vec<NaiveDate> {
    let mut out = Vec::with_capacity(n);
    let mut x = end;
    while out.len() < n {
        if x.weekday().num_days_from_monday() < 5 {
            out.push(x);
        }
        x -= Duration::days(1);
    }
    out.reverse();
    out
}

fn start() -> NaiveDate {
    weekdays_back(t0(), HIST)[0]
}

/// `k`-та сесія після T0: after(0) = пт 07.03, after(1) = пн 10.03, after(2) = вт 11.03, ...
fn after(k: usize) -> NaiveDate {
    weekdays_from(t0() + Duration::days(1), k + 1)[k]
}

fn bar(date: NaiveDate, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        date,
        open,
        high,
        low,
        close,
        volume: 1_000_000.0,
    }
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * (1.0 + b.abs()), "{a} != {b}");
}

fn geometric(first: f64, growth: f64, n: usize) -> Vec<f64> {
    (0..n).map(|i| first * growth.powi(i as i32)).collect()
}

fn steady(first: f64, growth: f64, n: usize) -> Vec<Bar> {
    bars_from_closes(start(), &geometric(first, growth, n), 1_000_000.0)
}

/// Дописує бари `[open, high, low, close]` на наступні будні після останнього бару.
fn append(bars: &mut Vec<Bar>, ohlc: &[[f64; 4]]) {
    let next = bars.last().unwrap().date + Duration::days(1);
    for (date, &[o, h, l, c]) in weekdays_from(next, ohlc.len()).into_iter().zip(ohlc) {
        bars.push(bar(date, o, h, l, c));
    }
}

/// 260 днів росту ×1.004, потім 2 дні ×0.985; close у T0 рівно 100.0.
fn aaa_history() -> Vec<Bar> {
    let mut closes = vec![0.0; HIST];
    closes[HIST - 1] = 100.0;
    for i in (0..HIST - 1).rev() {
        closes[i] = closes[i + 1] / if i >= HIST - 3 { 0.985 } else { 1.004 };
    }
    bars_from_closes(start(), &closes, 1_000_000.0)
}

/// SPY рівно росте `n` днів (фільтр ринку увімкнений); `symbols` — (тікер, сектор, бари).
fn universe(symbols: Vec<(&str, &str, Vec<Bar>)>, n: usize) -> HistoricalData {
    let mut bars = HashMap::from([(sym("SPY"), steady(400.0, 1.001, n))]);
    let mut sectors = HashMap::new();
    for (s, sector, b) in symbols {
        sectors.insert(sym(s), sector.to_string());
        bars.insert(sym(s), b);
    }
    HistoricalData {
        bars,
        sectors,
        earnings: None,
        earnings_days_ahead: 3,
    }
}

/// AAA із сигналом у T0 і заданими барами після нього; BBB рівно росте й сигналів не дає.
fn data_with(aaa_after: &[[f64; 4]]) -> HistoricalData {
    let n = HIST + aaa_after.len();
    let mut aaa = aaa_history();
    append(&mut aaa, aaa_after);
    universe(
        vec![
            ("AAA", "Tech", aaa),
            ("BBB", "Health", steady(50.0, 1.002, n)),
        ],
        n,
    )
}

fn accepted(r: &BacktestResult) -> Vec<&BtDecision> {
    r.decisions.iter().filter(|x| x.accepted).collect()
}

fn equity_on(r: &BacktestResult, date: NaiveDate) -> f64 {
    r.equity.iter().find(|p| p.date == date).unwrap().equity
}

#[test]
fn entry_fills_next_open_when_below_limit() {
    approx(
        entry_fill_price(100.5, 101.0, SLIP).unwrap(),
        100.5 * (1.0 + SLIP),
    );
    // open·(1+slip) = 101.04 > limit -> обмежується limit.
    approx(entry_fill_price(100.99, 101.0, SLIP).unwrap(), 101.0);

    let r = run_backtest(&req(t0(), after(0)), &data_with(&[NORMAL])).unwrap();
    let acc = accepted(&r);
    assert_eq!(acc.len(), 1);
    assert_eq!(
        (
            acc[0].date,
            acc[0].symbol.as_str(),
            acc[0].strategy.as_str()
        ),
        (t0(), "AAA", "trend_pullback")
    );
    assert_eq!(acc[0].reason, None);

    assert_eq!(r.trades.len(), 1);
    let tr = &r.trades[0];
    let entry = 100.5 * (1.0 + SLIP); // 100.55025 <= 101.00
    assert_eq!((tr.symbol.as_str(), tr.entry_date), ("AAA", after(0)));
    approx(tr.entry_price, entry);
    assert_eq!(tr.qty, 148);
    // Останній день: ліквідація за close з прослизанням.
    assert_eq!(
        (tr.exit_date, &tr.exit_reason),
        (after(0), &ExitReason::EndOfTest)
    );
    approx(tr.exit_price, 100.6 * (1.0 - SLIP));
    approx(tr.pnl, (100.6 * (1.0 - SLIP) - entry) * 148.0);

    assert_eq!(r.equity.len(), 2);
    assert!(!r.equity[0].in_market && !r.equity[1].in_market);
    approx(r.equity[0].equity, 100_000.0);
    approx(
        r.equity[1].equity,
        100_000.0 + (100.6 * (1.0 - SLIP) - entry) * 148.0,
    );
    approx(r.equity[0].benchmark, 100_000.0);
    approx(r.equity[1].benchmark, 100_000.0 * 1.001);
}

#[test]
fn gap_above_limit_does_not_fill() {
    assert_eq!(entry_fill_price(102.0, 101.0, SLIP), None);

    let r = run_backtest(
        &req(t0(), after(0)),
        &data_with(&[[102.0, 102.5, 101.5, 102.2]]),
    )
    .unwrap();
    let acc = accepted(&r);
    assert_eq!(acc.len(), 1);
    assert_eq!(acc[0].symbol, sym("AAA"));
    assert!(r.trades.is_empty());
    assert!(
        r.equity
            .iter()
            .all(|p| !p.in_market && p.equity == 100_000.0)
    );
}

#[test]
fn entry_dropped_without_bar_next_day() {
    // AAA не має бару в п'ятницю 07.03; у понеділок open 100.5 < limit, але ордер `day` уже знято.
    let n = HIST + 2;
    let mut aaa = aaa_history();
    aaa.push(bar(after(1), 100.5, 100.8, 100.2, 100.6));
    let data = universe(
        vec![
            ("AAA", "Tech", aaa),
            ("BBB", "Health", steady(50.0, 1.002, n)),
        ],
        n,
    );
    let r = run_backtest(&req(t0(), after(1)), &data).unwrap();
    assert_eq!(accepted(&r).len(), 1);
    assert!(r.trades.is_empty());
    assert!(r.equity.iter().all(|p| !p.in_market));
}

#[test]
fn stop_gap_down_fills_at_open() {
    let (px, why) =
        intraday_exit(&bar(after(1), 90.0, 91.0, 89.0, 90.5), 95.0, None, SLIP).unwrap();
    assert_eq!(why, ExitReason::Stop);
    approx(px, 90.0 * (1.0 - SLIP));

    // Стоп AAA ≈ 97 (close − 2·ATR); у понеділок геп до 90.
    let r = run_backtest(
        &req(t0(), after(1)),
        &data_with(&[NORMAL, [90.0, 91.0, 89.0, 90.5]]),
    )
    .unwrap();
    assert_eq!(r.trades.len(), 1);
    let tr = &r.trades[0];
    assert_eq!(
        (tr.exit_date, &tr.exit_reason),
        (after(1), &ExitReason::Stop)
    );
    approx(tr.exit_price, 90.0 * (1.0 - SLIP));
    approx(tr.pnl, (90.0 * (1.0 - SLIP) - 100.5 * (1.0 + SLIP)) * 148.0);
}

#[test]
fn stop_and_target_same_day_assumes_stop() {
    let day = after(1);
    let (px, why) = intraday_exit(
        &bar(day, 100.0, 111.0, 94.0, 105.0),
        95.0,
        Some(110.0),
        SLIP,
    )
    .unwrap();
    assert_eq!(why, ExitReason::Stop);
    approx(px, 95.0 * (1.0 - SLIP));

    // Лише ціль: за max(open, tp).
    let (px, why) = intraday_exit(
        &bar(day, 100.0, 111.0, 99.0, 105.0),
        95.0,
        Some(110.0),
        SLIP,
    )
    .unwrap();
    assert_eq!(why, ExitReason::Target);
    approx(px, 110.0 * (1.0 - SLIP));
    let (px, _) = intraday_exit(
        &bar(day, 112.0, 113.0, 111.0, 112.5),
        95.0,
        Some(110.0),
        SLIP,
    )
    .unwrap();
    approx(px, 112.0 * (1.0 - SLIP));

    // Нічого не зачеплено або цілі немає.
    assert_eq!(
        intraday_exit(
            &bar(day, 100.0, 109.0, 96.0, 105.0),
            95.0,
            Some(110.0),
            SLIP
        ),
        None
    );
    assert_eq!(
        intraday_exit(&bar(day, 100.0, 150.0, 96.0, 105.0), 95.0, None, SLIP),
        None
    );
}

#[test]
fn exit_signal_fills_next_open() {
    // Пн 10.03 close 103.0 > SMA5 ≈ 101.6 -> вихід за open вт 11.03.
    let r = run_backtest(
        &req(t0(), after(2)),
        &data_with(&[
            NORMAL,
            [100.8, 103.5, 100.7, 103.0],
            [103.2, 103.6, 102.9, 103.4],
        ]),
    )
    .unwrap();
    assert_eq!(r.trades.len(), 1);
    let tr = &r.trades[0];
    assert_eq!(tr.exit_date, after(2));
    approx(tr.exit_price, 103.2 * (1.0 - SLIP));
    match &tr.exit_reason {
        ExitReason::Signal(why) => assert!(why.contains("SMA5"), "{why}"),
        other => panic!("expected signal exit, got {other:?}"),
    }
}

#[test]
fn raised_stop_applies_from_next_day() {
    // BRK: боковик 100 ± 2, у T0 пробій до 106 на подвоєному обсязі -> сигнал breakout.
    let mut brk = breakout_series(start(), HIST - 1);
    assert_eq!(brk.last().unwrap().date, t0());
    // Вхід у пт 07.03 (low 106.0), далі 19 днів росту на 1%: увечері 20-го дня
    // мінімум за 20 барів = 106.0, і стоп підтягується до 106.
    let mut ohlc = vec![[106.5, 108.0, 106.0, 107.5]];
    let mut c: f64 = 107.5;
    for _ in 0..19 {
        let o = c;
        c *= 1.01;
        ohlc.push([o, c * 1.005, o * 0.995, c]);
    }
    // Наступного дня low 105.5 <= 106 при open ≈ 130 -> вихід за 106·(1−slip).
    ohlc.push([c, c * 1.002, 105.5, 106.5]);
    append(&mut brk, &ohlc);
    let n = brk.len();
    let exit_day = brk.last().unwrap().date;
    let data = universe(vec![("BRK", "Tech", brk)], n);

    let r = run_backtest(&req(t0(), exit_day), &data).unwrap();
    assert_eq!(r.trades.len(), 1);
    let tr = &r.trades[0];
    assert_eq!(tr.strategy.as_str(), "breakout");
    assert_eq!(tr.entry_date, after(0));
    approx(tr.entry_price, 106.5 * (1.0 + SLIP));
    assert_eq!(
        (tr.exit_date, &tr.exit_reason),
        (exit_day, &ExitReason::Stop)
    );
    approx(tr.exit_price, 106.0 * (1.0 - SLIP));
}

/// AAA входить у пт 07.03 і обвалюється у вт 11.03 до `crash` (стоп за open).
/// BBB відкочується з пн 10.03 шість днів поспіль: сигнали з вт 11.03 по пн 17.03.
fn crash_week(crash: f64) -> HistoricalData {
    let flat = [crash, crash * 1.01, crash * 0.99, crash];
    let mut aaa = aaa_history();
    append(
        &mut aaa,
        &[
            NORMAL,
            [100.6, 100.7, 100.1, 100.4],
            [crash, crash * 1.02, crash * 0.98, crash],
            flat,
            flat,
            flat,
            flat,
            flat,
        ],
    );
    let mut closes = geometric(50.0, 1.002, HIST + 1);
    for _ in 0..6 {
        let c = *closes.last().unwrap() * 0.985;
        closes.push(c);
    }
    closes.push(*closes.last().unwrap());
    let bbb = bars_from_closes(start(), &closes, 1_000_000.0);
    let n = aaa.len();
    assert_eq!(n, bbb.len());
    universe(vec![("AAA", "Tech", aaa), ("BBB", "Health", bbb)], n)
}

#[test]
fn weekly_loss_blocks_until_next_week() {
    let crash = after(2);
    let monday = after(6);
    assert_eq!((crash, monday), (d(2025, 3, 11), d(2025, 3, 17)));

    let r = run_backtest(&req(t0(), after(7)), &crash_week(50.0)).unwrap();
    let aaa = r.trades.iter().find(|x| x.symbol == sym("AAA")).unwrap();
    assert_eq!(
        (aaa.exit_date, &aaa.exit_reason),
        (crash, &ExitReason::Stop)
    );
    approx(aaa.exit_price, 50.0 * (1.0 - SLIP));
    // База тижня — закриття пт 07.03; у вівторок ≈ −7.5%.
    let weekly = equity_on(&r, crash) / equity_on(&r, after(0)) - 1.0;
    assert!(weekly < -0.07 && weekly > -0.08, "{weekly}");

    let tue: Vec<_> = r.decisions.iter().filter(|x| x.date == crash).collect();
    assert!(!tue.is_empty() && tue.iter().all(|x| !x.accepted));
    // Ср–пт денного збитку вже немає, але тижневий ліміт діє до кінця ISO-тижня.
    for k in 3..=5 {
        let day = after(k);
        let dd: Vec<_> = r.decisions.iter().filter(|x| x.date == day).collect();
        assert!(
            dd.iter().any(|x| x.symbol == sym("BBB")),
            "no BBB signal on {day}"
        );
        for x in dd {
            assert!(!x.accepted, "{x:?}");
            assert_eq!(
                x.reason.as_deref(),
                Some("entries_blocked: weekly loss limit")
            );
        }
    }
    assert!(
        r.decisions
            .iter()
            .filter(|x| x.date >= crash && x.date < monday)
            .all(|x| !x.accepted)
    );

    let mon: Vec<_> = r
        .decisions
        .iter()
        .filter(|x| x.date == monday && x.symbol == sym("BBB"))
        .collect();
    assert_eq!(mon.len(), 1);
    assert!(mon[0].accepted, "{:?}", mon[0]);
    assert!(
        r.trades
            .iter()
            .any(|x| x.symbol == sym("BBB") && x.entry_date == after(7))
    );
}

#[test]
fn daily_loss_blocks_only_next_session() {
    let crash = after(2);
    let r = run_backtest(&req(t0(), after(7)), &crash_week(70.0)).unwrap();
    // ≈ −4.5%: денний ліміт (3%) порушено, тижневий (6%) — ні.
    let daily = equity_on(&r, crash) / equity_on(&r, after(1)) - 1.0;
    assert!(daily < -0.03 && daily > -0.06, "{daily}");
    let weekly = equity_on(&r, crash) / equity_on(&r, after(0)) - 1.0;
    assert!(weekly > -0.06, "{weekly}");

    let bbb_on = |day: NaiveDate| {
        r.decisions
            .iter()
            .find(|x| x.date == day && x.symbol == sym("BBB"))
            .cloned()
            .unwrap()
    };
    let tue = bbb_on(crash);
    assert!(!tue.accepted);
    assert_eq!(
        tue.reason.as_deref(),
        Some("entries_blocked: daily loss limit")
    );
    assert!(bbb_on(after(3)).accepted);
}

#[test]
fn commissions_charged_on_both_sides() {
    let c = CostModel {
        slippage_bps: 0.0,
        commission_per_share: 0.005,
        commission_min: 1.0,
    };
    approx(commission(100, &c), 1.0);
    approx(commission(1000, &c), 5.0);
    let min_only = CostModel {
        commission_per_share: 0.0,
        commission_min: 2.5,
        ..c.clone()
    };
    approx(commission(10, &min_only), 2.5);
    approx(commission(1000, &CostModel::default()), 0.0);

    let mut rq = req(t0(), after(0));
    rq.costs = CostModel {
        slippage_bps: 5.0,
        commission_per_share: 0.01,
        commission_min: 1.0,
    };
    let r = run_backtest(&rq, &data_with(&[NORMAL])).unwrap();
    let gross = (100.6 * (1.0 - SLIP) - 100.5 * (1.0 + SLIP)) * 148.0;
    // max(148 · 0.01, 1.0) = 1.48 за кожну сторону.
    approx(r.trades[0].pnl, gross - 2.0 * 1.48);
    // Ліквідація останнього дня відбувається до фіксації кривої, тож обидві комісії в equity.
    approx(r.equity[1].equity, 100_000.0 + gross - 2.0 * 1.48);
}

#[test]
fn earnings_inside_window_blocks_signal() {
    let mut data = data_with(&[NORMAL]);
    data.earnings = Some(HashMap::from([(
        sym("AAA"),
        vec![d(2025, 1, 30), after(0)],
    )]));
    let r = run_backtest(&req(t0(), after(0)), &data).unwrap();
    assert!(r.trades.is_empty());
    assert_eq!(
        r.decisions,
        vec![BtDecision {
            date: t0(),
            symbol: sym("AAA"),
            strategy: StrategyId::new("trend_pullback"),
            accepted: false,
            reason: Some("earnings".into()),
        }]
    );

    // Звіт далі ніж за 3 сесії не заважає входу.
    data.earnings = Some(HashMap::from([(sym("AAA"), vec![d(2025, 4, 24)])]));
    let r = run_backtest(&req(t0(), after(0)), &data).unwrap();
    assert_eq!(accepted(&r).len(), 1);
    assert_eq!(r.trades.len(), 1);
}

#[test]
fn limitations_listed() {
    let mut data = data_with(&[NORMAL]);
    let r = run_backtest(&req(t0(), after(0)), &data).unwrap();
    for l in [
        Limitation::SurvivorshipBias,
        Limitation::NewsFilterNotSimulated,
        Limitation::WeeklyLossApproximation,
        Limitation::EarningsFilterNotApplied,
    ] {
        assert!(r.limitations.contains(&l), "{l:?}");
    }
    assert_eq!(r.limitations.len(), 4);

    data.earnings = Some(HashMap::new());
    let r = run_backtest(&req(t0(), after(0)), &data).unwrap();
    assert!(
        !r.limitations
            .contains(&Limitation::EarningsFilterNotApplied)
    );
    assert_eq!(r.limitations.len(), 3);
}

#[test]
fn invalid_requests_are_errors() {
    let data = data_with(&[NORMAL]);
    let mut no_spy = data.clone();
    no_spy.bars.remove(&sym("SPY"));
    assert_eq!(
        run_backtest(&req(t0(), after(0)), &no_spy),
        Err(BacktestError::MissingMarketSymbol)
    );
    assert_eq!(
        run_backtest(&req(after(0), t0()), &data),
        Err(BacktestError::EmptyRange)
    );
    assert_eq!(
        run_backtest(&req(d(2030, 1, 1), d(2030, 2, 1)), &data),
        Err(BacktestError::EmptyRange)
    );
    let mut bad = req(t0(), after(0));
    bad.strategies.breakout.allocation = 0.9;
    assert!(matches!(
        run_backtest(&bad, &data),
        Err(BacktestError::InvalidConfig(e)) if !e.is_empty()
    ));
}
