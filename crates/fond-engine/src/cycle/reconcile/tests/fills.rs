//! Виконання відправлених намірів: входи, частки, скасування, стопи й виходи.

use super::*;

#[tokio::test]
async fn entry_fill_creates_tracked_position_with_stop_leg() {
    let (dp, h) = setup().await;
    let mut rx = dp.bus.subscribe();
    let (intent, order) = submit_entry(
        &dp,
        "AAPL",
        10,
        dec!(101.00),
        dec!(95.50),
        Some(dec!(115.25)),
    )
    .await;
    // Виконання о 19:30 ET 5 листопада = 00:30 UTC 6 листопада: дата входу — за ET.
    let filled_at = et(session(), 19, 30);
    assert_eq!(filled_at.date_naive(), d(2026, 11, 6));
    h.broker.fill_at(&order.id, dec!(101.00), 10, filled_at);
    // Звірка наступного дня: дата входу береться з filled_at, а не з годинника.
    let now = et(d(2026, 11, 6), 10, 30);
    h.clock.set(now);

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.entries_filled, vec![sym("AAPL")]);
    assert_eq!(report.orders_updated, 1);
    assert!(report.exits_filled.is_empty() && report.stops_hit.is_empty());
    assert!(report.reprotected.is_empty());
    assert!(report.mismatches.is_empty());

    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    let p = &held[0];
    assert_eq!(p.position.symbol, sym("AAPL"));
    assert_eq!(p.position.strategy.as_str(), "trend_pullback");
    assert_eq!(p.position.qty, 10);
    assert_eq!(p.position.entry_price, 101.0);
    assert_eq!(p.position.entry_date, session());
    assert_eq!(p.position.stop_price, 95.5);
    assert_eq!(p.position.take_profit, Some(115.25));
    assert_eq!(p.position.sector, "Technology");
    assert_eq!(p.stop_order_id.as_deref(), Some("ord-1-stop"));
    assert_eq!(p.intent_id, intent.id);

    let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
    assert_eq!(i.status, IntentStatus::Filled);
    assert_eq!(i.broker_order_id.as_deref(), Some("ord-1"));
    // Нога OTO вже захищає позицію: додаткового стопа немає.
    assert_eq!(h.broker.submitted().len(), 1);
    assert!(h.broker.cancels().is_empty());
    assert_eq!(
        stored_order_status(&dp, "ord-1").await.as_deref(),
        Some("filled")
    );

    let events = drain(&mut rx);
    assert_eq!(
        filled_events(&events),
        vec![("AAPL".into(), "buy".into(), 10, dec!(101))]
    );
    assert_eq!(
        equity_rows(&dp).await,
        vec![("intraday".into(), "2026-11-06".into(), 100_000.0)]
    );
    assert_eq!(dp.store.last_reconcile_at().await.unwrap(), Some(now));
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn partial_fill_then_expire_tracks_filled_qty() {
    let (dp, h) = setup().await;
    // NVDA немає в universe: сектор «Unknown».
    let (intent, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
    h.broker.fill(&order.id, dec!(101.0), 50);
    h.broker.expire(&order.id);
    let now = dp.clock.now();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.entries_filled, vec![sym("NVDA")]);
    assert_eq!(report.reprotected, vec![sym("NVDA")]);
    assert!(report.mismatches.is_empty());
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    let p = &held[0];
    assert_eq!(p.position.qty, 50);
    assert_eq!(p.position.entry_price, 101.0);
    // Часткове виконання без filled_at: дата входу — дата ET годинника.
    assert_eq!(p.position.entry_date, session());
    assert_eq!(p.position.sector, "Unknown");
    assert_eq!(p.stop_order_id.as_deref(), Some("ord-2"));

    // Нога OTO на 148 шт. скасована, подано захисний стоп на 50.
    assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
    assert_eq!(
        h.broker.order("ord-1-stop").await.unwrap().status,
        OrderStatus::Canceled
    );
    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 2);
    protect_request(&submitted[1], "NVDA", 50, dec!(96.00));
    assert_eq!(submitted[1].client_order_id, protect_cid(&intent.id, now));
    assert_eq!(
        stored_order_status(&dp, "ord-2").await.as_deref(),
        Some("accepted")
    );

    assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
    let journal = journal_messages(&dp).await;
    assert!(
        journal
            .iter()
            .any(|m| m.starts_with("Стоп відновлено") && m.contains("ord-2")),
        "{journal:?}"
    );
}

#[tokio::test]
async fn unfilled_cancel_marks_intent_cancelled() {
    let (dp, h) = setup().await;
    let mut rx = dp.bus.subscribe();
    let (intent, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
    h.broker.cancel(&order.id).await.unwrap();

    let report = reconcile(&dp).await.unwrap();

    assert!(report.entries_filled.is_empty());
    assert_eq!(report.orders_updated, 1);
    assert!(report.mismatches.is_empty());
    let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
    assert_eq!(i.status, IntentStatus::Cancelled);
    assert_eq!(i.broker_order_id.as_deref(), Some("ord-1"));
    assert!(dp.store.positions().await.unwrap().is_empty());
    assert_eq!(h.broker.submitted().len(), 1);
    assert!(filled_events(&drain(&mut rx)).is_empty());
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn stop_fill_removes_position_and_emits_stop_hit() {
    let (dp, h) = setup().await;
    let (_, p) = held_aapl(&dp, &h).await;
    assert_eq!(p.stop_order_id.as_deref(), Some("ord-1-stop"));
    h.broker.fill("ord-1-stop", dec!(95.40), 10);
    let mut rx = dp.bus.subscribe();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.stops_hit, vec![sym("AAPL")]);
    assert!(report.reprotected.is_empty() && report.exits_filled.is_empty());
    assert!(report.mismatches.is_empty());
    assert!(dp.store.positions().await.unwrap().is_empty());
    let hits: Vec<(String, Decimal)> = drain(&mut rx)
        .into_iter()
        .filter_map(|e| match e {
            EngineEventDto::StopHit { symbol, price } => Some((symbol, price_of(&price))),
            _ => None,
        })
        .collect();
    assert_eq!(hits, vec![("AAPL".to_string(), dec!(95.40))]);
    assert_eq!(h.broker.submitted().len(), 1);
    assert_eq!(
        stored_order_status(&dp, "ord-1-stop").await.as_deref(),
        Some("filled")
    );
    assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn exit_fill_removes_position() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    // Крок відправки скасовує стоп і продає за ринком на відкритті.
    h.broker.cancel("ord-1-stop").await.unwrap();
    let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
    h.broker.fill(&order.id, dec!(104.00), 10);
    let mut rx = dp.bus.subscribe();

    let report = reconcile(&dp).await.unwrap();

    assert_eq!(report.exits_filled, vec![sym("AAPL")]);
    assert!(report.reprotected.is_empty() && report.stops_hit.is_empty());
    assert!(report.mismatches.is_empty());
    assert!(dp.store.positions().await.unwrap().is_empty());
    let i = dp.store.intent(&exit.id).await.unwrap().unwrap();
    assert_eq!(i.status, IntentStatus::Filled);
    assert_eq!(i.broker_order_id.as_deref(), Some(order.id.as_str()));
    assert_eq!(
        filled_events(&drain(&mut rx)),
        vec![("AAPL".into(), "sell".into(), 10, dec!(104))]
    );
    // Нових стопів немає.
    assert_eq!(h.broker.submitted().len(), 2);
}

#[tokio::test]
async fn expired_exit_reprotects_position() {
    let (dp, h) = setup().await;
    let (entry, _) = held_aapl(&dp, &h).await;
    h.broker.cancel("ord-1-stop").await.unwrap();
    let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
    h.broker.expire(&order.id);
    let now = dp.clock.now();

    let report = reconcile(&dp).await.unwrap();

    assert!(report.exits_filled.is_empty());
    assert_eq!(report.reprotected, vec![sym("AAPL")]);
    assert!(report.mismatches.is_empty());
    assert_eq!(status(&dp, &exit.id).await, IntentStatus::Cancelled);
    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 3);
    protect_request(&submitted[2], "AAPL", 10, dec!(95.50));
    assert_eq!(submitted[2].client_order_id, protect_cid(&entry.id, now));
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].position.qty, 10);
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-3"));
    let journal = journal_messages(&dp).await;
    assert!(
        journal.iter().any(|m| m.starts_with("Вихід не виконано")),
        "{journal:?}"
    );
    assert!(journal.iter().any(|m| m.starts_with("Стоп відновлено")));
}

#[tokio::test]
async fn pending_exit_does_not_trigger_reprotect() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.broker.cancel("ord-1-stop").await.unwrap();
    let (exit, _) = submit_exit(&dp, "AAPL", 10).await;

    let report = reconcile(&dp).await.unwrap();

    assert!(report.reprotected.is_empty());
    assert!(report.exits_filled.is_empty());
    assert!(report.mismatches.is_empty());
    assert_eq!(h.broker.submitted().len(), 2);
    assert_eq!(status(&dp, &exit.id).await, IntentStatus::Submitted);
    let held = dp.store.positions().await.unwrap();
    assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
}
