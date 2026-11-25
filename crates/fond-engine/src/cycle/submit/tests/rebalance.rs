//! Відправка ребалансу портфеля.

use super::*;

#[tokio::test]
async fn rebalance_sell_before_buy_and_limit_day_simple() {
    let (dp, h) = setup().await;
    h.data.set_latest_trade(&sym("MSFT"), 100.00);
    h.data.set_latest_trade(&sym("SPY"), 100.00);
    h.broker.set_position(&sym("MSFT"), 30, dec!(100.00));
    // Відстежувана позиція під знімком звірки (без стопа — стратегія portfolio).
    dp.store
        .upsert_position(&tracked("MSFT", "portfolio", 30, 0.0, None), dp.clock.now())
        .await
        .unwrap();
    let sell = intent_with(
        "MSFT",
        IntentTag::Rebalance,
        IntentKind::Rebalance {
            side: Side::Sell,
            qty: 30,
            limit_price: dec!(99.00),
        },
        IntentStatus::Approved,
        None,
    );
    let buy = intent_with(
        "SPY",
        IntentTag::Rebalance,
        IntentKind::Rebalance {
            side: Side::Buy,
            qty: 5,
            limit_price: dec!(101.00),
        },
        IntentStatus::Approved,
        None,
    );
    // Купівля вставлена першою: черга мусить поставити продаж перед нею.
    insert(&dp, &[&buy, &sell]).await;
    let summary = run(&dp).await;
    assert_eq!(summary.rebalances, 2, "{summary:?}");
    assert_eq!(summary.entries, 0);
    assert_eq!(intent(&dp, &sell.id).await.status, IntentStatus::Submitted);
    assert_eq!(intent(&dp, &buy.id).await.status, IntentStatus::Submitted);

    // Продаж відправлено раніше за купівлю (журнал — новіші першими, тож
    // повідомлення продажу стоїть у списку пізніше).
    let journal = journal_messages(&dp).await;
    let sell_at = journal
        .iter()
        .position(|m| m.contains("Відправляю ребаланс: продаж"))
        .unwrap();
    let buy_at = journal
        .iter()
        .position(|m| m.contains("Відправляю ребаланс: купівля"))
        .unwrap();
    assert!(sell_at > buy_at, "{journal:?}");

    // Обидва ордери — Limit без стоп-леги.
    let sent_sell = intent(&dp, &sell.id).await.broker_order_id.unwrap();
    let sent_buy = intent(&dp, &buy.id).await.broker_order_id.unwrap();
    let o_sell = h.broker.order(&sent_sell).await.unwrap();
    let o_buy = h.broker.order(&sent_buy).await.unwrap();
    assert_eq!(o_sell.side, Side::Sell);
    assert_eq!(o_sell.order_type, OrderType::Limit);
    assert_eq!(o_sell.limit_price, Some(dec!(99.00)));
    assert!(o_sell.legs.is_empty());
    assert_eq!(o_buy.side, Side::Buy);
    assert_eq!(o_buy.order_type, OrderType::Limit);
    assert_eq!(o_buy.limit_price, Some(dec!(101.00)));
    assert!(o_buy.legs.is_empty());
}

#[tokio::test]
async fn rebalance_respects_price_sanity_and_daily_limit() {
    let (dp, h) = setup().await;
    h.data.set_latest_trade(&sym("SPY"), 50.00);
    let buy = intent_with(
        "SPY",
        IntentTag::Rebalance,
        IntentKind::Rebalance {
            side: Side::Buy,
            qty: 5,
            limit_price: dec!(101.00),
        },
        IntentStatus::Approved,
        None,
    );
    insert(&dp, &[&buy]).await;
    let summary = run(&dp).await;
    assert_eq!(summary.rebalances, 0);
    assert_eq!(summary.failed, 1);
    let i = intent(&dp, &buy.id).await;
    assert_eq!(i.status, IntentStatus::Failed);
    assert!(
        i.error
            .as_deref()
            .unwrap_or_default()
            .contains("Ціна поза межами"),
        "{i:?}"
    );
}
