//! Контракт конфігурації рушія (`config.toml`) і файлу виявлення (`engine.json`).

use chrono::{DateTime, NaiveTime, Utc};
use fond_backtest::CostModel;
use fond_core::risk::RiskConfig;
use fond_core::scanner::ScannerConfig;
use fond_core::types::ExecutionMode;
use fond_strategies::StrategiesConfig;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Що робити з новиною-«обережно» в автоматичному режимі.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CautionPolicy {
    Allow,
    Block,
}

/// Що робити, коли фільтр недоступний.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum UnavailablePolicy {
    Block,
    Allow,
}

/// Час доби у форматі `HH:MM` (читається також `HH:MM:SS`).
mod hhmm {
    use chrono::{NaiveTime, Timelike};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &NaiveTime, s: S) -> Result<S::Ok, S::Error> {
        if t.second() == 0 {
            s.collect_str(&t.format("%H:%M"))
        } else {
            s.collect_str(&t.format("%H:%M:%S"))
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveTime, D::Error> {
        let raw = String::deserialize(d)?;
        NaiveTime::parse_from_str(&raw, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(&raw, "%H:%M:%S"))
            .map_err(|_| {
                serde::de::Error::custom(format!(
                    "очікується HH:MM або HH:MM:SS, отримано \"{raw}\""
                ))
            })
    }
}

/// Розклад у часі `America/New_York`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ScheduleConfig {
    #[serde(with = "hhmm")]
    #[ts(type = "string")]
    pub analysis: NaiveTime,
    #[serde(with = "hhmm")]
    #[ts(type = "string")]
    pub approval_deadline: NaiveTime,
    #[serde(with = "hhmm")]
    #[ts(type = "string")]
    pub submit_window_start: NaiveTime,
    #[serde(with = "hhmm")]
    #[ts(type = "string")]
    pub submit_window_end: NaiveTime,
    pub reconcile_every_minutes: u32,
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("коректний час")
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            analysis: hm(16, 30),
            approval_deadline: hm(8, 55),
            submit_window_start: hm(9, 0),
            submit_window_end: hm(9, 25),
            reconcile_every_minutes: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct NewsFilterConfig {
    pub enabled: bool,
    pub model: String,
    pub effort: String,
    pub lookback_days: u32,
    pub max_items: usize,
    pub caution_in_auto: CautionPolicy,
    pub on_unavailable: UnavailablePolicy,
    pub daily_budget_usd: f64,
    pub monthly_budget_usd: f64,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
}

impl Default for NewsFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "claude-haiku-5-5".into(),
            effort: "low".into(),
            lookback_days: 7,
            max_items: 20,
            caution_in_auto: CautionPolicy::Allow,
            on_unavailable: UnavailablePolicy::Block,
            daily_budget_usd: 1.0,
            monthly_budget_usd: 20.0,
            input_usd_per_mtok: 0.10,
            output_usd_per_mtok: 0.50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct AiReviewConfig {
    pub enabled: bool,
    pub model: String,
    pub effort: String,
    /// Скільки останніх барів передавати моделі.
    pub context_bars: usize,
    /// Максимум пропозицій (символів) за один прогін.
    pub max_proposals: usize,
    /// Ліміт пропозицій, що можуть завершитися намірами за один прогін.
    pub lookback_days: u32,
    /// Максимальна частка вартості позицій стратегії `ai` від капіталу.
    pub allocation: f64,
}

impl Default for AiReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "claude-haiku-5-5".into(),
            effort: "low".into(),
            context_bars: 90,
            max_proposals: 5,
            lookback_days: 7,
            allocation: 0.25,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct EarningsFilterConfig {
    pub enabled: bool,
    pub days_ahead: usize,
    pub lookahead_calendar_days: u32,
    pub on_unavailable: UnavailablePolicy,
}

impl Default for EarningsFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            days_ahead: 3,
            lookahead_calendar_days: 10,
            on_unavailable: UnavailablePolicy::Block,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct UiConfig {
    pub disclaimer_accepted_at: Option<DateTime<Utc>>,
    pub notifications_enabled: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            disclaimer_accepted_at: None,
            notifications_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct EngineConfig {
    pub execution_mode: ExecutionMode,
    pub api_port: u16,
    pub refresh_window_days: u32,
    pub schedule: ScheduleConfig,
    pub scanner: ScannerConfig,
    pub risk: RiskConfig,
    pub strategies: StrategiesConfig,
    pub news_filter: NewsFilterConfig,
    pub ai: AiReviewConfig,
    pub earnings_filter: EarningsFilterConfig,
    pub backtest: CostModel,
    pub ui: UiConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            execution_mode: ExecutionMode::Approval,
            api_port: 47615,
            refresh_window_days: 600,
            schedule: ScheduleConfig::default(),
            scanner: ScannerConfig::default(),
            risk: RiskConfig::default(),
            strategies: StrategiesConfig::default(),
            news_filter: NewsFilterConfig::default(),
            ai: AiReviewConfig::default(),
            earnings_filter: EarningsFilterConfig::default(),
            backtest: CostModel::default(),
            ui: UiConfig::default(),
        }
    }
}

/// `v` у проміжку (0, 1]; NaN не проходить.
fn in_unit(v: f64) -> bool {
    v > 0.0 && v <= 1.0
}

impl EngineConfig {
    /// Збирає всі порушення (по одному повідомленню на кожне), а не лише перше.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs: Vec<String> = Vec::new();
        let r = &self.risk;

        for (name, v) in [
            ("risk_per_trade", r.risk_per_trade),
            ("max_position_pct", r.max_position_pct),
            ("max_sector_pct", r.max_sector_pct),
            ("max_gross_exposure", r.max_gross_exposure),
            ("daily_loss_limit", r.daily_loss_limit),
            ("weekly_loss_limit", r.weekly_loss_limit),
            ("price_sanity_pct", r.price_sanity_pct),
            ("max_gap", r.max_gap),
        ] {
            if !in_unit(v) {
                errs.push(format!("risk.{name} має бути в межах (0, 1], зараз {v}"));
            }
        }
        if r.max_open_positions < 1 {
            errs.push("risk.max_open_positions має бути не менше 1".into());
        }
        if r.max_orders_per_day < 1 {
            errs.push("risk.max_orders_per_day має бути не менше 1".into());
        }

        if let Err(msgs) = self.strategies.validate() {
            errs.extend(msgs);
        }

        let s = &self.schedule;
        if s.approval_deadline >= s.submit_window_start {
            errs.push("schedule.approval_deadline має бути раніше за submit_window_start".into());
        }
        if s.submit_window_start >= s.submit_window_end {
            errs.push("schedule.submit_window_start має бути раніше за submit_window_end".into());
        }
        if s.submit_window_end > hm(9, 28) {
            errs.push("schedule.submit_window_end має бути не пізніше 09:28".into());
        }
        if s.analysis < hm(16, 16) {
            errs.push("schedule.analysis має бути не раніше 16:16".into());
        }
        if s.reconcile_every_minutes < 5 {
            errs.push("schedule.reconcile_every_minutes має бути не менше 5".into());
        }

        let n = &self.news_filter;
        for (name, v) in [
            ("daily_budget_usd", n.daily_budget_usd),
            ("monthly_budget_usd", n.monthly_budget_usd),
            ("input_usd_per_mtok", n.input_usd_per_mtok),
            ("output_usd_per_mtok", n.output_usd_per_mtok),
        ] {
            if !v.is_finite() || v < 0.0 {
                errs.push(format!("news_filter.{name} має бути не менше 0, зараз {v}"));
            }
        }
        if n.model.trim().is_empty() {
            errs.push("news_filter.model не може бути порожньою".into());
        }
        if !matches!(n.effort.as_str(), "low" | "medium" | "high") {
            errs.push(format!(
                "news_filter.effort має бути low, medium або high, зараз \"{}\"",
                n.effort
            ));
        }

        let a = &self.ai;
        if a.model.trim().is_empty() {
            errs.push("ai.model не може бути порожнім".into());
        }
        if !matches!(a.effort.as_str(), "low" | "medium" | "high") {
            errs.push(format!(
                "ai.effort має бути low, medium або high, зараз \"{}\"",
                a.effort
            ));
        }
        if !(20..=250).contains(&a.context_bars) {
            errs.push(format!(
                "ai.context_bars має бути в межах 20..=250, зараз {}",
                a.context_bars
            ));
        }
        if !(1..=20).contains(&a.max_proposals) {
            errs.push(format!(
                "ai.max_proposals має бути в межах 1..=20, зараз {}",
                a.max_proposals
            ));
        }
        if a.lookback_days < 1 || a.lookback_days > 30 {
            errs.push(format!(
                "ai.lookback_days має бути в межах 1..=30, зараз {}",
                a.lookback_days
            ));
        }
        if !in_unit(a.allocation) {
            errs.push(format!(
                "ai.allocation має бути в межах (0, 1], зараз {}",
                a.allocation
            ));
        }

        if self.api_port < 1024 {
            errs.push(format!(
                "api_port має бути не менше 1024, зараз {}",
                self.api_port
            ));
        }
        if self.refresh_window_days < 450 {
            errs.push(format!(
                "refresh_window_days має бути не менше 450, зараз {}",
                self.refresh_window_days
            ));
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}

/// Вміст `engine.json`: як UI знаходить запущений рушій.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Discovery {
    pub port: u16,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests;
