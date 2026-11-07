//! Еталонний знімок метрик на синтетичному універсумі.

mod common;

use fond_backtest::run_backtest;

use common::{d, req, round6, synthetic_universe};

#[test]
fn golden_metrics_snapshot() {
    let r = run_backtest(
        &req(d(2021, 1, 4), d(2023, 12, 29)),
        &synthetic_universe(20, 4),
    )
    .unwrap();
    assert!(r.metrics.full.trades > 0, "fixture must produce trades");
    insta::assert_json_snapshot!(round6(&r.metrics));
}
