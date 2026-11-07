use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderStatus, OrderType, Side};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::signal::EntrySignal;
use fond_core::traits::{NewsVerdict, Verdict};
use fond_core::types::{StrategyId, Symbol};
use fond_store::{
    BacktestStatus, EquityKind, EquitySnapshot, SignalRecord, SignalStatus, Store, StoreError,
};
use rust_decimal_macros::dec;
use serde_json::json;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn at(day: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, h, 0, 0).unwrap()
}

fn intent(id: &str, sym: &str, session: NaiveDate, status: IntentStatus, created: u32) -> Intent {
    Intent {
        id: id.into(),
        session_date: session,
        signal_date: d(2026, 1, 2),
        symbol: Symbol::new(sym),
        strategy: StrategyId::new("breakout"),
        kind: IntentKind::Entry {
            qty: 10,
            limit_price: dec!(100.50),
            stop_price: dec!(95.00),
            take_profit: Some(dec!(110.00)),
            risk_usd: 55.0,
            news: None,
        },
        status,
        explanation: "тест".into(),
        created_at: at(created, 10),
        updated_at: at(created, 10),
        broker_order_id: None,
        error: None,
    }
}

fn tracked(sym: &str) -> TrackedPosition {
    TrackedPosition {
        position: Position {
            symbol: Symbol::new(sym),
            strategy: StrategyId::new("breakout"),
            qty: 5,
            entry_price: 50.0,
            entry_date: d(2026, 1, 5),
            stop_price: 47.0,
            take_profit: None,
            sector: "Tech".into(),
        },
        stop_order_id: Some("stop-1".into()),
        intent_id: "i1".into(),
    }
}

#[tokio::test]
async fn insert_intent_is_idempotent() {
    let s = Store::open_in_memory().await.unwrap();
    let i = intent("a", "AAPL", d(2026, 1, 6), IntentStatus::Approved, 5);
    assert!(s.insert_intent(&i).await.unwrap());
    let mut other = i.clone();
    other.explanation = "інше".into();
    assert!(!s.insert_intent(&other).await.unwrap());
    let all = s
        .intents_with_status(&[IntentStatus::Approved])
        .await
        .unwrap();
    assert_eq!(all, vec![i.clone()]);
    assert_eq!(s.intent("a").await.unwrap(), Some(i));
    assert_eq!(s.intent("zzz").await.unwrap(), None);
}

#[tokio::test]
async fn invalid_transition_rejected() {
    let s = Store::open_in_memory().await.unwrap();
    s.insert_intent(&intent("a", "AAPL", d(2026, 1, 6), IntentStatus::Filled, 5))
        .await
        .unwrap();
    let err = s
        .update_intent("a", IntentStatus::Approved, at(6, 12), None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::InvalidTransition {
            from: IntentStatus::Filled,
            to: IntentStatus::Approved
        }
    ));
    assert_eq!(
        s.intent("a").await.unwrap().unwrap().status,
        IntentStatus::Filled
    );
    let err = s
        .update_intent("nope", IntentStatus::Approved, at(6, 12), None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
}

#[tokio::test]
async fn valid_transition_updates_fields() {
    let s = Store::open_in_memory().await.unwrap();
    s.insert_intent(&intent(
        "a",
        "AAPL",
        d(2026, 1, 6),
        IntentStatus::Approved,
        5,
    ))
    .await
    .unwrap();
    let u = s
        .update_intent("a", IntentStatus::Submitted, at(6, 14), Some("b-1"), None)
        .await
        .unwrap();
    assert_eq!(u.status, IntentStatus::Submitted);
    assert_eq!(u.updated_at, at(6, 14));
    assert_eq!(u.broker_order_id.as_deref(), Some("b-1"));
    let u = s
        .update_intent("a", IntentStatus::Failed, at(6, 15), None, Some("збій"))
        .await
        .unwrap();
    assert_eq!(u.broker_order_id.as_deref(), Some("b-1"));
    assert_eq!(u.error.as_deref(), Some("збій"));
    assert_eq!(s.intent("a").await.unwrap().unwrap(), u);
}

#[tokio::test]
async fn intents_by_status_and_session() {
    let s = Store::open_in_memory().await.unwrap();
    let s6 = d(2026, 1, 6);
    let s7 = d(2026, 1, 7);
    s.insert_intent(&intent("c", "MSFT", s7, IntentStatus::Approved, 5))
        .await
        .unwrap();
    s.insert_intent(&intent("b", "AAPL", s6, IntentStatus::PendingApproval, 6))
        .await
        .unwrap();
    s.insert_intent(&intent("a", "NVDA", s6, IntentStatus::Approved, 5))
        .await
        .unwrap();
    s.insert_intent(&intent("d", "KO", s6, IntentStatus::Filled, 4))
        .await
        .unwrap();
    let ids = |v: Vec<Intent>| v.into_iter().map(|i| i.id).collect::<Vec<_>>();
    let got = s
        .intents_with_status(&[IntentStatus::Approved, IntentStatus::PendingApproval])
        .await
        .unwrap();
    assert_eq!(ids(got), vec!["a", "b", "c"]);
    assert!(s.intents_with_status(&[]).await.unwrap().is_empty());
    let got = s.intents_for_session(s6).await.unwrap();
    assert_eq!(ids(got), vec!["d", "a", "b"]);
}

#[tokio::test]
async fn positions_roundtrip_and_remove() {
    let s = Store::open_in_memory().await.unwrap();
    let p = tracked("AAPL");
    s.upsert_position(&p, at(5, 20)).await.unwrap();
    s.upsert_position(&tracked("MSFT"), at(5, 20))
        .await
        .unwrap();
    let mut p2 = p.clone();
    p2.position.stop_price = 49.0;
    s.upsert_position(&p2, at(6, 20)).await.unwrap();
    let got = s.positions().await.unwrap();
    assert_eq!(got, vec![p2, tracked("MSFT")]);
    s.remove_position(&Symbol::new("AAPL")).await.unwrap();
    assert_eq!(s.positions().await.unwrap(), vec![tracked("MSFT")]);
    s.remove_position(&Symbol::new("AAPL")).await.unwrap();
}

#[tokio::test]
async fn orders_upsert() {
    let s = Store::open_in_memory().await.unwrap();
    let mut o = BrokerOrder {
        id: "o1".into(),
        client_order_id: "c1".into(),
        symbol: Symbol::new("AAPL"),
        side: Side::Buy,
        order_type: OrderType::Limit,
        qty: 10,
        filled_qty: 0,
        filled_avg_price: None,
        limit_price: Some(dec!(100.50)),
        stop_price: None,
        status: OrderStatus::New,
        legs: vec![],
        filled_at: None,
    };
    s.upsert_order(&o, at(5, 10)).await.unwrap();
    o.status = OrderStatus::Other("calculated".into());
    s.upsert_order(&o, at(5, 11)).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
    let st: String = sqlx::query_scalar("SELECT status FROM orders WHERE id = 'o1'")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(st, "calculated");
}

#[tokio::test]
async fn llm_spend_window() {
    let s = Store::open_in_memory().await.unwrap();
    assert_eq!(s.llm_spend_between(at(5, 0), at(6, 0)).await.unwrap(), 0.0);
    s.add_llm_usage(at(5, 12), "m", 100, 50, 0.10)
        .await
        .unwrap();
    s.add_llm_usage(at(6, 12), "m", 100, 50, 0.05)
        .await
        .unwrap();
    let today = s.llm_spend_between(at(6, 0), at(7, 0)).await.unwrap();
    assert!((today - 0.05).abs() < 1e-12);
    let both = s.llm_spend_between(at(5, 0), at(7, 0)).await.unwrap();
    assert!((both - 0.15).abs() < 1e-12);
    // верхня межа виключна
    assert_eq!(s.llm_spend_between(at(5, 0), at(5, 12)).await.unwrap(), 0.0);
}

fn snap(day: u32, h: u32, kind: EquityKind, session: NaiveDate, eq: f64) -> EquitySnapshot {
    EquitySnapshot {
        at: at(day, h),
        kind,
        session_date: session,
        equity: eq,
        cash: 1.0,
    }
}

#[tokio::test]
async fn equity_lookups() {
    let s = Store::open_in_memory().await.unwrap();
    let (s5, s6, s7) = (d(2026, 1, 5), d(2026, 1, 6), d(2026, 1, 7));
    s.insert_equity(&snap(5, 21, EquityKind::Close, s5, 1000.0))
        .await
        .unwrap();
    s.insert_equity(&snap(6, 15, EquityKind::Intraday, s6, 1050.0))
        .await
        .unwrap();
    s.insert_equity(&snap(6, 20, EquityKind::Close, s6, 1100.0))
        .await
        .unwrap();
    s.insert_equity(&snap(6, 21, EquityKind::Close, s6, 1110.0))
        .await
        .unwrap();
    s.insert_equity(&snap(7, 15, EquityKind::Intraday, s7, 1200.0))
        .await
        .unwrap();
    assert_eq!(s.close_equity(s5).await.unwrap(), Some(1000.0));
    assert_eq!(s.close_equity(s6).await.unwrap(), Some(1110.0));
    assert_eq!(s.close_equity(s7).await.unwrap(), None);
    assert_eq!(s.last_close_equity_before(s5).await.unwrap(), None);
    assert_eq!(
        s.last_close_equity_before(s6).await.unwrap(),
        Some((s5, 1000.0))
    );
    assert_eq!(
        s.last_close_equity_before(s7).await.unwrap(),
        Some((s6, 1110.0))
    );
}

#[tokio::test]
async fn news_verdict_cache_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let aapl = Symbol::new("AAPL");
    let day = d(2026, 1, 2);
    assert_eq!(s.news_verdict(&aapl, day).await.unwrap(), None);
    let v = NewsVerdict {
        verdict: Verdict::Caution,
        reasons: vec!["а".into(), "б".into()],
        key_events: vec!["подія".into()],
    };
    s.put_news_verdict(&aapl, day, &v, "haiku", at(2, 21))
        .await
        .unwrap();
    assert_eq!(s.news_verdict(&aapl, day).await.unwrap(), Some(v.clone()));
    let v2 = NewsVerdict {
        verdict: Verdict::Block,
        ..v
    };
    s.put_news_verdict(&aapl, day, &v2, "haiku", at(2, 22))
        .await
        .unwrap();
    assert_eq!(s.news_verdict(&aapl, day).await.unwrap(), Some(v2));
    s.put_earnings_check(&aapl, day, Some(d(2026, 1, 20)), true, true)
        .await
        .unwrap();
    s.put_earnings_check(&aapl, day, None, false, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn signals_insert_ignore_and_query() {
    let s = Store::open_in_memory().await.unwrap();
    let rec = |id: &str, sym: &str, status, reason: Option<&str>| SignalRecord {
        id: id.into(),
        signal: EntrySignal {
            strategy: StrategyId::new("breakout"),
            symbol: Symbol::new(sym),
            signal_date: d(2026, 1, 2),
            reference_price: 100.0,
            stop_price: 95.0,
            take_profit: Some(110.0),
            strength: 0.7,
            explanation: "пояснення".into(),
        },
        status,
        reject_reason: reason.map(String::from),
    };
    let b = rec("2", "MSFT", SignalStatus::Rejected, Some("новини"));
    let a = rec("1", "AAPL", SignalStatus::Accepted, None);
    s.insert_signal(&b).await.unwrap();
    s.insert_signal(&a).await.unwrap();
    s.insert_signal(&rec("1", "AAPL", SignalStatus::Rejected, Some("x")))
        .await
        .unwrap();
    assert_eq!(s.signals_on(d(2026, 1, 2)).await.unwrap(), vec![a, b]);
    assert!(s.signals_on(d(2026, 1, 3)).await.unwrap().is_empty());
}

#[tokio::test]
async fn backtest_lifecycle() {
    let s = Store::open_in_memory().await.unwrap();
    let req = json!({"from": "2025-01-01"});
    s.create_backtest("b1", &req, at(5, 10)).await.unwrap();
    s.create_backtest("b2", &req, at(6, 10)).await.unwrap();
    let r = s.backtest("b1").await.unwrap().unwrap();
    assert_eq!(r.status, BacktestStatus::Queued);
    assert_eq!(r.request, req);
    assert_eq!(r.created_at, at(5, 10));
    s.set_backtest_status("b1", BacktestStatus::Running)
        .await
        .unwrap();
    assert_eq!(
        s.backtest("b1").await.unwrap().unwrap().status,
        BacktestStatus::Running
    );
    let res = json!({"cagr": 0.12});
    s.finish_backtest("b1", &res).await.unwrap();
    let r = s.backtest("b1").await.unwrap().unwrap();
    assert_eq!(r.status, BacktestStatus::Done);
    assert_eq!(r.result, Some(res));
    s.fail_backtest("b2", "збій").await.unwrap();
    let r = s.backtest("b2").await.unwrap().unwrap();
    assert_eq!(
        (r.status, r.error.as_deref()),
        (BacktestStatus::Failed, Some("збій"))
    );
    let list = s.backtests().await.unwrap();
    assert_eq!(
        list.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["b2", "b1"]
    );
    assert!(list.iter().all(|r| r.result.is_none()));
    assert_eq!(s.backtest("zz").await.unwrap(), None);
}
