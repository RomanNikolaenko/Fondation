//! Відсутність заглядання в майбутнє і детермінованість прогону.

mod common;

use fond_backtest::{BacktestResult, run_backtest};

use common::{d, from, last_session, midpoint_session, req, synthetic_universe, truncate};

#[test]
fn no_look_ahead_decisions_match_truncated_data() {
    let data = synthetic_universe(10, 3);
    let full = run_backtest(&req(from(), last_session(&data)), &data).unwrap();
    let t = midpoint_session(&data);
    let cut = run_backtest(&req(from(), t), &truncate(&data, t)).unwrap();

    let before = |r: &BacktestResult| {
        r.decisions
            .iter()
            .filter(|d| d.date < t)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(
        before(&full).iter().any(|d| d.accepted),
        "fixture must accept entries before {t}"
    );
    assert_eq!(before(&full), before(&cut));

    // Крива капіталу та угоди, закриті до t, теж не залежать від майбутніх барів.
    let curve = |r: &BacktestResult| {
        r.equity
            .iter()
            .filter(|p| p.date < t)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(curve(&full), curve(&cut));
    let closed = |r: &BacktestResult| {
        r.trades
            .iter()
            .filter(|x| x.exit_date < t)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(!closed(&full).is_empty());
    assert_eq!(closed(&full), closed(&cut));
}

#[test]
fn same_input_gives_bit_identical_output() {
    // Дві окремо зібрані HashMap мають різний порядок обходу.
    let r = req(from(), d(2022, 6, 30));
    let a = run_backtest(&r, &synthetic_universe(10, 3)).unwrap();
    let b = run_backtest(&r, &synthetic_universe(10, 3)).unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_eq!(a, b);
}
