//! Підтягування стопа.

use super::*;

#[tokio::test]
async fn move_stop_failure_marks_failed_and_keeps_position() {
    let (dp, h) = setup().await;
    let (_, held) = held_aapl(&dp, &h).await;
    // Стоп у стані, який не можна замінити (звірка лишає його без змін).
    h.broker
        .set_status("ord-1-stop", OrderStatus::Other("pending_replace".into()));
    let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
    insert(&dp, &[&mv]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &mv.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(
        stored.error.as_deref(),
        Some(
            "Не вдалося підтягнути стоп: order rejected (422): order is not a \
             replaceable stop"
        )
    );
    assert_eq!(dp.store.positions().await.unwrap(), vec![held]);
}

#[tokio::test]
async fn move_stop_replaces_and_updates_position() {
    let (dp, h) = setup().await;
    let (_, held) = held_aapl(&dp, &h).await;
    let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
    insert(&dp, &[&mv]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            stop_moves: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Replaced);
    let new = h.broker.order("ord-2").await.unwrap();
    assert!(new.status.is_open());
    assert_eq!(
        (new.side, new.order_type, new.qty, new.stop_price),
        (Side::Sell, OrderType::Stop, 10, Some(dec!(98.00)))
    );
    assert_eq!(
        dp.store.positions().await.unwrap(),
        vec![TrackedPosition {
            position: Position {
                stop_price: 98.0,
                ..held.position.clone()
            },
            stop_order_id: Some("ord-2".into()),
            ..held.clone()
        }]
    );
    let stored = intent(&dp, &mv.id).await;
    assert_eq!(stored.status, IntentStatus::Filled);
    assert_eq!(stored.broker_order_id.as_deref(), Some("ord-2"));
    assert_eq!(
        stored_order_status(&dp, "ord-2").await.as_deref(),
        Some("new")
    );
    assert!(h.broker.cancels().is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
}

#[tokio::test]
async fn move_stop_uses_tracked_stop_and_never_lowers() {
    let (dp, h) = setup().await;
    held_aapl_msft(&dp, &h).await;
    // Намір створено з уже неактуальним id стопа (позицію відтоді перезахищено).
    let up = approved_move("AAPL", dec!(97.00), "ord-stale");
    let down = approved_move("MSFT", dec!(46.00), "ord-2-stop");
    insert(&dp, &[&up, &down]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            stop_moves: 1,
            skipped: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Replaced);
    assert_eq!(order_status(&h, "ord-2-stop").await, OrderStatus::New);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-3"));
    assert_eq!(held[0].position.stop_price, 97.0);
    assert_eq!(held[1].stop_order_id.as_deref(), Some("ord-2-stop"));
    assert_eq!(held[1].position.stop_price, 47.0);
    assert_eq!(intent(&dp, &up.id).await.status, IntentStatus::Filled);
    let down = intent(&dp, &down.id).await;
    assert_eq!(down.status, IntentStatus::Cancelled);
    assert_eq!(
        down.error.as_deref(),
        Some("Стоп не підтягнуто: новий рівень 46.00 не вище поточного 47.00")
    );
}

// ---------------------------------------------------------------- режими й ліміти
