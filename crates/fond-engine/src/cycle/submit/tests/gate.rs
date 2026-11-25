//! Режими рушія й автопаузи: пауза, аварійна зупинка, ліміт ордерів, серія помилок брокера.

use super::*;

#[tokio::test]
async fn paused_moves_stops_and_exits_but_skips_entries() {
    let (dp, h) = setup().await;
    held_aapl_msft(&dp, &h).await;
    // Невідома позиція: звірка на початку кроку ставить паузу.
    h.broker.set_position(&sym("TSLA"), 5, dec!(250));
    h.data.set_latest_trade(&sym("SPY"), 101.00);
    let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
    let exit = approved_exit("MSFT", 20);
    let entry = approved_entry("SPY", None);
    insert(&dp, &[&mv, &exit, &entry]).await;
    let mut rx = dp.bus.subscribe();

    let summary = run(&dp).await;

    assert_eq!(
        summary,
        SubmitSummary {
            stop_moves: 1,
            exits: 1,
            skipped: 1,
            ..SubmitSummary::default()
        }
    );
    assert!(matches!(
        dp.store.engine_mode().await.unwrap(),
        EngineMode::Paused { reason } if reason.starts_with("Розбіжність з брокером")
    ));
    assert!(
        drain(&mut rx)
            .iter()
            .any(|e| matches!(e, EngineEventDto::AutoPaused { .. }))
    );
    assert_eq!(intent(&dp, &mv.id).await.status, IntentStatus::Filled);
    assert_eq!(intent(&dp, &exit.id).await.status, IntentStatus::Submitted);
    assert_eq!(intent(&dp, &entry.id).await.status, IntentStatus::Approved);
    let submitted = h.broker.submitted();
    assert_eq!(submitted.len(), 3, "два входи стенда й продаж MSFT");
    assert_eq!(
        (
            submitted[2].symbol.as_str(),
            submitted[2].side,
            submitted[2].qty
        ),
        ("MSFT", Side::Sell, 20)
    );
    assert!(submitted.iter().all(|r| r.symbol != sym("SPY")));
    let journal = journal_messages(&dp).await;
    assert!(
        journal
            .iter()
            .any(|m| m.starts_with("Входи не відправлено: Пауза: Розбіжність з брокером")),
        "{journal:?}"
    );
}

#[tokio::test]
async fn killed_sends_nothing() {
    let (dp, h) = setup().await;
    held_aapl(&dp, &h).await;
    h.data.set_latest_trade(&sym("SPY"), 101.00);
    let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
    let exit = approved_exit("AAPL", 10);
    let entry = approved_entry("SPY", None);
    insert(&dp, &[&mv, &exit, &entry]).await;
    dp.store
        .set_engine_mode(&killed(), dp.clock.now())
        .await
        .unwrap();
    let (sd, broker) = scripted(&dp, &h, vec![]);

    let summary = run(&sd).await;

    assert_eq!(
        summary,
        SubmitSummary {
            skipped: 3,
            ..SubmitSummary::default()
        }
    );
    assert!(broker.log().is_empty(), "{:?}", broker.log());
    assert_eq!(h.broker.submitted().len(), 1);
    assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
    for i in [&mv, &exit, &entry] {
        assert_eq!(intent(&dp, &i.id).await.status, IntentStatus::Approved);
    }
    assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
    assert_eq!(
        journal_messages(&dp).await[0],
        "Відправка перед відкриттям (сесія 2026-11-05): входів 0, виходів 0, підтягнуто \
         стопів 0, ребалансу 0, помилок 0, пропущено 3; зупинено: Аварійна зупинка"
    );
}

#[tokio::test]
async fn max_orders_per_day_pauses() {
    let (dp, h) = setup().await;
    let aapl = approved_entry("AAPL", None);
    let msft = approved_entry("MSFT", None);
    insert(&dp, &[&aapl, &msft]).await;
    h.data.set_latest_trade(&sym("AAPL"), 101.00);
    h.data.set_latest_trade(&sym("MSFT"), 101.00);
    let cfg = EngineConfig {
        risk: RiskConfig {
            max_orders_per_day: 1,
            ..RiskConfig::default()
        },
        ..EngineConfig::default()
    };
    let mut rx = dp.bus.subscribe();

    let summary = run_with(&dp, &cfg).await;

    assert_eq!(
        summary,
        SubmitSummary {
            entries: 1,
            skipped: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(intent(&dp, &aapl.id).await.status, IntentStatus::Submitted);
    assert_eq!(intent(&dp, &msft.id).await.status, IntentStatus::Approved);
    assert_eq!(h.broker.submitted().len(), 1);
    let reason = "Ліміт ордерів на день";
    assert_eq!(
        dp.store.engine_mode().await.unwrap(),
        EngineMode::Paused {
            reason: reason.into()
        }
    );
    assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused {
        reason: reason.into()
    }));
}

#[tokio::test]
async fn three_broker_errors_autopause() {
    let (dp, h) = setup().await;
    let entries: Vec<Intent> = ["AAPL", "MSFT", "NVDA", "SPY"]
        .into_iter()
        .map(|s| approved_entry(s, None))
        .collect();
    for e in &entries {
        insert(&dp, &[e]).await;
        h.data.set_latest_trade(&e.symbol, 101.00);
    }
    let rejected = || {
        Script::Fail(BrokerError::Rejected {
            status: 403,
            message: "insufficient buying power".into(),
        })
    };
    let (sd, broker) = scripted(&dp, &h, vec![rejected(), rejected(), rejected()]);
    let mut rx = dp.bus.subscribe();

    let summary = run(&sd).await;

    assert_eq!(
        summary,
        SubmitSummary {
            failed: 3,
            skipped: 1,
            ..SubmitSummary::default()
        }
    );
    assert_eq!(broker.submits(), 3);
    assert!(h.broker.submitted().is_empty());
    for e in &entries[..3] {
        assert_eq!(intent(&dp, &e.id).await.status, IntentStatus::Failed);
    }
    assert_eq!(
        intent(&dp, &entries[3].id).await.status,
        IntentStatus::Approved
    );
    let reason = "3 помилки брокера поспіль";
    assert_eq!(
        dp.store.engine_mode().await.unwrap(),
        EngineMode::Paused {
            reason: reason.into()
        }
    );
    assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused {
        reason: reason.into()
    }));
}

// ---------------------------------------------------------------- дедлайни
