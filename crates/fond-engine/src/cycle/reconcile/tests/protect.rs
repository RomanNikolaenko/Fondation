//! Захисні стопи: прийняття наявного, подання нового, ноги в стані скасування, обсяг.

use super::*;

#[tokio::test]
async fn ensure_stop_adopts_existing_open_stop() {
    let (dp, h) = setup().await;
    let (_, p) = held_aapl(&dp, &h).await;
    // Після перезапуску: нога скасована, а захисний стоп уже подано раніше.
    h.broker.cancel("ord-1-stop").await.unwrap();
    let earlier = OrderRequest {
        client_order_id: "fond-earlier-protect".into(),
        symbol: sym("AAPL"),
        side: Side::Sell,
        qty: 10,
        order_type: OrderType::Stop,
        limit_price: None,
        stop_price: Some(dec!(95.50)),
        tif: TimeInForce::Gtc,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    };
    let existing = dp.broker.submit(&earlier).await.unwrap();

    let updated = ensure_stop(&dp, &p).await.unwrap();

    assert_eq!(updated.stop_order_id.as_deref(), Some(existing.id.as_str()));
    assert_eq!(h.broker.submitted().len(), 2);
    let stored = dp.store.positions().await.unwrap();
    assert_eq!(stored[0].stop_order_id, updated.stop_order_id);

    // Повторний виклик нічого не змінює.
    assert_eq!(ensure_stop(&dp, &updated).await.unwrap(), updated);
    assert_eq!(h.broker.submitted().len(), 2);
}

#[tokio::test]
async fn ensure_stop_without_stop_order_submits_protect() {
    let (dp, h) = setup().await;
    let (entry, p) = held_aapl(&dp, &h).await;
    h.broker.cancel("ord-1-stop").await.unwrap();
    let bare = TrackedPosition {
        stop_order_id: None,
        ..p
    };
    h.clock.advance(Duration::minutes(5));

    let updated = ensure_stop(&dp, &bare).await.unwrap();

    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 2);
    protect_request(&submitted[1], "AAPL", 10, dec!(95.50));
    assert_eq!(
        submitted[1].client_order_id,
        protect_cid(&entry.id, dp.clock.now())
    );
    assert_eq!(updated.stop_order_id.as_deref(), Some("ord-2"));
    assert_eq!(
        dp.store.positions().await.unwrap()[0].stop_order_id,
        updated.stop_order_id
    );
}

#[tokio::test]
async fn duplicate_protect_id_of_dead_order_is_not_adopted() {
    let (dp, h) = setup().await;
    let (_, p) = held_aapl(&dp, &h).await;
    h.broker.cancel("ord-1-stop").await.unwrap();
    let first = ensure_stop(&dp, &p).await.unwrap();
    assert_eq!(first.stop_order_id.as_deref(), Some("ord-2"));
    // Той самий момент часу: новий стоп мав би той самий client id, а ордер з ним
    // уже скасовано — він не береться як захист.
    h.broker.cancel("ord-2").await.unwrap();

    let err = ensure_stop(&dp, &first).await.unwrap_err();

    assert!(matches!(
        err,
        CycleError::Broker(BrokerError::DuplicateClientOrderId)
    ));
    assert_eq!(h.broker.submitted().len(), 2);
    assert_eq!(
        dp.store.positions().await.unwrap()[0]
            .stop_order_id
            .as_deref(),
        Some("ord-2")
    );
    // Наступна спроба (інша секунда) подає новий стоп.
    h.clock.advance(Duration::seconds(1));
    let next = ensure_stop(&dp, &first).await.unwrap();
    assert_eq!(next.stop_order_id.as_deref(), Some("ord-3"));
}

#[tokio::test]
async fn stale_leg_that_cannot_be_cancelled_blocks_new_stop() {
    let (dp, h) = setup().await;
    let (intent, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
    h.broker.fill(&order.id, dec!(101.00), 50);
    h.broker.expire(&order.id);
    h.broker.fail_cancel("ord-1-stop");

    let report = reconcile(&dp).await.unwrap();

    // Позицію й виконання збережено, але другий стоп поверх старої ноги не подано.
    assert_eq!(report.entries_filled, vec![sym("NVDA")]);
    assert!(report.reprotected.is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
    assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].position.qty, 50);
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    let journal = journal_messages(&dp).await;
    assert!(
        journal
            .iter()
            .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop"),
        "{journal:?}"
    );

    // Наступна звірка знову перевіряє ногу (інший обсяг) і знову не подвоює стоп.
    reconcile(&dp).await.unwrap();
    assert_eq!(h.broker.submitted().len(), 1);
}

#[tokio::test]
async fn submitted_move_stop_intent_is_left_to_submit_step() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let kind = IntentKind::MoveStop {
        new_stop: dec!(98.00),
        stop_order_id: "ord-1-stop".into(),
    };
    let mv = intent_with("AAPL", IntentTag::Stop, kind, IntentStatus::Submitted, None);
    dp.store.insert_intent(&mv).await.unwrap();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(status(&dp, &mv.id).await, IntentStatus::Submitted);
    assert!(report.reprotected.is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
}

#[tokio::test]
async fn pending_cancel_stop_blocks_reprotect_after_expired_exit() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // Крок відправки скасував стоп, але брокер ще не завершив скасування.
    h.broker.set_status("ord-1-stop", pending_cancel());
    let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
    h.broker.expire(&order.id);

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(status(&dp, &exit.id).await, IntentStatus::Cancelled);
    assert!(report.reprotected.is_empty());
    // Другого стопа поруч зі старим, що ще може виконатись, немає.
    assert_eq!(h.broker.submitted().len(), 2);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    let journal = journal_messages(&dp).await;
    assert!(
        journal
            .iter()
            .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop"),
        "{journal:?}"
    );
}

#[tokio::test]
async fn pending_cancel_leg_blocks_protect_on_partial_entry() {
    let (dp, h) = setup().await;
    let (_, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
    h.broker.fill(&order.id, dec!(101.00), 50);
    h.broker.expire(&order.id);
    h.broker.set_status("ord-1-stop", pending_cancel());

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.entries_filled, vec![sym("NVDA")]);
    assert!(report.reprotected.is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].position.qty, 50);
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    assert!(
        journal_messages(&dp)
            .await
            .iter()
            .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop")
    );
}

#[tokio::test]
async fn protective_stop_never_exceeds_broker_qty() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // 3 шт. продано повз рушій: стоп на 10 при 7 у брокера відкрив би шорт.
    h.broker.set_position(&sym("AAPL"), 7, dec!(101));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    assert_eq!(
        live_sell_stops(&h, "AAPL").await,
        vec![("ord-2".to_string(), 7)]
    );
    assert_eq!(
        report.mismatches,
        vec![Mismatch::QtyDiffers {
            symbol: sym("AAPL"),
            local: 10,
            broker: 7
        }]
    );
}
