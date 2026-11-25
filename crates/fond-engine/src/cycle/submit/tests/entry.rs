//! Відправка входів: ідемпотентність, повтори після збоїв, перевірка ціни.

use super::super::orders::{build_entry_order, submit_idempotent};
use super::*;

#[tokio::test]
async fn submit_marks_intent_submitted_with_broker_id() {
    let (dp, h) = setup().await;
    let entry = approved_entry("AAPL", None);
    insert(&dp, &[&entry]).await;
    h.data.set_latest_trade(&sym("AAPL"), 100.40);

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            entries: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(h.broker.submitted(), vec![build_entry_order(&entry)]);
    let stored = intent(&dp, &entry.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id.as_deref(), Some("ord-1"));
    assert_eq!(stored.error, None);
    assert_eq!(
        stored_order_status(&dp, "ord-1").await.as_deref(),
        Some("accepted")
    );
    // Журнал (новіші першими): запис перед відправкою і після неї.
    let journal = journal_messages(&dp).await;
    let at = |m: &str| {
        journal
            .iter()
            .position(|x| x == m)
            .unwrap_or_else(|| panic!("немає «{m}» у {journal:?}"))
    };
    let before = at("Відправляю вхід: купівля 10 шт. AAPL за лімітом 101.00, стоп 95.50");
    let after = at("Вхід відправлено: ордер ord-1");
    assert!(after < before, "{journal:?}");
    assert_eq!(
        journal[0],
        "Відправка перед відкриттям (сесія 2026-11-05): входів 1, виходів 0, підтягнуто \
         стопів 0, ребалансу 0, помилок 0, пропущено 0"
    );
}

#[tokio::test]
async fn duplicate_after_restart_resolves_existing_order() {
    let (dp, h) = setup().await;
    let entry = approved_entry("AAPL", None);
    insert(&dp, &[&entry]).await;
    h.data.set_latest_trade(&sym("AAPL"), 101.00);
    // Збій після подання ордера, до позначки наміру: ордер уже у брокера.
    let existing = dp.broker.submit(&build_entry_order(&entry)).await.unwrap();

    let summary = run(&dp).await;

    assert_eq!(summary.entries, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(h.broker.submitted().len(), 1);
    let stored = intent(&dp, &entry.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id, Some(existing.id));
}

#[tokio::test]
async fn transient_then_found_does_not_double_submit() {
    let (dp, h) = setup().await;
    let entry = approved_entry("AAPL", None);
    insert(&dp, &[&entry]).await;
    h.data.set_latest_trade(&sym("AAPL"), 101.00);
    // Брокер прийняв ордер, але відповідь загубилась.
    let (sd, broker) = scripted(&dp, &h, vec![Script::Lose(transient())]);

    let summary = run(&sd).await;

    assert_eq!(summary.entries, 1);
    assert_eq!(broker.submits(), 1, "без повторної відправки");
    assert_eq!(h.broker.submitted().len(), 1);
    let stored = intent(&dp, &entry.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id.as_deref(), Some("ord-1"));
    assert_eq!(dp.health.streak(), 0);
}

#[tokio::test]
async fn transient_without_order_retries_twice_then_gives_up() {
    let (dp, h) = setup().await;
    let req = build_entry_order(&approved_entry("AAPL", None));

    // Два збої без ордера у брокера: третя спроба проходить.
    let (sd, broker) = scripted(
        &dp,
        &h,
        vec![Script::Fail(transient()), Script::Fail(transient())],
    );
    let order = submit_idempotent(&sd, &req).await.unwrap();
    assert_eq!(order.client_order_id, req.client_order_id);
    assert_eq!(broker.submits(), 3);
    assert_eq!(h.broker.submitted().len(), 1);

    // Три збої поспіль: більше спроб немає.
    let other = OrderRequest {
        client_order_id: "fond-other".into(),
        ..req.clone()
    };
    let failing = vec![
        Script::Fail(transient()),
        Script::Fail(transient()),
        Script::Fail(transient()),
    ];
    let (sd, broker) = scripted(&dp, &h, failing);
    assert_eq!(submit_idempotent(&sd, &other).await, Err(transient()));
    assert_eq!(broker.submits(), 3);
    assert_eq!(h.broker.submitted().len(), 1);

    // Відмова брокера не повторюється й рахується в серії помилок.
    let rejected = BrokerError::Rejected {
        status: 422,
        message: "x".into(),
    };
    let (sd, broker) = scripted(&dp, &h, vec![Script::Fail(rejected.clone())]);
    assert_eq!(submit_idempotent(&sd, &other).await, Err(rejected));
    assert_eq!(broker.submits(), 1);
    assert_eq!(dp.health.streak(), 1);
}

// ---------------------------------------------------------------- виходи

#[tokio::test]
async fn price_sanity_fails_far_limit() {
    let (dp, h) = setup().await;
    h.data.set_latest_trade(&sym("AAPL"), 90.0);
    let entry = approved_entry("AAPL", None);
    insert(&dp, &[&entry]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &entry.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(
        stored.error.as_deref(),
        Some("Ціна поза межами ±5% від останньої")
    );
    assert!(h.broker.submitted().is_empty());
}

#[tokio::test]
async fn price_sanity_uses_stored_close_or_fails_without_price() {
    let (dp, h) = setup().await;
    // MSFT: угод немає, останній close у сховищі 100 — ліміт 101 у межах ±5%.
    dp.store
        .upsert_bars(&sym("MSFT"), &flat_bars(start_date(), N_BARS))
        .await
        .unwrap();
    let msft = approved_entry("MSFT", None);
    // SPY: ні угод, ні барів у сховищі.
    let spy = approved_entry("SPY", None);
    insert(&dp, &[&msft, &spy]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            entries: 1,
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(intent(&dp, &msft.id).await.status, IntentStatus::Submitted);
    let spy = intent(&dp, &spy.id).await;
    assert_eq!(spy.status, IntentStatus::Failed);
    assert_eq!(spy.error.as_deref(), Some("Немає останньої ціни"));
    assert_eq!(h.broker.submitted().len(), 1);
}
