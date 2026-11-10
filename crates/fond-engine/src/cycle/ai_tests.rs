//! Тести AI-проходу аналізу: пропозиція → намір, бюджет, санітарні перевірки, помилки.

use fond_api::config::EngineConfig;
use fond_core::ai::{AiAction, AiProposal};
use fond_core::error::JudgeError;
use fond_store::{AiProposalStatus, EventQuery};

use crate::cycle::ai::{self, indicator_summary};
use crate::cycle::analysis::run_analysis;
use crate::cycle::test_support::{d, fixture};
use fond_core::testkit::bars_from_closes;

fn review(proposal: AiProposal) -> Result<fond_core::traits::AiReview, JudgeError> {
    Ok(fond_core::traits::AiReview {
        proposals: vec![proposal],
        model: "claude-haiku-5-5-20261001".into(),
        input_tokens: 2_200,
        output_tokens: 90,
    })
}

fn buy_proposal() -> AiProposal {
    AiProposal {
        symbol: "MSFT".into(),
        action: AiAction::Buy,
        reference_price: 100.0,
        stop_price: 90.0,
        take_profit: Some(115.0),
        strength: 0.7,
        explanation: "Тренд нагору, новини позитивні.".into(),
        key_reasons: vec!["Ціна над опорою.".into()],
    }
}

fn ai_cfg() -> EngineConfig {
    let mut cfg = EngineConfig::default();
    cfg.ai.enabled = true;
    cfg
}

fn window(
    dp: &crate::deps::Deps,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    (
        dp.clock.now() - chrono::Duration::hours(1),
        dp.clock.now() + chrono::Duration::hours(1),
    )
}

#[tokio::test]
async fn ai_proposal_becomes_pending_intent_and_record() {
    let (dp, h) = fixture().await;
    h.strategist.set_results(vec![review(buy_proposal())]);

    let date = h.last_date();
    let s = run_analysis(&dp, &ai_cfg(), date).await.unwrap();
    assert!(
        s.entries_created >= 1,
        "щонайменше одна AI-пропозиція дошла до наміру: {s:?}"
    );

    let intents = dp.store.intents_for_session(d(2026, 11, 5)).await.unwrap();
    let ai_intents: Vec<_> = intents
        .iter()
        .filter(|i| i.strategy.as_str() == ai::AI_STRATEGY)
        .collect();
    assert!(
        !ai_intents.is_empty(),
        "намір від стратегії ai повинен існувати"
    );

    // Журнал ai_proposals: записи зі статусом IntentCreated (MSFT і SPY).
    let (from, to) = window(&dp);
    let records = dp.store.ai_proposals_between(from, to).await.unwrap();
    assert_eq!(records.len(), 2, "записів на MSFT і SPY");
    assert_eq!(records[0].model, "claude-haiku-5-5-20261001");
    assert!(
        records
            .iter()
            .all(|r| r.status == AiProposalStatus::IntentCreated)
    );

    // Витрати LLM записані (спільний бюджет із фільтром новин).
    let spend = dp.store.llm_spend_between(from, to).await.unwrap();
    assert!(spend > 0.0, "витрати LLM мають бути записані");
}

#[tokio::test]
async fn exhausted_budget_skips_strategist() {
    let (dp, h) = fixture().await;
    h.strategist.set_results(vec![review(buy_proposal())]);
    dp.store
        .add_llm_usage(dp.clock.now(), "m", 0, 0, 1_000.0)
        .await
        .unwrap();

    let s = run_analysis(&dp, &ai_cfg(), h.last_date()).await.unwrap();
    assert_eq!(s.entries_created, 0, "AI-пропозицій немає");
    assert_eq!(h.strategist.calls(), 0, "стратегіста не викликали");
    let (from, to) = window(&dp);
    assert!(
        dp.store
            .ai_proposals_between(from, to)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn malformed_proposal_is_dropped_without_signal() {
    let (dp, h) = fixture().await;
    let mut bad = buy_proposal();
    bad.stop_price = 110.0; // вище ціни для купівлі
    h.strategist.set_results(vec![review(bad)]);

    run_analysis(&dp, &ai_cfg(), h.last_date()).await.unwrap();
    // Технічний сигнал AAPL законно створює намір; AI-намірів бути не має.
    let intents = dp.store.intents_for_session(d(2026, 11, 5)).await.unwrap();
    assert!(
        !intents
            .iter()
            .any(|i| i.strategy.as_str() == ai::AI_STRATEGY),
        "невалідна AI-пропозиція не створює намірів"
    );
    let (from, to) = window(&dp);
    assert!(
        dp.store
            .ai_proposals_between(from, to)
            .await
            .unwrap()
            .is_empty(),
        "невалідна пропозиція не записується"
    );
    let events = dp.store.events(&EventQuery::default()).await.unwrap();
    assert!(
        events.iter().any(|e| e.message.contains("санітарними")),
        "має бути подія санітарного відхилення"
    );
}

#[tokio::test]
async fn strategist_error_is_journaled_and_does_not_break_analysis() {
    let (dp, h) = fixture().await;
    h.strategist
        .set_results(vec![Err(JudgeError::Unauthorized)]);

    let s = run_analysis(&dp, &ai_cfg(), h.last_date()).await;
    assert!(s.is_ok(), "помилка стратегіста не ламає аналіз: {s:?}");
    let events = dp.store.events(&EventQuery::default()).await.unwrap();
    assert!(events.iter().any(|e| e.message.contains("не відповів")));
}

/// Контекст містить лімітовані бари, індикатори, новини і позицію.
#[tokio::test]
async fn strategist_context_has_bars_indicators() {
    let (dp, h) = fixture().await;
    h.strategist.set_results(vec![review(buy_proposal())]);

    let mut cfg = ai_cfg();
    cfg.ai.context_bars = 20;
    run_analysis(&dp, &cfg, h.last_date()).await.unwrap();

    let ctxs = h.strategist.contexts();
    assert!(!ctxs.is_empty());
    let msft = ctxs
        .iter()
        .find(|c| c.symbol.as_str() == "MSFT")
        .expect("MSFT у контексті");
    assert!(msft.bars.len() <= 20);
    assert_eq!(msft.company, "Microsoft Corp.");
    assert!(
        msft.indicator_summary.contains("SMA"),
        "індикатори: {}",
        msft.indicator_summary
    );
    assert!(msft.position.is_none(), "позиції немає");
}

#[test]
fn indicator_summary_includes_key_metrics() {
    let bars = bars_from_closes(
        d(2026, 10, 1),
        &[100.0, 101.0, 102.0, 103.0, 104.0],
        1_000_000.0,
    );
    let text = indicator_summary(&bars);
    assert!(text.contains("Останній close: 104.00"));
}
