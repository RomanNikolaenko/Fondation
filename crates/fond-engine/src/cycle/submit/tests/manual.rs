//! Ручне закриття позиції.

use super::super::manual::manual_attempt;
use super::*;

#[tokio::test]
async fn close_position_now_cancels_stop_and_sells_day() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.clock.set(et(session(), 10, 30));

    let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(closed.id, "20261105-trend_pullback-AAPL-exit-manual");
    assert_eq!(closed.session_date, session());
    assert_eq!(closed.strategy.as_str(), "trend_pullback");
    assert!(matches!(closed.kind, IntentKind::Exit { qty: 10, .. }));
    assert_eq!(closed.status, IntentStatus::Submitted);
    assert_eq!(closed.broker_order_id.as_deref(), Some("ord-2"));
    assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 2);
    assert_eq!(
        submitted[1],
        OrderRequest {
            client_order_id: "fond-20261105-trend_pullback-AAPL-exit-manual".into(),
            symbol: sym("AAPL"),
            side: Side::Sell,
            qty: 10,
            order_type: OrderType::Market,
            limit_price: None,
            stop_price: None,
            tif: TimeInForce::Day,
            class: OrderClass::Simple,
            stop_loss: None,
            take_profit: None,
        }
    );
    assert_eq!(intent(&dp, &closed.id).await, closed);
}

#[tokio::test]
async fn close_position_now_cancels_analysis_exit() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    let planned = approved_exit("AAPL", 10);
    insert(&dp, &[&planned]).await;
    // Операторська дія працює в будь-якому режимі, зокрема після аварійної зупинки.
    dp.store
        .set_engine_mode(&killed(), dp.clock.now())
        .await
        .unwrap();
    // Субота: ручне закриття належить наступній сесії (понеділок).
    h.clock.set(et(d(2026, 11, 7), 12, 0));

    let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(closed.id, "20261109-trend_pullback-AAPL-exit-manual");
    assert_eq!(closed.session_date, d(2026, 11, 9));
    assert_eq!(closed.status, IntentStatus::Submitted);
    let planned = intent(&dp, &planned.id).await;
    assert_eq!(planned.status, IntentStatus::Cancelled);
    assert_eq!(planned.error.as_deref(), Some("Позицію закривають вручну"));
    let sells: Vec<OrderRequest> = h
        .broker
        .submitted()
        .into_iter()
        .filter(|r| r.side == Side::Sell)
        .collect();
    assert_eq!(sells.len(), 1);
    assert_eq!(
        (
            sells[0].tif,
            sells[0].qty,
            sells[0].client_order_id.as_str()
        ),
        (
            TimeInForce::Day,
            10,
            "fond-20261109-trend_pullback-AAPL-exit-manual"
        )
    );
    let journal = journal_messages(&dp).await;
    let note = format!(
        "Намір на вихід {} скасовано: позицію закривають вручну",
        planned.id
    );
    assert!(journal.contains(&note), "{journal:?}");
    assert_eq!(dp.store.engine_mode().await.unwrap(), killed());

    // Повторне натискання не продає вдруге.
    let again = close_position_now(&dp, &sym("AAPL")).await.unwrap();
    assert_eq!(again, closed);
    assert_eq!(h.broker.submitted().len(), 2);
}

#[test]
fn manual_attempt_numbers() {
    let base = "20261105-trend_pullback-AAPL-exit-manual";
    assert_eq!(manual_attempt(base, base), Some(1));
    assert_eq!(manual_attempt(base, &format!("{base}-2")), Some(2));
    assert_eq!(manual_attempt(base, &format!("{base}-12")), Some(12));
    assert_eq!(manual_attempt(base, &format!("{base}-1")), None);
    assert_eq!(manual_attempt(base, &format!("{base}-x")), None);
    assert_eq!(manual_attempt(base, &format!("{base}2")), None);
    assert_eq!(
        manual_attempt(base, "20261105-trend_pullback-AAPL-exit"),
        None
    );
}

#[tokio::test]
async fn close_position_now_retries_after_attempt_without_sale() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.clock.set(et(session(), 10, 30));
    let first_id = "20261105-trend_pullback-AAPL-exit-manual";
    // Перша спроба: разовий збій перевірки ордера — `Failed`, нічого не скасовано й не
    // продано.
    h.broker.fail_next_lookup(&client_order_id(first_id));

    let first = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(first.id, first_id);
    assert_eq!(first.status, IntentStatus::Failed);
    assert!(h.broker.cancels().is_empty());
    assert_eq!(h.broker.submitted().len(), 1);

    // Друга спроба — новий намір `-manual-2`, продаж проходить.
    let second = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(second.id, format!("{first_id}-2"));
    assert_eq!(second.status, IntentStatus::Submitted);
    let sale = h.broker.submitted().last().cloned().unwrap();
    assert_eq!(sale.client_order_id, client_order_id(&second.id));
    assert_eq!(
        (sale.side, sale.qty, sale.tif),
        (Side::Sell, 10, TimeInForce::Day)
    );
    assert_eq!(intent(&dp, first_id).await.status, IntentStatus::Failed);
    let note = format!(
        "Намір на вихід (вручну): 10 шт. (попередня спроба {first_id} завершилась без продажу)"
    );
    assert!(journal_messages(&dp).await.contains(&note));

    // Денний продаж минув без виконання: звірка скасовує намір і знову ставить стоп.
    h.broker.expire(second.broker_order_id.as_deref().unwrap());
    reconcile(&dp).await.unwrap();
    assert_eq!(
        intent(&dp, &second.id).await.status,
        IntentStatus::Cancelled
    );
    let reprotected = dp.store.positions().await.unwrap()[0]
        .stop_order_id
        .clone()
        .unwrap();

    // Третя спроба — `-manual-3`: новий стоп скасовано, продаж знову подано.
    let third = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(third.id, format!("{first_id}-3"));
    assert_eq!(third.status, IntentStatus::Submitted);
    assert_eq!(order_status(&h, &reprotected).await, OrderStatus::Canceled);
    let sales: Vec<String> = h
        .broker
        .submitted()
        .into_iter()
        .filter(|r| r.order_type == OrderType::Market)
        .map(|r| r.client_order_id)
        .collect();
    assert_eq!(
        sales,
        vec![client_order_id(&second.id), client_order_id(&third.id)]
    );
}

#[tokio::test]
async fn close_position_now_resumes_live_manual_intent() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.clock.set(et(session(), 10, 30));
    // Попередній виклик створив намір і перервався до відправки.
    let live = Intent {
        id: "20261105-trend_pullback-AAPL-exit-manual".into(),
        ..approved_exit("AAPL", 10)
    };
    insert(&dp, &[&live]).await;

    let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(closed.id, live.id);
    assert_eq!(closed.status, IntentStatus::Submitted);
    assert_eq!(
        dp.store.intent(&format!("{}-2", live.id)).await.unwrap(),
        None
    );
    let sells: Vec<OrderRequest> = h
        .broker
        .submitted()
        .into_iter()
        .filter(|r| r.side == Side::Sell)
        .collect();
    assert_eq!(sells.len(), 1);
    assert_eq!(sells[0].client_order_id, client_order_id(&live.id));
}

#[tokio::test]
async fn close_position_now_does_not_repeat_completed_sale() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.clock.set(et(session(), 10, 30));
    let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();
    // Продано 4 із 10, решта денного ордера минула: звірка позначає вихід виконаним.
    let sale = closed.broker_order_id.clone().unwrap();
    h.broker.fill(&sale, dec!(100.00), 4);
    h.broker.expire(&sale);
    reconcile(&dp).await.unwrap();
    let done = intent(&dp, &closed.id).await;
    assert_eq!(done.status, IntentStatus::Filled);
    assert_eq!(dp.store.positions().await.unwrap()[0].position.qty, 6);
    let before = h.broker.submitted().len();

    let again = close_position_now(&dp, &sym("AAPL")).await.unwrap();

    assert_eq!(again, done);
    assert_eq!(h.broker.submitted().len(), before, "продаж не повторюється");
    assert_eq!(
        dp.store.intent(&format!("{}-2", closed.id)).await.unwrap(),
        None
    );
}
