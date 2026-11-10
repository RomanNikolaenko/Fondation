use chrono::{NaiveDate, TimeZone, Utc};
use fond_core::ai::{AiAction, AiProposal};
use fond_store::{AiProposalStatus, NewAiProposal, Store};

fn at(day: u32, h: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 2, day, h, 0, 0).unwrap()
}

fn new_proposal(sym: &str, action: AiAction) -> NewAiProposal {
    NewAiProposal {
        at: at(10, 17),
        run_date: NaiveDate::from_ymd_opt(2026, 2, 10).unwrap(),
        proposal: AiProposal {
            symbol: sym.into(),
            action,
            reference_price: 400.0,
            stop_price: 385.0,
            take_profit: Some(440.0),
            strength: 0.72,
            explanation: "Тренд нагору, новини позитивні.".into(),
            key_reasons: vec!["Ціна над опорою.".into(), "Сильні зведення.".into()],
        },
        model: "claude-haiku-5-5-20261001".into(),
        input_tokens: 2200,
        output_tokens: 90,
        cost_usd: 0.01,
    }
}

#[tokio::test]
async fn insert_list_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let id = s
        .add_ai_proposal(&new_proposal("MSFT", AiAction::Buy))
        .await
        .unwrap();
    assert!(id > 0);

    let rows = s.ai_proposals_between(at(9, 0), at(11, 0)).await.unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.id, id);
    assert_eq!(r.proposal.symbol, "MSFT");
    assert_eq!(r.proposal.action, AiAction::Buy);
    assert_eq!(r.proposal.reference_price, 400.0);
    assert_eq!(r.proposal.stop_price, 385.0);
    assert_eq!(r.proposal.take_profit, Some(440.0));
    assert_eq!(r.proposal.strength, 0.72);
    assert_eq!(r.proposal.key_reasons.len(), 2);
    assert_eq!(r.model, "claude-haiku-5-5-20261001");
    assert_eq!(r.input_tokens, 2200);
    assert_eq!(r.output_tokens, 90);
    assert_eq!(r.cost_usd, 0.01);
    assert_eq!(r.status, AiProposalStatus::Recorded);
    assert_eq!(r.reject_reason, None);
    assert_eq!(r.run_date, NaiveDate::from_ymd_opt(2026, 2, 10).unwrap());

    // За межами вікна — порожньо.
    assert!(
        s.ai_proposals_between(at(11, 0), at(12, 0))
            .await
            .unwrap()
            .is_empty()
    );
    // Полуінтервал [from, to): запис на межі не входить.
    assert!(
        s.ai_proposals_between(at(11, 0), at(13, 0))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.ai_proposals_between(at(10, 17), at(12, 0))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn status_transitions_persisted() {
    let s = Store::open_in_memory().await.unwrap();
    let id = s
        .add_ai_proposal(&new_proposal("NVDA", AiAction::Buy))
        .await
        .unwrap();

    let ok = s
        .set_ai_proposal_status(id, AiProposalStatus::RiskRejected, Some("грос-ліміт"))
        .await
        .unwrap();
    assert!(ok);
    let rows = s.ai_proposals_between(at(10, 0), at(11, 0)).await.unwrap();
    assert_eq!(rows[0].status, AiProposalStatus::RiskRejected);
    assert_eq!(rows[0].reject_reason.as_deref(), Some("грос-ліміт"));

    let ok = s
        .set_ai_proposal_status(id, AiProposalStatus::IntentCreated, None)
        .await
        .unwrap();
    assert!(ok);
    let rows = s.ai_proposals_between(at(10, 0), at(11, 0)).await.unwrap();
    assert_eq!(rows[0].status, AiProposalStatus::IntentCreated);
    assert_eq!(rows[0].reject_reason, None);

    // Невідомий id → false.
    assert!(
        !s.set_ai_proposal_status(9999, AiProposalStatus::IntentCreated, None)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn exit_proposals_stored_without_take_profit() {
    let s = Store::open_in_memory().await.unwrap();
    let p = new_proposal("AAPL", AiAction::Exit);
    let p = NewAiProposal {
        proposal: AiProposal {
            take_profit: None,
            stop_price: 205.0,
            ..p.proposal
        },
        ..p
    };
    s.add_ai_proposal(&p).await.unwrap();
    let rows = s.ai_proposals_between(at(10, 0), at(11, 0)).await.unwrap();
    assert_eq!(rows[0].proposal.action, AiAction::Exit);
    assert_eq!(rows[0].proposal.take_profit, None);
}

#[tokio::test]
async fn ordering_by_at_then_id() {
    let s = Store::open_in_memory().await.unwrap();
    let mut first = new_proposal("A", AiAction::Buy);
    first.at = at(10, 18);
    let mut second = new_proposal("B", AiAction::Buy);
    second.at = at(10, 17);
    s.add_ai_proposal(&first).await.unwrap();
    s.add_ai_proposal(&second).await.unwrap();

    let rows = s.ai_proposals_between(at(10, 0), at(11, 0)).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].proposal.symbol, "B"); // раніше за часом
    assert_eq!(rows[1].proposal.symbol, "A");
}
