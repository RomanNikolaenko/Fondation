//! Оновлення ринкових даних: свічки (вікно перезаписується), активи, календар.

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDate};
use fond_api::config::EngineConfig;
use fond_core::types::Symbol;
use fond_store::EventKind;
use serde_json::json;

use super::{CALENDAR_SPAN_DAYS, CycleError, journal};
use crate::deps::Deps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshSummary {
    pub symbols: usize,
    pub bars: usize,
}

/// Початок вікна свічок: `date − refresh_window_days` календарних днів.
pub fn window_start(cfg: &EngineConfig, date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(cfg.refresh_window_days))
}

/// Символи = universe (без excluded) ∪ символ ринкового фільтра ∪ утримувані; вікно
/// `[date − refresh_window_days, date]` перезаписується. Також `assets()` і
/// `calendar(date − 40, date + 40)`.
pub async fn refresh_market_data(
    d: &Deps,
    cfg: &EngineConfig,
    date: NaiveDate,
) -> Result<RefreshSummary, CycleError> {
    let symbols = refresh_symbols(d, cfg).await?;
    let from = window_start(cfg, date);
    let fetched = d.data.daily_bars(&symbols, from, date).await?;
    let mut bars = 0;
    for s in &symbols {
        if let Some(b) = fetched.get(s) {
            bars += d.store.upsert_bars(s, b).await?;
        }
    }

    let assets = d.data.assets().await?;
    d.store.upsert_assets(&assets, d.clock.now()).await?;

    let span = Duration::days(CALENDAR_SPAN_DAYS);
    let sessions = d.data.calendar(date - span, date + span).await?;
    d.store.upsert_sessions(&sessions).await?;

    let summary = RefreshSummary {
        symbols: symbols.len(),
        bars,
    };
    journal(
        d,
        EventKind::Cycle,
        None,
        None,
        format!(
            "Оновлено дані: {} тікерів, {} свічок з {from}",
            summary.symbols, summary.bars
        ),
        json!({
            "date": date,
            "from": from,
            "symbols": summary.symbols,
            "bars": summary.bars,
            "assets": assets.len(),
            "sessions": sessions.len(),
        }),
    )
    .await?;
    Ok(summary)
}

/// Відсортований набір без дублікатів.
async fn refresh_symbols(d: &Deps, cfg: &EngineConfig) -> Result<Vec<Symbol>, CycleError> {
    let mut set: BTreeSet<Symbol> = d
        .store
        .universe(false)
        .await?
        .into_iter()
        .map(|e| e.symbol)
        .collect();
    set.insert(cfg.strategies.market_filter.symbol.clone());
    for p in d.store.positions().await? {
        set.insert(p.position.symbol);
    }
    Ok(set.into_iter().collect())
}
