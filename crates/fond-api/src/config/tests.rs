use super::*;
use chrono::NaiveTime;
use fond_core::types::ExecutionMode;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

#[test]
fn defaults_are_valid() {
    EngineConfig::default().validate().unwrap();
}

#[test]
fn defaults_match_contract() {
    let c = EngineConfig::default();
    assert_eq!(c.execution_mode, ExecutionMode::Approval);
    assert_eq!(c.api_port, 47615);
    assert_eq!(c.refresh_window_days, 600);
    assert_eq!(c.schedule.analysis, t(16, 30));
    assert_eq!(c.schedule.approval_deadline, t(8, 55));
    assert_eq!(c.schedule.submit_window_start, t(9, 0));
    assert_eq!(c.schedule.submit_window_end, t(9, 25));
    assert_eq!(c.schedule.reconcile_every_minutes, 60);
    assert!(c.news_filter.enabled);
    assert_eq!(c.news_filter.model, "claude-haiku-5-5");
    assert_eq!(c.news_filter.effort, "low");
    assert_eq!(c.news_filter.lookback_days, 7);
    assert_eq!(c.news_filter.max_items, 20);
    assert_eq!(c.news_filter.caution_in_auto, CautionPolicy::Allow);
    assert_eq!(c.news_filter.on_unavailable, UnavailablePolicy::Block);
    assert_eq!(c.news_filter.daily_budget_usd, 1.0);
    assert_eq!(c.news_filter.monthly_budget_usd, 20.0);
    assert_eq!(c.news_filter.input_usd_per_mtok, 0.10);
    assert_eq!(c.news_filter.output_usd_per_mtok, 0.50);
    assert!(c.earnings_filter.enabled);
    assert_eq!(c.earnings_filter.days_ahead, 3);
    assert_eq!(c.earnings_filter.lookahead_calendar_days, 10);
    assert_eq!(c.earnings_filter.on_unavailable, UnavailablePolicy::Block);
    assert!(!c.portfolio.enabled);
    assert_eq!(c.portfolio.rebalance, RebalanceMode::Off);
    assert!(c.portfolio.targets.is_empty());
    assert_eq!(c.portfolio.drift, 0.05);
    assert_eq!(c.portfolio.min_order_usd, 100.0);
    assert_eq!(c.ui.disclaimer_accepted_at, None);
    assert!(c.ui.notifications_enabled);
}

#[test]
fn partial_toml_fills_defaults() {
    let c: EngineConfig =
        toml::from_str("execution_mode = \"auto\"\n[risk]\nmax_open_positions = 5\n").unwrap();
    assert_eq!(c.execution_mode, ExecutionMode::Auto);
    assert_eq!(c.risk.max_open_positions, 5);
    assert_eq!(c.risk.risk_per_trade, 0.01);
    assert_eq!(
        c.schedule.analysis,
        NaiveTime::from_hms_opt(16, 30, 0).unwrap()
    );
}

#[test]
fn toml_roundtrip_preserves_config() {
    let mut c = EngineConfig::default();
    c.schedule.analysis = t(17, 0);
    c.news_filter.caution_in_auto = CautionPolicy::Block;
    let s = toml::to_string_pretty(&c).unwrap();
    assert!(s.contains("analysis = \"17:00\""), "{s}");
    assert!(s.contains("caution_in_auto = \"block\""), "{s}");
    let back: EngineConfig = toml::from_str(&s).unwrap();
    assert_eq!(back, c);
}

#[test]
fn schedule_time_accepts_hh_mm_and_hh_mm_ss() {
    let a: ScheduleConfig = toml::from_str("analysis = \"17:05\"").unwrap();
    let b: ScheduleConfig = toml::from_str("analysis = \"17:05:30\"").unwrap();
    assert_eq!(a.analysis, t(17, 5));
    assert_eq!(b.analysis, NaiveTime::from_hms_opt(17, 5, 30).unwrap());
    assert!(toml::from_str::<ScheduleConfig>("analysis = \"abc\"").is_err());
}

#[test]
fn validate_rejects_bad_values() {
    let mut c = EngineConfig::default();
    c.strategies.trend_pullback.allocation = 0.6;
    c.strategies.breakout.allocation = 0.6;
    c.schedule.approval_deadline = t(9, 5);
    c.schedule.analysis = t(16, 10);
    c.risk.max_gross_exposure = 1.5;
    let errs = c.validate().unwrap_err();
    assert_eq!(errs.len(), 4, "{errs:?}");
    assert!(errs.iter().any(|e| e.contains("allocation")), "{errs:?}");
    assert!(
        errs.iter().any(|e| e.contains("approval_deadline")),
        "{errs:?}"
    );
    assert!(errs.iter().any(|e| e.contains("analysis")), "{errs:?}");
    assert!(
        errs.iter().any(|e| e.contains("max_gross_exposure")),
        "{errs:?}"
    );
}

#[test]
fn validate_checks_each_rule() {
    fn bad(f: impl FnOnce(&mut EngineConfig), needle: &str) {
        let mut c = EngineConfig::default();
        f(&mut c);
        let errs = c.validate().unwrap_err();
        assert_eq!(errs.len(), 1, "{needle}: {errs:?}");
        assert!(errs[0].contains(needle), "{needle}: {errs:?}");
    }
    bad(|c| c.risk.risk_per_trade = 0.0, "risk_per_trade");
    bad(|c| c.risk.max_position_pct = 1.1, "max_position_pct");
    bad(|c| c.risk.max_sector_pct = 0.0, "max_sector_pct");
    bad(|c| c.risk.daily_loss_limit = -0.1, "daily_loss_limit");
    bad(|c| c.risk.weekly_loss_limit = 2.0, "weekly_loss_limit");
    bad(|c| c.risk.price_sanity_pct = 0.0, "price_sanity_pct");
    bad(|c| c.risk.max_gap = f64::NAN, "max_gap");
    bad(|c| c.risk.max_open_positions = 0, "max_open_positions");
    bad(|c| c.risk.max_orders_per_day = 0, "max_orders_per_day");
    bad(
        |c| c.schedule.submit_window_end = t(9, 29),
        "submit_window_end",
    );
    bad(
        |c| c.schedule.submit_window_start = t(9, 30),
        "submit_window_start",
    );
    bad(
        |c| c.schedule.reconcile_every_minutes = 4,
        "reconcile_every_minutes",
    );
    bad(
        |c| c.news_filter.daily_budget_usd = -1.0,
        "daily_budget_usd",
    );
    bad(
        |c| c.news_filter.monthly_budget_usd = -1.0,
        "monthly_budget_usd",
    );
    bad(|c| c.news_filter.model = " ".into(), "model");
    bad(|c| c.news_filter.effort = "max".into(), "effort");
    bad(|c| c.ai.model = " ".into(), "ai.model");
    bad(|c| c.ai.effort = "max".into(), "ai.effort");
    bad(|c| c.ai.context_bars = 10, "ai.context_bars");
    bad(|c| c.ai.max_proposals = 0, "ai.max_proposals");
    bad(|c| c.ai.lookback_days = 31, "ai.lookback_days");
    bad(|c| c.ai.allocation = 0.0, "ai.allocation");
    // Вага поза діапазоном дає два повідомлення: діапазон і сума.
    {
        let mut c = EngineConfig::default();
        c.portfolio.targets.insert("SPY".into(), 1.5);
        let errs = c.validate().unwrap_err();
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(
            errs.iter().any(|e| e.contains("portfolio.targets.SPY")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("сума цільових ваг")),
            "{errs:?}"
        );
    }
    {
        let mut c = EngineConfig::default();
        c.portfolio.targets.insert("SPY".into(), 0.0);
        let errs = c.validate().unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("portfolio.targets.SPY"), "{errs:?}");
    }
    bad(
        |c| {
            c.portfolio.targets.insert("SPY".into(), 0.6);
            c.portfolio.targets.insert("AAPL".into(), 0.6);
        },
        "сума цільових ваг",
    );
    bad(|c| c.portfolio.drift = 0.0, "portfolio.drift");
    bad(|c| c.portfolio.drift = 0.6, "portfolio.drift");
    bad(
        |c| c.portfolio.min_order_usd = 0.5,
        "portfolio.min_order_usd",
    );
    bad(
        |c| c.portfolio.enabled = true,
        "rebalance = monthly або quarterly",
    );
    bad(|c| c.api_port = 80, "api_port");
    bad(|c| c.refresh_window_days = 449, "refresh_window_days");
}

#[test]
fn validate_accepts_boundaries() {
    let mut c = EngineConfig::default();
    c.schedule.analysis = t(16, 16);
    c.schedule.submit_window_end = t(9, 28);
    c.schedule.reconcile_every_minutes = 5;
    c.api_port = 1024;
    c.refresh_window_days = 450;
    c.risk.max_gross_exposure = 1.0;
    c.validate().unwrap();
}

#[test]
fn portfolio_config_roundtrips_toml() {
    let toml_src = r#"
[portfolio]
enabled = true
rebalance = "monthly"
drift = 0.03
min_order_usd = 250

[portfolio.targets]
SPY = 0.5
AAPL = 0.2
"#;
    let c: EngineConfig = toml::from_str(toml_src).unwrap();
    assert!(c.portfolio.enabled);
    assert_eq!(c.portfolio.rebalance, RebalanceMode::Monthly);
    assert_eq!(c.portfolio.targets.get("SPY"), Some(&0.5));
    assert_eq!(c.portfolio.targets.get("AAPL"), Some(&0.2));
    c.validate().unwrap();

    // Серіалізація назад читається так само.
    let s = toml::to_string_pretty(&c.portfolio).unwrap();
    let back: PortfolioConfig = toml::from_str(&s).unwrap();
    assert_eq!(back, c.portfolio);
}

#[test]
fn discovery_serializes() {
    let d = Discovery {
        port: 47615,
        pid: 42,
        started_at: "2026-11-07T10:00:00Z".parse().unwrap(),
    };
    let s = serde_json::to_string(&d).unwrap();
    assert!(
        s.contains("\"port\":47615") && s.contains("\"pid\":42"),
        "{s}"
    );
}
