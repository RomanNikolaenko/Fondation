//! Фільтри входу: звіти (Finnhub) і новини (LLM-суддя) з кешем і бюджетом.
//!
//! Фільтри не повертають помилок: недоступне джерело (зокрема збій читання зі сховища)
//! обробляється за політикою `on_unavailable`, а невдалий запис кешу чи витрат лише логується.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use fond_api::config::{CautionPolicy, EngineConfig, UnavailablePolicy};
use fond_core::calendar::{TradingCalendar, et_to_utc, utc_to_et};
use fond_core::filters::{earnings_blocks, llm_cost_usd};
use fond_core::signal::EntrySignal;
use fond_core::traits::{NewsJudgeRequest, NewsVerdict, Verdict};
use fond_core::types::ExecutionMode;
use fond_store::{EventKind, StoreError};
use serde_json::json;

use super::journal;
use crate::deps::Deps;

pub const BUDGET_EXHAUSTED: &str = "Вичерпано бюджет LLM";
pub const CAUTION_BLOCKED: &str = "Новини: обережно (політика режиму авто)";

#[derive(Debug, Clone, PartialEq)]
pub enum FilterOutcome {
    Pass { news: Option<NewsVerdict> },
    Reject(String),
}

const PASS: FilterOutcome = FilterOutcome::Pass { news: None };

/// Подія фільтра в журналі; збій запису лише логується.
async fn note(d: &Deps, s: &EntrySignal, message: String, data: serde_json::Value) {
    if let Err(e) = journal(d, EventKind::Filter, Some(&s.symbol), None, message, data).await {
        tracing::warn!(error = %e, symbol = %s.symbol, "не вдалося записати подію фільтра");
    }
}

/// Джерело недоступне: `Block` відхиляє з причиною, `Allow` пропускає (із записом у журнал).
async fn unavailable(
    d: &Deps,
    policy: UnavailablePolicy,
    s: &EntrySignal,
    reason: String,
) -> FilterOutcome {
    match policy {
        UnavailablePolicy::Block => FilterOutcome::Reject(reason),
        UnavailablePolicy::Allow => {
            let message = format!("{reason}; вхід дозволено політикою");
            note(
                d,
                s,
                message,
                json!({ "reason": reason, "policy": "allow" }),
            )
            .await;
            PASS
        }
    }
}

fn warn_store(what: &str, s: &EntrySignal, e: &StoreError) {
    tracing::warn!(error = %e, symbol = %s.symbol, "не вдалося зберегти {what}");
}

/// Звіт у межах `days_ahead` торгових днів від дня сигналу → відхилення.
pub async fn earnings_filter(
    d: &Deps,
    cfg: &EngineConfig,
    cal: &TradingCalendar,
    s: &EntrySignal,
) -> FilterOutcome {
    let ec = &cfg.earnings_filter;
    if !ec.enabled {
        return PASS;
    }
    let date = s.signal_date;
    let to = date + Duration::days(i64::from(ec.lookahead_calendar_days));
    match d.earnings.next_earnings(&s.symbol, date, to).await {
        Ok(next) => {
            let blocked = earnings_blocks(date, next, cal, ec.days_ahead);
            let saved = d
                .store
                .put_earnings_check(&s.symbol, date, next, blocked, true)
                .await;
            if let Err(e) = saved {
                warn_store("перевірку звітів", s, &e);
            }
            match next {
                Some(day) if blocked => FilterOutcome::Reject(format!(
                    "Звіт {day}: у межах {} торгових днів",
                    ec.days_ahead
                )),
                _ => PASS,
            }
        }
        Err(e) => {
            let blocked = ec.on_unavailable == UnavailablePolicy::Block;
            let saved = d
                .store
                .put_earnings_check(&s.symbol, date, None, blocked, false)
                .await;
            if let Err(err) = saved {
                warn_store("перевірку звітів", s, &err);
            }
            let reason = format!("Фільтр звітів недоступний: {e}");
            unavailable(d, ec.on_unavailable, s, reason).await
        }
    }
}

/// `Block` → відхилення з першою причиною; `Caution` в «авто» з політикою `Block` → відхилення.
fn apply_verdict(cfg: &EngineConfig, v: NewsVerdict) -> FilterOutcome {
    match v.verdict {
        Verdict::Block => {
            let why = v.reasons.first().map_or("заблоковано", String::as_str);
            FilterOutcome::Reject(format!("Новини: {why}"))
        }
        Verdict::Caution
            if cfg.execution_mode == ExecutionMode::Auto
                && cfg.news_filter.caution_in_auto == CautionPolicy::Block =>
        {
            FilterOutcome::Reject(CAUTION_BLOCKED.to_string())
        }
        Verdict::Allow | Verdict::Caution => FilterOutcome::Pass { news: Some(v) },
    }
}

/// Чи лишився бюджет LLM на поточний день і місяць (за ET).
async fn budget_left(d: &Deps, cfg: &EngineConfig, now: DateTime<Utc>) -> Result<bool, StoreError> {
    let nc = &cfg.news_filter;
    let (day_start, month_start) = budget_windows(now);
    let today = utc_to_et(now).date_naive();
    let day_end = et_midnight(today + Duration::days(1));
    let month_end = et_midnight(next_month(today));
    let daily = d.store.llm_spend_between(day_start, day_end).await?;
    let monthly = d.store.llm_spend_between(month_start, month_end).await?;
    Ok(daily < nc.daily_budget_usd && monthly < nc.monthly_budget_usd)
}

/// Вердикт новин: кеш (символ, дата сигналу) → бюджет → новини → суддя.
pub async fn news_filter(
    d: &Deps,
    cfg: &EngineConfig,
    s: &EntrySignal,
    company: &str,
) -> FilterOutcome {
    let nc = &cfg.news_filter;
    if !nc.enabled {
        return PASS;
    }
    let policy = nc.on_unavailable;
    let date = s.signal_date;
    match d.store.news_verdict(&s.symbol, date).await {
        Ok(Some(v)) => return apply_verdict(cfg, v),
        Ok(None) => {}
        Err(e) => {
            let reason = format!("Фільтр новин недоступний: {e}");
            return unavailable(d, policy, s, reason).await;
        }
    }

    let now = d.clock.now();
    match budget_left(d, cfg, now).await {
        Ok(true) => {}
        Ok(false) => return unavailable(d, policy, s, BUDGET_EXHAUSTED.to_string()).await,
        Err(e) => {
            let reason = format!("Фільтр новин недоступний: {e}");
            return unavailable(d, policy, s, reason).await;
        }
    }

    let since = now - Duration::days(i64::from(nc.lookback_days));
    let items = match d.news.news(&s.symbol, since, nc.max_items).await {
        Ok(items) => items,
        Err(e) => return unavailable(d, policy, s, format!("Новини недоступні: {e}")).await,
    };
    let req = NewsJudgeRequest {
        symbol: s.symbol.clone(),
        company: company.to_string(),
        signal_summary: format!("{}: {}", s.strategy, s.explanation),
        items,
    };
    let judgment = match d.judge.judge(&req).await {
        Ok(j) => j,
        Err(e) => {
            let reason = format!("Фільтр новин недоступний: {e}");
            return unavailable(d, policy, s, reason).await;
        }
    };

    let cost = llm_cost_usd(
        judgment.input_tokens,
        judgment.output_tokens,
        nc.input_usd_per_mtok,
        nc.output_usd_per_mtok,
    );
    let usage = d
        .store
        .add_llm_usage(
            now,
            &judgment.model,
            judgment.input_tokens,
            judgment.output_tokens,
            cost,
        )
        .await;
    if let Err(e) = usage {
        warn_store("витрати LLM", s, &e);
    }
    let saved = d
        .store
        .put_news_verdict(&s.symbol, date, &judgment.verdict, &judgment.model, now)
        .await;
    if let Err(e) = saved {
        warn_store("вердикт новин", s, &e);
    }
    apply_verdict(cfg, judgment.verdict)
}

fn et_midnight(date: NaiveDate) -> DateTime<Utc> {
    et_to_utc(date, NaiveTime::MIN)
}

fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

fn next_month(date: NaiveDate) -> NaiveDate {
    first_of_month(first_of_month(date) + Duration::days(32))
}

/// Початок поточного дня і поточного місяця за часом ET, у UTC.
pub fn budget_windows(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = utc_to_et(now).date_naive();
    (et_midnight(today), et_midnight(first_of_month(today)))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate, TimeZone};
    use fond_api::config::UnavailablePolicy;
    use fond_core::error::DataError;
    use fond_core::traits::Clock;
    use fond_core::types::StrategyId;

    use super::*;
    use crate::cycle::test_support::{d, fixture, sym};
    use crate::cycle::trading_calendar;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn signal(date: NaiveDate) -> EntrySignal {
        EntrySignal {
            strategy: StrategyId::new("trend_pullback"),
            symbol: sym("AAPL"),
            signal_date: date,
            reference_price: 100.0,
            stop_price: 95.0,
            take_profit: None,
            strength: 0.5,
            explanation: "тест".into(),
        }
    }

    fn rejected_with(o: &FilterOutcome, prefix: &str) -> bool {
        matches!(o, FilterOutcome::Reject(m) if m.starts_with(prefix))
    }

    #[tokio::test]
    async fn earnings_outside_window_passes_and_inside_rejects() {
        let (dp, h) = fixture().await;
        let cfg = EngineConfig::default();
        let cal = trading_calendar(&dp, h.last_date()).await.unwrap();
        let s = signal(h.last_date());
        // 4-та сесія після D (вівторок 10.11) — уже поза вікном у 3 сесії.
        h.earnings.set(&sym("AAPL"), Ok(Some(d(2026, 11, 10))));
        assert_eq!(
            earnings_filter(&dp, &cfg, &cal, &s).await,
            FilterOutcome::Pass { news: None }
        );
        h.earnings.set(&sym("AAPL"), Ok(Some(d(2026, 11, 9))));
        assert_eq!(
            earnings_filter(&dp, &cfg, &cal, &s).await,
            FilterOutcome::Reject("Звіт 2026-11-09: у межах 3 торгових днів".into())
        );
    }

    #[tokio::test]
    async fn earnings_unavailable_follows_policy() {
        let (dp, h) = fixture().await;
        let cal = trading_calendar(&dp, h.last_date()).await.unwrap();
        let s = signal(h.last_date());
        h.earnings
            .set(&sym("AAPL"), Err(DataError::NotAvailable("premium".into())));
        let mut cfg = EngineConfig::default();
        let o = earnings_filter(&dp, &cfg, &cal, &s).await;
        assert!(rejected_with(&o, "Фільтр звітів недоступний"), "{o:?}");
        cfg.earnings_filter.on_unavailable = UnavailablePolicy::Allow;
        assert_eq!(
            earnings_filter(&dp, &cfg, &cal, &s).await,
            FilterOutcome::Pass { news: None }
        );
    }

    #[tokio::test]
    async fn disabled_filters_pass_without_calls() {
        let (dp, h) = fixture().await;
        let cal = trading_calendar(&dp, h.last_date()).await.unwrap();
        let s = signal(h.last_date());
        let mut cfg = EngineConfig::default();
        cfg.earnings_filter.enabled = false;
        cfg.news_filter.enabled = false;
        assert_eq!(
            earnings_filter(&dp, &cfg, &cal, &s).await,
            FilterOutcome::Pass { news: None }
        );
        assert_eq!(
            news_filter(&dp, &cfg, &s, "Apple Inc.").await,
            FilterOutcome::Pass { news: None }
        );
        assert_eq!(
            (h.earnings.calls(), h.news.calls(), h.judge.calls()),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn news_source_error_follows_policy() {
        let (dp, h) = fixture().await;
        let s = signal(h.last_date());
        let mut cfg = EngineConfig::default();
        h.news.fail_next(DataError::Transient("503".into()));
        let o = news_filter(&dp, &cfg, &s, "Apple Inc.").await;
        assert!(rejected_with(&o, "Новини недоступні"), "{o:?}");
        assert_eq!(h.judge.calls(), 0);

        cfg.news_filter.on_unavailable = UnavailablePolicy::Allow;
        h.news.fail_next(DataError::Transient("503".into()));
        assert_eq!(
            news_filter(&dp, &cfg, &s, "Apple Inc.").await,
            FilterOutcome::Pass { news: None }
        );
    }

    #[tokio::test]
    async fn monthly_budget_exhausted_blocks() {
        let (dp, h) = fixture().await;
        let s = signal(h.last_date());
        let cfg = EngineConfig::default();
        // 2 листопада: цей місяць, але не сьогодні (D = 4 листопада).
        let earlier = h.clock.now() - Duration::days(2);
        dp.store
            .add_llm_usage(earlier, "m", 0, 0, 20.0)
            .await
            .unwrap();
        assert_eq!(
            news_filter(&dp, &cfg, &s, "Apple Inc.").await,
            FilterOutcome::Reject("Вичерпано бюджет LLM".into())
        );
        assert_eq!(h.judge.calls(), 0);
    }

    #[tokio::test]
    async fn news_verdict_is_stored_with_usage() {
        let (dp, h) = fixture().await;
        let s = signal(h.last_date());
        let cfg = EngineConfig::default();
        let o = news_filter(&dp, &cfg, &s, "Apple Inc.").await;
        let FilterOutcome::Pass { news: Some(v) } = o else {
            panic!("очікувався Pass з вердиктом, є {o:?}")
        };
        assert_eq!(v.verdict, fond_core::traits::Verdict::Allow);
        assert_eq!(
            dp.store
                .news_verdict(&sym("AAPL"), h.last_date())
                .await
                .unwrap(),
            Some(v)
        );
        let (day, _) = budget_windows(h.clock.now());
        let spent = dp
            .store
            .llm_spend_between(day, day + Duration::days(1))
            .await
            .unwrap();
        assert!(spent > 0.0);
    }

    #[test]
    fn budget_windows_use_new_york_day_and_month() {
        // 2026-11-07 03:00Z = 2026-11-06 22:00 EST; 1 листопада ще EDT (до 02:00).
        assert_eq!(
            budget_windows(utc(2026, 11, 7, 3, 0)),
            (utc(2026, 11, 6, 5, 0), utc(2026, 11, 1, 4, 0))
        );
        // Літній час: 2026-07-15 12:00Z = 08:00 EDT.
        assert_eq!(
            budget_windows(utc(2026, 7, 15, 12, 0)),
            (utc(2026, 7, 15, 4, 0), utc(2026, 7, 1, 4, 0))
        );
        // Перший день місяця: день і місяць починаються однаково.
        assert_eq!(
            budget_windows(utc(2026, 12, 1, 15, 0)),
            (utc(2026, 12, 1, 5, 0), utc(2026, 12, 1, 5, 0))
        );
    }
}
