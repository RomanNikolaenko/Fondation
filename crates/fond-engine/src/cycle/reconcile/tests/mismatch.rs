//! Розбіжності з брокером і збої запитів.

use super::*;

#[tokio::test]
async fn unknown_broker_position_autopauses() {
    let (dp, h) = setup().await;
    let mut rx = dp.bus.subscribe();
    h.broker.set_position(&sym("TSLA"), 5, dec!(250));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(
        report.mismatches,
        vec![Mismatch::UnknownBrokerPosition {
            symbol: sym("TSLA"),
            qty: 5
        }]
    );
    let EngineMode::Paused { reason } = dp.store.engine_mode().await.unwrap() else {
        panic!("очікувалась пауза");
    };
    assert!(reason.starts_with("Розбіжність з брокером: "), "{reason}");
    assert!(reason.contains("TSLA"), "{reason}");
    assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused { reason }));
    // Звірка завершується: знімок капіталу й мітка часу записані.
    assert_eq!(equity_rows(&dp).await.len(), 1);
    assert_eq!(
        dp.store.last_reconcile_at().await.unwrap(),
        Some(dp.clock.now())
    );
}

#[tokio::test]
async fn qty_difference_pauses_and_vanished_position_is_closed() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let (_, msft) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
    h.broker.fill(&msft.id, dec!(50.00), 20);
    reconcile(&dp).await.unwrap();
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    // Хтось продав частину AAPL і весь MSFT повз рушій.
    h.broker.set_position(&sym("AAPL"), 7, dec!(101));
    h.broker.set_position(&sym("MSFT"), 0, dec!(0));

    let report = reconcile(&dp).await.unwrap();

    // AAPL — розбіжність (пауза); MSFT у брокера немає — позицію закрито, а її стоп
    // скасовано, щоб він не відкрив шорт.
    assert_eq!(
        report.mismatches,
        vec![Mismatch::QtyDiffers {
            symbol: sym("AAPL"),
            local: 10,
            broker: 7
        }]
    );
    assert_eq!(report.exits_filled, vec![sym("MSFT")]);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].position.symbol, sym("AAPL"));
    assert_eq!(order_status(&h, "ord-2-stop").await, OrderStatus::Canceled);
    assert!(
        journal_messages(&dp)
            .await
            .iter()
            .any(|m| m.starts_with("Позицію MSFT закрито у брокера"))
    );
    assert!(matches!(
        dp.store.engine_mode().await.unwrap(),
        EngineMode::Paused { .. }
    ));
}

#[tokio::test]
async fn fill_seen_after_positions_snapshot_is_not_a_mismatch() {
    let (dp, h) = setup().await;
    // Знімок позицій зроблено до запиту ордера: у знімку купівлі ще немає.
    let (intent, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
    h.broker.fill(&order.id, dec!(101.00), 10);
    h.broker.set_position(&sym("AAPL"), 0, dec!(0));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
    assert!(report.mismatches.is_empty());
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    // Стоп-нога не чіпається, поки знімок не підтвердить обсяг.
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
    assert_eq!(h.broker.submitted().len(), 1);
}

#[tokio::test]
async fn open_partial_entry_is_not_a_mismatch() {
    let (dp, h) = setup().await;
    let (intent, order) = submit_entry(&dp, "AAPL", 100, dec!(101.00), dec!(95.50), None).await;
    h.broker.fill(&order.id, dec!(101.00), 40);

    let report = reconcile(&dp).await.unwrap();

    assert!(report.mismatches.is_empty());
    assert!(report.entries_filled.is_empty());
    assert_eq!(status(&dp, &intent.id).await, IntentStatus::Submitted);
    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn missing_broker_order_marks_intent_failed() {
    let (dp, _h) = setup().await;
    let intent = intent_with(
        "AAPL",
        IntentTag::Entry,
        entry_kind(10, dec!(101.00), dec!(95.50), None),
        IntentStatus::Submitted,
        Some("ord-404"),
    );
    dp.store.insert_intent(&intent).await.unwrap();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.orders_updated, 0);
    let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
    assert_eq!(i.status, IntentStatus::Failed);
    assert_eq!(i.error.as_deref(), Some("Ордер не знайдено у брокера"));
}

#[tokio::test]
async fn lookup_failure_is_journaled_and_next_intent_is_processed() {
    let (dp, h) = setup().await;
    let (aapl, a) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
    let (msft, m) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
    h.broker.fill(&a.id, dec!(101.00), 10);
    h.broker.fill(&m.id, dec!(50.00), 20);
    // Запит ордера AAPL падає (один раз).
    h.broker.fail_next_lookup(&client_order_id(&aapl.id));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.entries_filled, vec![sym("MSFT")]);
    assert_eq!(status(&dp, &aapl.id).await, IntentStatus::Submitted);
    assert_eq!(status(&dp, &msft.id).await, IntentStatus::Filled);
    // Стан AAPL невідомий: розбіжністю не вважається, паузи немає.
    assert!(report.mismatches.is_empty());
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    let journal = journal_messages(&dp).await;
    assert!(
        journal
            .iter()
            .any(|m| m.contains("не вдалося перевірити ордер") && m.contains("AAPL")),
        "{journal:?}"
    );
    assert_eq!(dp.health.streak(), 0);

    // Наступна звірка добирає AAPL.
    let report = reconcile(&dp).await.unwrap();
    assert_eq!(report.entries_filled, vec![sym("AAPL")]);
    assert_eq!(status(&dp, &aapl.id).await, IntentStatus::Filled);
}

#[tokio::test]
async fn broker_positions_failure_is_fatal_and_journaled() {
    let (dp, h) = setup().await;
    h.broker.fail_next(BrokerError::Transient("timeout".into()));

    let err = reconcile(&dp).await.unwrap_err();

    assert!(matches!(err, CycleError::Broker(BrokerError::Transient(_))));
    assert_eq!(dp.health.streak(), 1);
    assert_eq!(dp.store.last_reconcile_at().await.unwrap(), None);
    assert!(equity_rows(&dp).await.is_empty());
    let journal = journal_messages(&dp).await;
    assert!(
        journal.iter().any(|m| m.starts_with("Звірку перервано")),
        "{journal:?}"
    );
}
