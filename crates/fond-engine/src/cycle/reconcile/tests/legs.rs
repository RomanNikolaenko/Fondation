//! Залишкові ноги Bracket/OCO після виконання цілі, стопа чи виходу.

use super::*;

#[tokio::test]
async fn bracket_target_fill_closes_position_and_cancels_leftover_stop() {
    let (dp, h) = setup().await;
    held_aapl_bracket(&dp, &h).await;
    // Ціль виконано; стоп-нога (FakeBroker не має OCO) ще відкрита.
    h.broker.fill("ord-1-tp", dec!(115.25), 10);

    let report = reconcile(&dp).await.unwrap();

    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    assert_eq!(h.broker.submitted().len(), 1);
    assert_eq!(report.exits_filled, vec![sym("AAPL")]);
    assert!(report.reprotected.is_empty());
    assert!(report.mismatches.is_empty());
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn oco_cancelled_stop_after_target_fill_is_not_replaced() {
    let (dp, h) = setup().await;
    held_aapl_bracket(&dp, &h).await;
    // Брокер скасував стоп (OCO), коли виконалась ціль: позиції більше немає.
    h.broker.cancel("ord-1-stop").await.unwrap();
    h.broker.fill("ord-1-tp", dec!(115.25), 10);

    let report = reconcile(&dp).await.unwrap();

    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
    assert!(live_sell_stops(&h, "AAPL").await.is_empty());
    assert!(report.reprotected.is_empty());
    assert!(report.mismatches.is_empty());
}

#[tokio::test]
async fn exit_fill_cancels_leftover_open_stop() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // Стоп-нога лишилась відкритою, а вихід виконався повністю.
    let (_, order) = submit_exit(&dp, "AAPL", 10).await;
    h.broker.fill(&order.id, dec!(104.00), 10);

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.exits_filled, vec![sym("AAPL")]);
    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    assert_eq!(h.broker.submitted().len(), 2);
}

#[tokio::test]
async fn stop_hit_cancels_leftover_take_profit() {
    let (dp, h) = setup().await;
    held_aapl_bracket(&dp, &h).await;
    h.broker.fill("ord-1-stop", dec!(95.40), 10);

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.stops_hit, vec![sym("AAPL")]);
    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
}

#[tokio::test]
async fn partial_exit_adoption_leaves_one_live_stop() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // Вихід продав 4 з 10 і прострочився; стоп-нога на 10 ще відкрита, а на залишок
    // уже є захисний стоп, поданий раніше.
    let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
    h.broker.fill(&order.id, dec!(104.00), 4);
    h.broker.expire(&order.id);
    let earlier = dp
        .broker
        .submit(&stop_request("fond-earlier", "AAPL", 6, dec!(95.50)))
        .await
        .unwrap();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.exits_filled, vec![sym("AAPL")]);
    assert_eq!(status(&dp, &exit.id).await, IntentStatus::Filled);
    assert_eq!(
        live_sell_stops(&h, "AAPL").await,
        vec![(earlier.id.clone(), 6)]
    );
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].position.qty, 6);
    assert_eq!(held[0].stop_order_id.as_deref(), Some(earlier.id.as_str()));
    assert_eq!(h.broker.submitted().len(), 3);
    assert!(report.mismatches.is_empty());
}

#[tokio::test]
async fn partial_bracket_entry_cancels_take_profit_leg() {
    let (dp, h) = setup().await;
    let (_, order) = submit_entry(
        &dp,
        "NVDA",
        148,
        dec!(101.00),
        dec!(96.00),
        Some(dec!(120.00)),
    )
    .await;
    h.broker.fill(&order.id, dec!(101.00), 50);
    h.broker.expire(&order.id);

    reconcile(&dp).await.unwrap();

    // Ноги розраховані на 148 шт.: обидві скасовано, стоп — на виконані 50.
    assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    assert_eq!(
        live_sell_stops(&h, "NVDA").await,
        vec![("ord-2".to_string(), 50)]
    );
}
