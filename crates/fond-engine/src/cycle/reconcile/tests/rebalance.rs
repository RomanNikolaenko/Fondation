//! Звірка намірів ребалансу.

use super::*;

#[tokio::test]
async fn rebalance_buy_creates_portfolio_position_without_stop() {
    let (dp, h) = setup().await;
    let (i, order) = submit_rebalance(&dp, "AAPL", Side::Buy, 10, dec!(101.00)).await;
    h.broker
        .fill_at(&order.id, dec!(101.00), 10, et(session(), 9, 31));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.rebalances_filled, vec![sym("AAPL")]);
    assert_eq!(status(&dp, &i.id).await, IntentStatus::Filled);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    let tp = &held[0];
    assert_eq!(tp.position.symbol, sym("AAPL"));
    assert_eq!(
        tp.position.strategy,
        StrategyId::new(fond_core::rebalance::PORTFOLIO_STRATEGY)
    );
    assert_eq!(tp.position.qty, 10);
    assert_eq!(tp.position.entry_price, 101.0);
    assert_eq!(tp.stop_order_id, None);
    assert_eq!(tp.intent_id, i.id);
    // Ідемпотентність: повторна звірка нічого не змінює.
    let report2 = reconcile(&dp).await.unwrap();
    assert!(report2.rebalances_filled.is_empty());
    let held2 = dp.store.positions().await.unwrap();
    assert_eq!(held2[0].position.qty, 10);
}

#[tokio::test]
async fn rebalance_buy_not_executed_is_cancelled() {
    let (dp, h) = setup().await;
    let (i, order) = submit_rebalance(&dp, "AAPL", Side::Buy, 10, dec!(101.00)).await;
    // Ордер завершився без виконання.
    h.broker.cancel(&order.id).await.unwrap();

    let report = reconcile(&dp).await.unwrap();

    assert!(report.rebalances_filled.is_empty());
    assert_eq!(status(&dp, &i.id).await, IntentStatus::Cancelled);
    assert!(dp.store.positions().await.unwrap().is_empty());
}

#[tokio::test]
async fn rebalance_sell_reduces_position_and_closes_at_zero() {
    let (dp, h) = setup().await;
    let now = dp.clock.now();
    dp.store
        .upsert_position(&tracked("MSFT", "portfolio", 30, 0.0, None), now)
        .await
        .unwrap();
    h.broker.set_position(&sym("MSFT"), 30, dec!(100.00));
    let (i, order) = submit_rebalance(&dp, "MSFT", Side::Sell, 30, dec!(99.00)).await;
    h.broker
        .fill_at(&order.id, dec!(99.00), 30, et(session(), 9, 31));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.rebalances_filled, vec![sym("MSFT")]);
    assert_eq!(status(&dp, &i.id).await, IntentStatus::Filled);
    // Позицію закрито, re-protect не потрібен: відстежуваних позицій немає.
    assert!(dp.store.positions().await.unwrap().is_empty());
    // Розбіжностей немає: локальний стан збігається з брокером.
    assert!(report.mismatches.is_empty());
}

#[tokio::test]
async fn rebalance_sell_keeps_rest_of_position() {
    let (dp, h) = setup().await;
    let now = dp.clock.now();
    dp.store
        .upsert_position(&tracked("MSFT", "portfolio", 30, 0.0, None), now)
        .await
        .unwrap();
    h.broker.set_position(&sym("MSFT"), 30, dec!(100.00));
    let (i, order) = submit_rebalance(&dp, "MSFT", Side::Sell, 10, dec!(99.00)).await;
    h.broker
        .fill_at(&order.id, dec!(99.00), 10, et(session(), 9, 31));

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.rebalances_filled, vec![sym("MSFT")]);
    assert_eq!(status(&dp, &i.id).await, IntentStatus::Filled);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].position.qty, 20);
    assert_eq!(held[0].stop_order_id, None);
    assert!(report.mismatches.is_empty());
}
