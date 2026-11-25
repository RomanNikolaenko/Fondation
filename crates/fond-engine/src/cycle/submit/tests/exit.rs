//! Відправка виходів: скасування стопа й продажів, обсяг за позицією брокера, перезахист.

use super::super::orders::build_exit_order;
use super::*;

#[tokio::test]
async fn exit_cancels_stop_then_sells_opg() {
    let (dp, h) = setup().await;
    let (_, held) = held_aapl(&dp, &h).await;
    assert_eq!(held.stop_order_id.as_deref(), Some("ord-1-stop"));
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    let (sd, broker) = scripted(&dp, &h, vec![]);

    let summary = run(&sd).await;

    assert_eq!(
        summary,
        SubmitSummary {
            exits: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(h.broker.cancels()[0], "ord-1-stop");
    // Спершу підтверджене скасування стопа, потім продаж.
    assert_eq!(
        broker.log(),
        vec![
            "cancel ord-1-stop".to_string(),
            format!("submit {}", client_order_id(&exit.id)),
        ]
    );
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 2);
    assert_eq!(submitted[1], build_exit_order(&exit, TimeInForce::Opg));
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id.as_deref(), Some("ord-2"));
    // Позиція лишається в обліку до виконання продажу (її зніме звірка).
    assert_eq!(dp.store.positions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn exit_not_sent_if_cancel_fails() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.broker.fail_cancel("ord-1-stop");
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(stored.error.as_deref(), Some("Не вдалося скасувати стоп"));
    assert_eq!(h.broker.submitted().len(), 1, "продажу немає");
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
}

#[tokio::test]
async fn exit_never_sells_more_than_broker_holds() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // У брокера лише 7 із 10 відстежуваних акцій: звірка ставить паузу й зменшує стоп.
    h.broker.set_position(&sym("AAPL"), 7, dec!(101.00));
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;

    let summary = run(&dp).await;

    assert_eq!(summary.exits, 1);
    assert!(matches!(
        dp.store.engine_mode().await.unwrap(),
        EngineMode::Paused { .. }
    ));
    let sale = h.broker.submitted().last().cloned().unwrap();
    assert_eq!(sale.client_order_id, client_order_id(&exit.id));
    assert_eq!(
        (sale.side, sale.qty, sale.tif),
        (Side::Sell, 7, TimeInForce::Opg)
    );
    // Поряд із виходом не лишилося жодного іншого відкритого продажу.
    let open = h.broker.open_orders().await.unwrap();
    let open_sells: Vec<String> = flatten(&open)
        .into_iter()
        .filter(|(o, _)| o.side == Side::Sell && o.status.is_open())
        .map(|(o, _)| o.id.clone())
        .collect();
    let sale_id = intent(&dp, &exit.id).await.broker_order_id.unwrap();
    assert_eq!(open_sells, vec![sale_id]);
}

#[tokio::test]
async fn exit_without_broker_position_fails() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.broker.set_position(&sym("AAPL"), 0, Decimal::ZERO);
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(stored.error.as_deref(), Some("Позиції у брокера немає"));
    assert!(h.broker.submitted().iter().all(|r| r.side == Side::Buy));
}

#[tokio::test]
async fn exit_after_restart_adopts_existing_sale() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    // Збій після скасування стопа й подання продажу, до позначки наміру.
    h.broker.cancel("ord-1-stop").await.unwrap();
    let sale = dp
        .broker
        .submit(&build_exit_order(&exit, TimeInForce::Opg))
        .await
        .unwrap();

    let summary = run(&dp).await;

    assert_eq!(summary.exits, 1);
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id.as_deref(), Some(sale.id.as_str()));
    assert_eq!(h.broker.submitted().len(), 2, "продаж не дублюється");
    assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
    assert!(order_status(&h, &sale.id).await.is_open());
}

#[tokio::test]
async fn rejected_sale_reprotects_position() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    let rejected = BrokerError::Rejected {
        status: 422,
        message: "market closed".into(),
    };
    let (sd, _) = scripted(&dp, &h, vec![Script::Fail(rejected)]);

    let summary = run(&sd).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(
        stored.error.as_deref(),
        Some("Помилка брокера: order rejected (422): market closed")
    );
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    // Стоп уже скасовано, продажу немає: позиція знову під стопом.
    let held = dp.store.positions().await.unwrap();
    let stop_id = held[0].stop_order_id.clone().unwrap();
    assert_ne!(stop_id, "ord-1-stop");
    let stop = h.broker.order(&stop_id).await.unwrap();
    assert!(stop.status.is_open());
    assert_eq!(
        (stop.side, stop.order_type, stop.qty, stop.stop_price),
        (Side::Sell, OrderType::Stop, 10, Some(dec!(95.50)))
    );
    assert!(
        h.broker
            .submitted()
            .iter()
            .all(|r| r.order_type != OrderType::Market)
    );
}

#[tokio::test]
async fn unanswered_sale_is_left_to_reconcile() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    let failing = vec![
        Script::Fail(transient()),
        Script::Fail(transient()),
        Script::Fail(transient()),
    ];
    let (sd, broker) = scripted(&dp, &h, failing);

    let summary = run(&sd).await;

    assert_eq!(summary.exits, 1);
    assert_eq!(broker.submits(), 3);
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Submitted);
    assert_eq!(stored.broker_order_id, None);
    assert!(
        stored
            .error
            .as_deref()
            .is_some_and(|e| e.ends_with("стан ордера перевірить звірка")),
        "{:?}",
        stored.error
    );
    // Продаж міг дійти до брокера: новий стоп не подається, доки звірка не з'ясує стан.
    assert_eq!(h.broker.submitted().len(), 1);

    reconcile(&dp).await.unwrap();

    assert_eq!(intent(&dp, &exit.id).await.status, IntentStatus::Failed);
    let held = dp.store.positions().await.unwrap();
    let stop = h
        .broker
        .order(held[0].stop_order_id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(stop.status.is_open());
    assert_eq!((stop.side, stop.qty), (Side::Sell, 10));
}

#[tokio::test]
async fn exit_cancels_take_profit_leg_before_stop() {
    let (dp, h) = setup().await;
    let (_, order) = submit_entry(
        &dp,
        "AAPL",
        10,
        dec!(101.00),
        dec!(95.50),
        Some(dec!(115.25)),
    )
    .await;
    h.broker.fill(&order.id, dec!(101.00), 10);
    reconcile(&dp).await.unwrap();
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    let (sd, broker) = scripted(&dp, &h, vec![]);

    let summary = run(&sd).await;

    assert_eq!(summary.exits, 1);
    // Ціль — першою: поки стоп живий, позиція під захистом.
    assert_eq!(
        broker.log(),
        vec![
            "cancel ord-1-tp".to_string(),
            "cancel ord-1-stop".to_string(),
            format!("submit {}", client_order_id(&exit.id)),
        ]
    );
    assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
}

#[tokio::test]
async fn exit_after_partial_stop_fill_sells_only_the_rest() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // Стоп уже продав 4 шт. і ще виконується: у брокера 6.
    h.broker.fill("ord-1-stop", dec!(95.40), 4);
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;

    let summary = run(&dp).await;

    assert_eq!(summary.exits, 1);
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    let sale = h.broker.submitted().last().cloned().unwrap();
    assert_eq!((sale.side, sale.qty), (Side::Sell, 6));
}

#[tokio::test]
async fn stop_filled_while_cancelling_blocks_sale_and_reprotects() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let exit = approved_exit("AAPL", 10);
    insert(&dp, &[&exit]).await;
    let (sd, broker) = scripted(&dp, &h, vec![]);
    // Між знімком позицій і скасуванням стоп продав 4 шт.
    *broker.fill_on_cancel.lock().unwrap() = Some(("ord-1-stop".into(), dec!(95.40), 4));

    let summary = run(&sd).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 1,
            ..SubmitSummary::default()
        }
    );
    let stored = intent(&dp, &exit.id).await;
    assert_eq!(stored.status, IntentStatus::Failed);
    assert_eq!(
        stored.error.as_deref(),
        Some("Стоп ord-1-stop виконано до скасування (4 шт.): продаж не відправлено")
    );
    assert!(
        h.broker
            .submitted()
            .iter()
            .all(|r| r.order_type != OrderType::Market),
        "продажу за застарілим знімком немає"
    );
    // Решта (6 шт.) знову під стопом.
    let held = dp.store.positions().await.unwrap();
    let stop = h
        .broker
        .order(held[0].stop_order_id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(stop.status.is_open());
    assert_eq!(
        (stop.side, stop.order_type, stop.qty),
        (Side::Sell, OrderType::Stop, 6)
    );
}

// ---------------------------------------------------------------- підтягування стопа
