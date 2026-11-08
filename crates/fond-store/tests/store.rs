use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use fond_core::scanner::ScanReject;
use fond_core::traits::{AssetInfo, Session};
use fond_core::types::{Bar, EngineMode, Symbol};
use fond_store::{
    ETFS_CSV, EventKind, EventQuery, NewEvent, SP500_CSV, StepName, StepStatus, Store, StoreError,
    UniverseEntry, UniverseSource, parse_universe_csv,
};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn t(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 5, h, m, 0).unwrap()
}

fn bar(date: NaiveDate, close: f64) -> Bar {
    Bar {
        date,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 1000.0,
    }
}

fn ev(kind: EventKind, symbol: Option<&str>, msg: &str) -> NewEvent {
    NewEvent {
        kind,
        symbol: symbol.map(Symbol::new),
        intent_id: None,
        message: msg.to_string(),
        data: serde_json::json!({"k": 1}),
    }
}

#[tokio::test]
async fn migrations_apply_on_fresh_db() {
    Store::open_in_memory().await.unwrap();
}

#[tokio::test]
async fn open_creates_file_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub.db");
    {
        let s = Store::open(&path).await.unwrap();
        s.set_last_reconcile_at(t(14, 0)).await.unwrap();
    }
    let s = Store::open(&path).await.unwrap();
    assert_eq!(s.last_reconcile_at().await.unwrap(), Some(t(14, 0)));
}

#[tokio::test]
async fn upsert_overwrites_existing_bar() {
    let s = Store::open_in_memory().await.unwrap();
    let aapl = Symbol::new("AAPL");
    s.upsert_bars(&aapl, &[bar(d(2026, 1, 5), 100.0)])
        .await
        .unwrap();
    s.upsert_bars(&aapl, &[bar(d(2026, 1, 5), 50.0)])
        .await
        .unwrap();
    let got = s.bars(&aapl, d(2026, 1, 1), d(2026, 1, 31)).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].close, 50.0);
}

#[tokio::test]
async fn bars_range_order_and_last_date() {
    let s = Store::open_in_memory().await.unwrap();
    let aapl = Symbol::new("AAPL");
    assert_eq!(s.last_bar_date(&aapl).await.unwrap(), None);
    let n = s
        .upsert_bars(
            &aapl,
            &[
                bar(d(2026, 1, 7), 3.0),
                bar(d(2026, 1, 5), 1.0),
                bar(d(2026, 1, 6), 2.0),
            ],
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
    let got = s.bars(&aapl, d(2026, 1, 6), d(2026, 1, 7)).await.unwrap();
    assert_eq!(
        got.iter().map(|b| b.close).collect::<Vec<_>>(),
        vec![2.0, 3.0]
    );
    assert_eq!(s.last_bar_date(&aapl).await.unwrap(), Some(d(2026, 1, 7)));
    assert!(
        s.bars(&Symbol::new("MSFT"), d(2026, 1, 1), d(2026, 2, 1))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sessions_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let mk = |day| Session {
        date: d(2026, 1, day),
        open: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
        close: NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
    };
    s.upsert_sessions(&[mk(6), mk(5)]).await.unwrap();
    let mut early = mk(6);
    early.close = NaiveTime::from_hms_opt(13, 0, 0).unwrap();
    s.upsert_sessions(std::slice::from_ref(&early))
        .await
        .unwrap();
    let got = s.sessions(d(2026, 1, 1), d(2026, 1, 31)).await.unwrap();
    assert_eq!(got, vec![mk(5), early]);
}

#[tokio::test]
async fn events_are_append_only() {
    let s = Store::open_in_memory().await.unwrap();
    let e = s
        .append_event(t(10, 0), &ev(EventKind::Scan, Some("AAPL"), "m"))
        .await
        .unwrap();
    assert_eq!(e.kind, EventKind::Scan);
    assert_eq!(e.at, t(10, 0));
    let upd = sqlx::query("UPDATE events SET message='x'")
        .execute(s.pool())
        .await;
    assert!(upd.is_err());
    let del = sqlx::query("DELETE FROM events").execute(s.pool()).await;
    assert!(del.is_err());
    let all = s.events(&EventQuery::default()).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].message, "m");
}

#[tokio::test]
async fn events_query_filters_and_order() {
    let s = Store::open_in_memory().await.unwrap();
    s.append_event(t(10, 0), &ev(EventKind::Scan, Some("AAPL"), "a"))
        .await
        .unwrap();
    s.append_event(t(11, 0), &ev(EventKind::Signal, Some("MSFT"), "b"))
        .await
        .unwrap();
    s.append_event(t(12, 0), &ev(EventKind::Signal, Some("AAPL"), "c"))
        .await
        .unwrap();
    let all = s.events(&EventQuery::default()).await.unwrap();
    assert_eq!(
        all.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
        ["c", "b", "a"]
    );
    let q = EventQuery {
        kinds: vec![EventKind::Signal],
        ..Default::default()
    };
    assert_eq!(s.events(&q).await.unwrap().len(), 2);
    let q = EventQuery {
        symbol: Some(Symbol::new("AAPL")),
        ..Default::default()
    };
    assert_eq!(s.events(&q).await.unwrap().len(), 2);
    let q = EventQuery {
        from: Some(t(11, 0)),
        to: Some(t(11, 30)),
        ..Default::default()
    };
    let r = s.events(&q).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].message, "b");
    let q = EventQuery {
        limit: 1,
        ..Default::default()
    };
    assert_eq!(s.events(&q).await.unwrap()[0].message, "c");
}

#[tokio::test]
async fn corrupt_event_kind_is_error() {
    let s = Store::open_in_memory().await.unwrap();
    sqlx::query("INSERT INTO events (at, kind, message, data) VALUES ('2026-01-05T10:00:00Z', 'bogus', 'm', 'null')")
        .execute(s.pool())
        .await
        .unwrap();
    let r = s.events(&EventQuery::default()).await;
    assert!(matches!(r, Err(StoreError::Corrupt(_))));
}

#[tokio::test]
async fn cycle_step_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let date = d(2026, 1, 5);
    assert_eq!(s.step_status(date, StepName::Analysis).await.unwrap(), None);
    s.mark_step(
        date,
        StepName::Analysis,
        StepStatus::Running,
        t(21, 30),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        s.step_status(date, StepName::Analysis).await.unwrap(),
        Some((StepStatus::Running, t(21, 30)))
    );
    s.mark_step(date, StepName::Analysis, StepStatus::Done, t(21, 45), None)
        .await
        .unwrap();
    assert_eq!(
        s.step_status(date, StepName::Analysis).await.unwrap(),
        Some((StepStatus::Done, t(21, 30)))
    );
    s.mark_step(
        date,
        StepName::PreOpenSubmit,
        StepStatus::Missed,
        t(14, 0),
        Some("late"),
    )
    .await
    .unwrap();
    assert_eq!(
        s.step_status(date, StepName::PreOpenSubmit).await.unwrap(),
        Some((StepStatus::Missed, t(14, 0)))
    );
    assert_eq!(
        s.step_status(date, StepName::ApprovalDeadline)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn clear_step_removes_only_that_record() {
    let s = Store::open_in_memory().await.unwrap();
    let date = d(2026, 1, 5);
    for step in [StepName::PreOpenSubmit, StepName::ApprovalDeadline] {
        s.mark_step(date, step, StepStatus::Running, t(14, 0), None)
            .await
            .unwrap();
    }
    s.mark_step(
        d(2026, 1, 6),
        StepName::PreOpenSubmit,
        StepStatus::Done,
        t(14, 0),
        None,
    )
    .await
    .unwrap();

    s.clear_step(date, StepName::PreOpenSubmit).await.unwrap();
    assert_eq!(
        s.step_status(date, StepName::PreOpenSubmit).await.unwrap(),
        None
    );
    assert_eq!(
        s.step_status(date, StepName::ApprovalDeadline)
            .await
            .unwrap(),
        Some((StepStatus::Running, t(14, 0)))
    );
    assert_eq!(
        s.step_status(d(2026, 1, 6), StepName::PreOpenSubmit)
            .await
            .unwrap(),
        Some((StepStatus::Done, t(14, 0)))
    );
    // Немає запису — не помилка; після очищення крок знову можна позначити.
    s.clear_step(date, StepName::Analysis).await.unwrap();
    s.mark_step(
        date,
        StepName::PreOpenSubmit,
        StepStatus::Running,
        t(14, 2),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        s.step_status(date, StepName::PreOpenSubmit).await.unwrap(),
        Some((StepStatus::Running, t(14, 2)))
    );
}

#[tokio::test]
async fn engine_mode_defaults_to_running_and_roundtrips() {
    let s = Store::open_in_memory().await.unwrap();
    assert_eq!(s.engine_mode().await.unwrap(), EngineMode::Running);
    let p = EngineMode::Paused {
        reason: "manual".into(),
    };
    s.set_engine_mode(&p, t(10, 0)).await.unwrap();
    assert_eq!(s.engine_mode().await.unwrap(), p);
    let k = EngineMode::Killed {
        reason: "loss".into(),
    };
    s.set_engine_mode(&k, t(11, 0)).await.unwrap();
    assert_eq!(s.engine_mode().await.unwrap(), k);
    s.set_engine_mode(&EngineMode::Running, t(12, 0))
        .await
        .unwrap();
    assert_eq!(s.engine_mode().await.unwrap(), EngineMode::Running);
}

#[tokio::test]
async fn pause_if_running_only_changes_running_mode() {
    let s = Store::open_in_memory().await.unwrap();
    // Без запису режим — Running: пауза вставляє рядок.
    assert!(s.pause_if_running("ліміт", t(10, 0)).await.unwrap());
    let paused = EngineMode::Paused {
        reason: "ліміт".into(),
    };
    assert_eq!(s.engine_mode().await.unwrap(), paused);
    // Повторна пауза не перезаписує причину.
    assert!(!s.pause_if_running("інша", t(10, 5)).await.unwrap());
    assert_eq!(s.engine_mode().await.unwrap(), paused);
    // Аварійна зупинка ніколи не «понижується» до паузи.
    let killed = EngineMode::Killed {
        reason: "користувач".into(),
    };
    s.set_engine_mode(&killed, t(11, 0)).await.unwrap();
    assert!(!s.pause_if_running("розбіжність", t(11, 5)).await.unwrap());
    assert_eq!(s.engine_mode().await.unwrap(), killed);
    // З Running (наявний рядок) — пауза; last_reconcile_at не зачіпається.
    s.set_last_reconcile_at(t(11, 30)).await.unwrap();
    s.set_engine_mode(&EngineMode::Running, t(12, 0))
        .await
        .unwrap();
    assert!(s.pause_if_running("розбіжність", t(12, 5)).await.unwrap());
    assert_eq!(
        s.engine_mode().await.unwrap(),
        EngineMode::Paused {
            reason: "розбіжність".into()
        }
    );
    assert_eq!(s.last_reconcile_at().await.unwrap(), Some(t(11, 30)));
}

#[tokio::test]
async fn last_reconcile_roundtrip_keeps_mode() {
    let s = Store::open_in_memory().await.unwrap();
    assert_eq!(s.last_reconcile_at().await.unwrap(), None);
    let p = EngineMode::Paused { reason: "r".into() };
    s.set_engine_mode(&p, t(10, 0)).await.unwrap();
    s.set_last_reconcile_at(t(15, 0)).await.unwrap();
    assert_eq!(s.last_reconcile_at().await.unwrap(), Some(t(15, 0)));
    assert_eq!(s.engine_mode().await.unwrap(), p);
    s.set_engine_mode(&EngineMode::Running, t(16, 0))
        .await
        .unwrap();
    assert_eq!(s.last_reconcile_at().await.unwrap(), Some(t(15, 0)));
}

#[test]
fn parse_universe_csv_handles_quotes_and_dots() {
    let text = "symbol,name,sector\n\"BRK.B\",\"Berkshire Hathaway, Inc.\",Financials\nAAPL,Apple Inc.,Information Technology\n\"X\",\"Say \"\"hi\"\"\",Energy\n";
    let v = parse_universe_csv(text, UniverseSource::Sp500).unwrap();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].symbol, Symbol::new("BRK.B"));
    assert_eq!(v[0].name, "Berkshire Hathaway, Inc.");
    assert_eq!(v[0].sector, "Financials");
    assert_eq!(v[0].source, UniverseSource::Sp500);
    assert!(!v[0].excluded);
    assert_eq!(v[2].name, "Say \"hi\"");
    assert!(parse_universe_csv("a,b,c\nx,y,z\n", UniverseSource::Etf).is_err());
    assert!(parse_universe_csv("symbol,name,sector\nonly,two\n", UniverseSource::Etf).is_err());
}

#[test]
fn bundled_universe_files_parse() {
    assert!(
        parse_universe_csv(SP500_CSV, UniverseSource::Sp500)
            .unwrap()
            .len()
            >= 495
    );
    assert_eq!(
        parse_universe_csv(ETFS_CSV, UniverseSource::Etf)
            .unwrap()
            .len(),
        15
    );
}

#[tokio::test]
async fn seed_universe_only_once() {
    let s = Store::open_in_memory().await.unwrap();
    assert!(s.seed_universe_if_empty().await.unwrap());
    let n = s.universe(true).await.unwrap().len();
    assert!(n >= 510);
    s.set_excluded(&Symbol::new("AAPL"), true).await.unwrap();
    assert!(!s.seed_universe_if_empty().await.unwrap());
    assert_eq!(s.universe(true).await.unwrap().len(), n);
    assert_eq!(s.universe(false).await.unwrap().len(), n - 1);
    let all = s.universe(true).await.unwrap();
    assert!(
        all.iter()
            .find(|e| e.symbol == Symbol::new("AAPL"))
            .unwrap()
            .excluded
    );
}

#[tokio::test]
async fn manual_entries_add_and_remove() {
    let s = Store::open_in_memory().await.unwrap();
    let e = UniverseEntry {
        symbol: Symbol::new("zzzz"),
        name: "Zed".into(),
        sector: "Energy".into(),
        source: UniverseSource::Manual,
        excluded: false,
    };
    s.add_manual(&e).await.unwrap();
    assert_eq!(s.universe(false).await.unwrap(), vec![e.clone()]);
    s.remove_manual(&e.symbol).await.unwrap();
    assert!(s.universe(true).await.unwrap().is_empty());
}

fn manual(symbol: &str, sector: &str) -> UniverseEntry {
    UniverseEntry {
        symbol: Symbol::new(symbol),
        name: format!("{symbol} Inc."),
        sector: sector.into(),
        source: UniverseSource::Manual,
        excluded: false,
    }
}

#[tokio::test]
async fn import_manual_skips_existing_and_counts() {
    let s = Store::open_in_memory().await.unwrap();
    s.seed_universe_if_empty().await.unwrap();
    s.set_excluded(&Symbol::new("AAPL"), true).await.unwrap();
    let before = s.universe(true).await.unwrap();
    let aapl_before = before
        .iter()
        .find(|e| e.symbol == Symbol::new("AAPL"))
        .unwrap()
        .clone();

    let (added, skipped) = s
        .import_manual(&[
            manual("AAPL", "Other"),
            manual("ZZZZ", "Energy"),
            manual("YYYY", "Utilities"),
            manual("ZZZZ", "Energy"),
        ])
        .await
        .unwrap();
    assert_eq!((added, skipped), (2, 2));

    let all = s.universe(true).await.unwrap();
    assert_eq!(all.len(), before.len() + 2);
    // Наявний запис не змінено: джерело, назва, сектор і виключення ті самі.
    let aapl = all
        .iter()
        .find(|e| e.symbol == Symbol::new("AAPL"))
        .unwrap();
    assert_eq!(aapl, &aapl_before);
    assert_eq!(aapl.source, UniverseSource::Sp500);
    assert!(aapl.excluded);
    let zzzz = all
        .iter()
        .find(|e| e.symbol == Symbol::new("ZZZZ"))
        .unwrap();
    assert_eq!(zzzz, &manual("ZZZZ", "Energy"));
}

#[tokio::test]
async fn import_manual_is_atomic() {
    let s = Store::open_in_memory().await.unwrap();
    // Друга вставка падає (тригер): перша має відкотитися.
    sqlx::query(
        "CREATE TRIGGER fail_yyyy BEFORE INSERT ON universe WHEN NEW.symbol = 'YYYY' \
         BEGIN SELECT RAISE(ABORT, 'test: YYYY is rejected'); END",
    )
    .execute(s.pool())
    .await
    .unwrap();
    let res = s
        .import_manual(&[manual("ZZZZ", "Energy"), manual("YYYY", "Utilities")])
        .await;
    assert!(res.is_err());
    assert!(s.universe(true).await.unwrap().is_empty());
}

#[tokio::test]
async fn remove_manual_ignores_non_manual_rows() {
    let s = Store::open_in_memory().await.unwrap();
    s.seed_universe_if_empty().await.unwrap();
    let before = s.universe(true).await.unwrap().len();
    s.remove_manual(&Symbol::new("SPY")).await.unwrap();
    assert_eq!(s.universe(true).await.unwrap().len(), before);
}

#[tokio::test]
async fn assets_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let a = AssetInfo {
        symbol: Symbol::new("AAPL"),
        tradable: true,
        active: false,
    };
    assert_eq!(s.asset(&a.symbol).await.unwrap(), None);
    s.upsert_assets(std::slice::from_ref(&a), t(10, 0))
        .await
        .unwrap();
    assert_eq!(s.asset(&a.symbol).await.unwrap(), Some(a.clone()));
    let b = AssetInfo {
        tradable: false,
        active: true,
        ..a.clone()
    };
    s.upsert_assets(std::slice::from_ref(&b), t(11, 0))
        .await
        .unwrap();
    assert_eq!(s.asset(&a.symbol).await.unwrap(), Some(b));
}

#[tokio::test]
async fn scan_results_roundtrip() {
    let s = Store::open_in_memory().await.unwrap();
    let date = d(2026, 1, 5);
    let first = vec![
        (Symbol::new("AAPL"), None),
        (Symbol::new("PENNY"), Some(ScanReject::PriceBelowMin)),
        (Symbol::new("BAD"), Some(ScanReject::BadData("gap".into()))),
    ];
    s.put_scan_results(date, &first).await.unwrap();
    let mut got = s.scan_results(date).await.unwrap();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = first.clone();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, want);
    s.put_scan_results(date, &[(Symbol::new("MSFT"), None)])
        .await
        .unwrap();
    assert_eq!(
        s.scan_results(date).await.unwrap(),
        vec![(Symbol::new("MSFT"), None)]
    );
    assert!(s.scan_results(d(2026, 1, 6)).await.unwrap().is_empty());
}
