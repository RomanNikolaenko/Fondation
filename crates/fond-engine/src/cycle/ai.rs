//! AI-аналітик: прогін Claude по кандидатах скана після технічного аналізу.
//!
//! Пропозиції AI перетворюються на сигнали зі `StrategyId("ai")` і надходять у
//! той самий ризик-конвеєр. Новинний і звітний фільтри для AI-сигналів не
//! викликаються: модель уже бачила і новини, і дату звіту в контексті.

use std::collections::HashMap;

use chrono::Duration;
use fond_core::ai::proposal_is_well_formed;
use fond_core::filters::llm_cost_usd;
use fond_core::indicators::{
    atr_wilder_last, avg_volume, highest_high, lowest_low, rsi_wilder_last, sma_last,
};
use fond_core::signal::EntrySignal;
use fond_core::traits::{AiPositionBrief, AiTickerContext, NewsItem};
use fond_core::types::StrategyId;
use fond_store::{EventKind, NewAiProposal};
use serde_json::json;

use super::analysis::Run;
use super::filters::budget_left;
use super::{CycleError, journal};
use crate::deps::Deps;

pub(crate) const AI_STRATEGY: &str = "ai";

pub(crate) struct AiPassResult {
    /// Сигнали, готові до ризик-менеджера (у порядку виклику AI).
    pub candidates: Vec<fond_core::risk::Candidate>,
    /// Актив id записів `ai_proposals`, узгоджений з `candidates`.
    pub ids: Vec<i64>,
}

pub(crate) fn indicator_summary(bars: &[fond_core::types::Bar]) -> String {
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut lines = Vec::new();
    let last_close = bars.last().map(|b| b.close);
    if let Some(c) = last_close {
        lines.push(format!("Останній close: {c:.2}"));
    }
    if let Some(s) = sma_last(&closes, 20) {
        lines.push(format!("SMA20: {s:.2}"));
    }
    if let Some(s) = sma_last(&closes, 50) {
        lines.push(format!("SMA50: {s:.2}"));
    }
    if let Some(r) = rsi_wilder_last(&closes, 14) {
        lines.push(format!("RSI14: {r:.1}"));
    }
    if let Some(a) = atr_wilder_last(bars, 14) {
        lines.push(format!("ATR14: {a:.2}"));
    }
    if let Some(v) = avg_volume(bars, 20) {
        lines.push(format!("Середній обсяг 20д: {v:.0}"));
    }
    if let Some(h) = highest_high(bars, 20) {
        let Some(l) = lowest_low(bars, 20) else {
            return lines.join("\n");
        };
        lines.push(format!("Діапазон 20д: {l:.2}–{h:.2}"));
    }
    lines.join("\n")
}

async fn position_brief(d: &Deps, sym: &str) -> Option<AiPositionBrief> {
    let list = d.store.positions().await.ok()?;
    let p = list
        .iter()
        .find(|p| p.position.symbol.as_str().eq_ignore_ascii_case(sym))?;
    Some(AiPositionBrief {
        qty: f64::from(p.position.qty),
        avg_cost: p.position.entry_price,
        stop_price: Some(p.position.stop_price),
        take_profit: p.position.take_profit,
    })
}

impl Run<'_> {
    /// Прогін AI-стратегіста: контекст → пропозиції → журналювання → кандидати.
    /// Повертає записані пропозиції (готові до ризик-менеджера) та їх id.
    pub(crate) async fn ai_pass(
        &self,
        scan: &super::analysis::Scanned,
        candidates: &[&fond_core::types::Symbol],
        names: &HashMap<&fond_core::types::Symbol, &fond_store::UniverseEntry>,
    ) -> Result<AiPassResult, CycleError> {
        let mut out = AiPassResult {
            candidates: Vec::new(),
            ids: Vec::new(),
        };
        if !self.cfg.ai.enabled || candidates.is_empty() {
            return Ok(out);
        }

        let now = self.d.clock.now();
        if !budget_left(self.d, self.cfg, now).await? {
            journal(
                self.d,
                EventKind::Cycle,
                None,
                None,
                "AI-аналітик пропущено: вичерпано бюджет LLM",
                json!({ "step": "ai_review", "reason": "бюджет" }),
            )
            .await?;
            return Ok(out);
        }

        let take = self.cfg.ai.max_proposals.min(candidates.len());
        let since = now - Duration::days(i64::from(self.cfg.news_filter.lookback_days));
        let news_limit = self.cfg.news_filter.max_items;
        let nc = &self.cfg.news_filter;

        for sym in candidates.iter().take(take) {
            let bars_full = match scan.bars.get(*sym) {
                Some(b) if !b.is_empty() => b,
                _ => continue,
            };
            let skip = bars_full.len().saturating_sub(self.cfg.ai.context_bars);
            let bars = &bars_full[skip.min(bars_full.len())..];
            let company = names
                .get(*sym)
                .map_or(sym.as_str(), |e| e.name.as_str())
                .to_string();
            let next_earnings = self
                .d
                .earnings
                .next_earnings(
                    sym,
                    self.date + Duration::days(1),
                    self.date
                        + Duration::days(i64::from(
                            self.cfg.earnings_filter.lookahead_calendar_days,
                        )),
                )
                .await
                .ok()
                .flatten();
            let news: Vec<NewsItem> = self
                .d
                .news
                .news(sym, since, news_limit)
                .await
                .unwrap_or_default();
            let ctx = AiTickerContext {
                symbol: (*sym).clone(),
                company,
                bars: bars.to_vec(),
                indicator_summary: indicator_summary(bars),
                position: position_brief(self.d, sym.as_str()).await,
                next_earnings,
                news,
            };
            let review = match self.d.strategist.analyze(&ctx).await {
                Ok(r) => r,
                Err(e) => {
                    journal(
                        self.d,
                        EventKind::Error,
                        Some(sym),
                        None,
                        format!("AI-аналітик не відповів: {e}"),
                        json!({ "step": "ai_review", "symbol": sym.as_str() }),
                    )
                    .await?;
                    continue;
                }
            };
            let cost = llm_cost_usd(
                review.input_tokens,
                review.output_tokens,
                nc.input_usd_per_mtok,
                nc.output_usd_per_mtok,
            );
            if let Err(e) = self
                .d
                .store
                .add_llm_usage(
                    now,
                    &review.model,
                    review.input_tokens,
                    review.output_tokens,
                    cost,
                )
                .await
            {
                journal(
                    self.d,
                    EventKind::Error,
                    Some(sym),
                    None,
                    format!("Не вдалося зберегти витрати LLM: {e}"),
                    json!({ "step": "ai_review", "symbol": sym.as_str() }),
                )
                .await?;
            }

            for p in review.proposals {
                let explanation = p.explanation.clone();
                if !proposal_is_well_formed(&p) {
                    journal(
                        self.d,
                        EventKind::Signal,
                        Some(sym),
                        None,
                        "AI-пропозицію відхилено санітарними перевірками",
                        json!({ "step": "ai_review", "symbol": sym.as_str(), "proposal": p }),
                    )
                    .await?;
                    continue;
                }
                let record_id = self
                    .d
                    .store
                    .add_ai_proposal(&NewAiProposal {
                        at: now,
                        run_date: self.date,
                        proposal: p.clone(),
                        model: review.model.clone(),
                        input_tokens: i64::try_from(review.input_tokens).unwrap_or(0),
                        output_tokens: i64::try_from(review.output_tokens).unwrap_or(0),
                        cost_usd: cost,
                    })
                    .await?;
                let action_word = match p.action {
                    fond_core::ai::AiAction::Buy => "купівля",
                    fond_core::ai::AiAction::Exit => "вихід",
                };
                let message = format!(
                    "AI-пропозиція: {action_word} {sym} (стоп {:.2}, сила {:.2}, {explanation})",
                    p.stop_price, p.strength
                );
                let signal = EntrySignal {
                    strategy: StrategyId::new(AI_STRATEGY),
                    symbol: (*sym).clone(),
                    signal_date: self.date,
                    reference_price: p.reference_price,
                    stop_price: p.stop_price,
                    take_profit: p.take_profit,
                    strength: p.strength,
                    explanation,
                };
                out.candidates.push(fond_core::risk::Candidate {
                    signal,
                    sector: super::analysis::sector_of(names.get(*sym).copied()),
                });
                out.ids.push(record_id);
                journal(
                    self.d,
                    EventKind::Signal,
                    Some(sym),
                    None,
                    message,
                    json!({
                        "step": "ai_review",
                        "symbol": sym.as_str(),
                        "strategy": AI_STRATEGY,
                        "stop": p.stop_price,
                        "take_profit": p.take_profit,
                        "strength": p.strength,
                        "record_id": record_id,
                    }),
                )
                .await?;
            }
        }
        Ok(out)
    }

    /// Після persist_decisions: узгоджує статуси `ai_proposals` конвеєром:
    /// запис сигналу зі `strategy = ai` за тікером визначає підсумок.
    pub(crate) async fn ai_finalize(
        &self,
        proposals: &[(&i64, &fond_core::types::Symbol)],
    ) -> Result<(), CycleError> {
        if proposals.is_empty() {
            return Ok(());
        }
        let signals = self.d.store.signals_on(self.date).await?;
        for (id, symbol) in proposals {
            let record = signals
                .iter()
                .find(|s| s.signal.strategy.as_str() == AI_STRATEGY && s.signal.symbol == **symbol);
            match record {
                Some(r) => match r.status {
                    fond_store::SignalStatus::Accepted => {
                        self.d
                            .store
                            .set_ai_proposal_status(
                                **id,
                                fond_store::AiProposalStatus::IntentCreated,
                                None,
                            )
                            .await?;
                    }
                    fond_store::SignalStatus::Rejected => {
                        self.d
                            .store
                            .set_ai_proposal_status(
                                **id,
                                fond_store::AiProposalStatus::RiskRejected,
                                r.reject_reason.as_deref(),
                            )
                            .await?;
                    }
                },
                None => {
                    journal(
                        self.d,
                        EventKind::Error,
                        Some(symbol),
                        None,
                        format!("AI-пропозиція id={id} не потрапила в сигнали"),
                        json!({ "step": "ai_finalize", "symbol": symbol.as_str() }),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}
