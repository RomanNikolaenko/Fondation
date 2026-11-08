use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{Duration, NaiveTime, TimeZone, Utc};
use fond_adapters::http::Secret;
use fond_adapters::secrets::{MemorySecretStore, SecretStore};
use fond_api::config::EngineConfig;
use fond_api::events::{EngineEventDto, JournalEventDto};
use fond_backtest::{BacktestRequest, CostModel};
use fond_core::calendar::et_to_utc;
use fond_core::error::DataError;
use fond_core::ids::IntentTag;
use fond_core::intent::IntentStatus;
use fond_core::keys::SecretKey;
use fond_core::orders::Side;
use fond_core::portfolio::Account;
use fond_core::risk::RiskConfig;
use fond_core::scanner::ScannerConfig;
use fond_core::testkit::uptrend_then_pullback;
use fond_core::types::{EngineMode, ExecutionMode, Symbol};
use fond_store::{EquityKind, EquitySnapshot, EventQuery};
use fond_strategies::StrategiesConfig;
use futures::StreamExt;
use rust_decimal_macros::dec;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

use super::auth::{bind_addr, ensure_api_token};
use super::{ApiState, router};
use crate::config_io::{load_or_create, save_atomic};
use crate::cycle::test_support::{
    Handles, N_BARS, d, entry_kind, et, fixture, flat_bars, held_aapl, intent_with, session,
    tracked,
};
use crate::deps::{Deps, DepsFactory, IndustryLookup, SetupError};
use crate::runtime::Engine;

const TOKEN: &str = "test-token-0123456789abcdef";

/// Фабрика: залежності стенда, «бракує ключів Alpaca» або (як `LiveDepsFactory`) «бракує
/// ключів увімкнених фільтрів»; рахує збірки.
struct Factory {
    deps: Deps,
    builds: AtomicUsize,
    missing: AtomicBool,
    missing_filter_keys: AtomicBool,
}

impl Factory {
    fn builds(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }

    fn set_missing(&self, missing: bool) {
        self.missing.store(missing, Ordering::SeqCst);
    }

    fn set_missing_filter_keys(&self, missing: bool) {
        self.missing_filter_keys.store(missing, Ordering::SeqCst);
    }
}

impl DepsFactory for Factory {
    fn build(&self, cfg: &EngineConfig) -> Result<Deps, SetupError> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        if self.missing.load(Ordering::SeqCst) {
            return Err(SetupError::MissingSecrets(vec![
                SecretKey::AlpacaKeyId,
                SecretKey::AlpacaSecret,
            ]));
        }
        if self.missing_filter_keys.load(Ordering::SeqCst) {
            let mut keys = Vec::new();
            if cfg.news_filter.enabled {
                keys.push(SecretKey::AnthropicApiKey);
            }
            if cfg.earnings_filter.enabled {
                keys.push(SecretKey::FinnhubApiKey);
            }
            if !keys.is_empty() {
                return Err(SetupError::MissingSecrets(keys));
            }
        }
        Ok(self.deps.clone())
    }
}

/// Галузь недоступна (як заглушка з вимкненим фільтром звітності).
struct NoIndustry;

#[async_trait]
impl IndustryLookup for NoIndustry {
    async fn industry(&self, _s: &Symbol) -> Result<Option<String>, DataError> {
        Err(DataError::NotAvailable("off".into()))
    }
}

struct Api {
    app: Router,
    engine: Arc<Engine>,
    d: Deps,
    h: Handles,
    factory: Arc<Factory>,
    secrets: Arc<MemorySecretStore>,
    dir: tempfile::TempDir,
    _shutdown: watch::Sender<bool>,
}

async fn api_with(tweak: impl FnOnce(&mut Deps)) -> Api {
    let (mut d, h) = fixture().await;
    tweak(&mut d);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = EngineConfig::default();
    save_atomic(&path, &cfg).unwrap();
    let factory = Arc::new(Factory {
        deps: d.clone(),
        builds: AtomicUsize::new(0),
        missing: AtomicBool::new(false),
        missing_filter_keys: AtomicBool::new(false),
    });
    let secrets = Arc::new(MemorySecretStore::default());
    let engine = Engine::new(
        factory.clone(),
        d.store.clone(),
        d.clock.clone(),
        d.bus.clone(),
        cfg,
        path,
        secrets.clone(),
    );
    let (tx, rx) = watch::channel(false);
    let app = router(ApiState::new(engine.clone(), Secret::new(TOKEN), rx));
    Api {
        app,
        engine,
        d,
        h,
        factory,
        secrets,
        dir,
        _shutdown: tx,
    }
}

async fn api() -> Api {
    api_with(|_| {}).await
}

fn request(method: Method, uri: &str, body: Option<Value>, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(a) = auth {
        b = b.header(header::AUTHORIZATION, a);
    }
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    b.body(body).unwrap()
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

impl Api {
    async fn raw(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, v)
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.raw(request(method, uri, body, Some(&bearer()))).await
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::GET, uri, None).await
    }

    async fn mode(&self) -> EngineMode {
        self.d.store.engine_mode().await.unwrap()
    }
}

fn pending_entry(symbol: &str) -> fond_core::intent::Intent {
    intent_with(
        symbol,
        IntentTag::Entry,
        entry_kind(10, dec!(101.00), dec!(95.50), None),
        IntentStatus::PendingApproval,
        None,
    )
}

fn error_text(body: &Value) -> &str {
    body["error"].as_str().unwrap_or_default()
}

/// Чекає (до 5 с), доки умова не справдиться.
async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(5);
    while !cond().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дочекались: {what}"
        );
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
}

async fn intent_status(a: &Api, id: &str) -> IntentStatus {
    a.d.store.intent(id).await.unwrap().unwrap().status
}

fn kill_request(close_positions: bool) -> Request<Body> {
    request(
        Method::POST,
        "/engine/kill",
        Some(json!({ "close_positions": close_positions })),
        Some(&bearer()),
    )
}

async fn journal_messages(a: &Api) -> Vec<String> {
    a.d.store
        .events(&EventQuery::default())
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.message)
        .collect()
}

#[tokio::test]
async fn health_is_public() {
    let a = api().await;
    let (st, body) = a.raw(request(Method::GET, "/health", None, None)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn rejects_missing_or_wrong_token() {
    let a = api().await;
    let (st, body) = a.raw(request(Method::GET, "/status", None, None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(!error_text(&body).is_empty());

    let wrong = format!("Bearer {TOKEN}x");
    let (st, _) = a
        .raw(request(Method::GET, "/status", None, Some(&wrong)))
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // Токен без схеми Bearer не приймається.
    let (st, _) = a
        .raw(request(Method::GET, "/status", None, Some(TOKEN)))
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // Захищені й POST-маршрути.
    let (st, _) = a
        .raw(request(
            Method::POST,
            "/engine/kill",
            Some(json!({ "close_positions": false })),
            None,
        ))
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(a.mode().await, EngineMode::Running);

    let (st, body) = a.get("/status").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["engine_mode"]["state"], "running");
}

#[test]
fn binds_loopback_only() {
    assert_eq!(bind_addr(47615).ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(bind_addr(47615).port(), 47615);
}

#[tokio::test]
async fn ensure_api_token_generates_once() {
    let s = MemorySecretStore::default();
    let first = ensure_api_token(&s).await.unwrap();
    assert_eq!(first.expose().len(), 64);
    assert!(first.expose().chars().all(|c| c.is_ascii_hexdigit()));
    let again = ensure_api_token(&s).await.unwrap();
    assert_eq!(again.expose(), first.expose());
    assert_eq!(
        s.get(SecretKey::EngineApiToken).unwrap().unwrap().expose(),
        first.expose()
    );
}

#[tokio::test]
async fn status_reports_needs_setup_and_next_step() {
    let a = api().await;
    a.factory.set_missing(true);
    let (st, body) = a.get("/status").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        body["needs_setup"],
        json!(["alpaca_key_id", "alpaca_secret"])
    );
    assert_eq!(body["account_mode"], "paper");
    assert_eq!(body["execution_mode"], "approval");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    // Годинник — середа 16:30 ET: наступний крок — дедлайн схвалення в четвер 08:55 ET.
    assert_eq!(body["next_step"]["name"], "approval_deadline");
    assert_eq!(body["next_step"]["at"], "2026-11-05T13:55:00Z");
    assert_eq!(body["now"], "2026-11-04T21:30:00Z");
    assert!(body["last_reconcile_at"].is_null());

    a.factory.set_missing(false);
    let (_, body) = a.get("/status").await;
    assert_eq!(body["needs_setup"], json!([]));
}

#[tokio::test]
async fn approve_moves_pending_to_approved() {
    let a = api().await;
    let i = pending_entry("AAPL");
    a.d.store.insert_intent(&i).await.unwrap();

    let (st, body) = a
        .call(Method::POST, &format!("/intents/{}/approve", i.id), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["id"], i.id);
    assert_eq!(body["status"], "approved");
    let stored = a.d.store.intent(&i.id).await.unwrap().unwrap();
    assert_eq!(stored.status, IntentStatus::Approved);
    let journal = a.d.store.events(&EventQuery::default()).await.unwrap();
    assert!(
        journal
            .iter()
            .any(|e| e.intent_id.as_deref() == Some(i.id.as_str()))
    );

    let (st, list) = a.get("/intents?status=approved,pending_approval").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, none) = a.get("/intents?status=pending_approval").await;
    assert_eq!(none, json!([]));
    let (st, _) = a.get("/intents?status=bogus").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let other = pending_entry("MSFT");
    a.d.store.insert_intent(&other).await.unwrap();
    let (st, body) = a
        .call(Method::POST, &format!("/intents/{}/reject", other.id), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "rejected");
}

#[tokio::test]
async fn approve_non_pending_is_conflict() {
    let a = api().await;
    let mut i = pending_entry("AAPL");
    i.status = IntentStatus::Expired;
    a.d.store.insert_intent(&i).await.unwrap();

    let (st, body) = a
        .call(Method::POST, &format!("/intents/{}/approve", i.id), None)
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(!error_text(&body).is_empty());
    let (st, _) = a
        .call(Method::POST, &format!("/intents/{}/reject", i.id), None)
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        a.d.store.intent(&i.id).await.unwrap().unwrap().status,
        IntentStatus::Expired
    );

    let (st, _) = a.call(Method::POST, "/intents/nope/approve", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn approve_after_deadline_is_conflict() {
    let a = api().await;
    let i = pending_entry("AAPL");
    a.d.store.insert_intent(&i).await.unwrap();
    // 08:56 ET у день сесії: дедлайн 08:55 минув, хоча крок ще не встиг виконатись.
    a.h.clock.set(et_to_utc(
        i.session_date,
        NaiveTime::from_hms_opt(8, 56, 0).unwrap(),
    ));
    let (st, _) = a
        .call(Method::POST, &format!("/intents/{}/approve", i.id), None)
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        a.d.store.intent(&i.id).await.unwrap().unwrap().status,
        IntentStatus::PendingApproval
    );
}

#[tokio::test]
async fn put_config_invalid_returns_422_and_keeps_old() {
    let a = api().await;
    let (st, old) = a.get("/config").await;
    assert_eq!(st, StatusCode::OK);

    let mut bad = old.clone();
    bad["risk"]["risk_per_trade"] = json!(2.0);
    bad["api_port"] = json!(80);
    let (st, body) = a.call(Method::PUT, "/config", Some(bad)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let details: Vec<&str> = body["details"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(details.len(), 2, "{details:?}");
    assert!(details.iter().any(|m| m.contains("risk.risk_per_trade")));
    assert!(details.iter().any(|m| m.contains("api_port")));

    let (_, now) = a.get("/config").await;
    assert_eq!(now, old);
    assert_eq!(a.engine.config().await, EngineConfig::default());
    let on_disk = load_or_create(&a.dir.path().join("config.toml")).unwrap();
    assert_eq!(on_disk, EngineConfig::default());

    // Тіло не того типу — теж ApiErrorDto, а не текст.
    let (st, body) = a
        .call(Method::PUT, "/config", Some(json!({ "execution_mode": 5 })))
        .await;
    assert!(st.is_client_error(), "{st}");
    assert!(!error_text(&body).is_empty(), "{body}");

    let mut good = old.clone();
    good["execution_mode"] = json!("auto");
    let (st, body) = a.call(Method::PUT, "/config", Some(good)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["execution_mode"], "auto");
    assert_eq!(a.engine.config().await.execution_mode, ExecutionMode::Auto);
}

#[tokio::test]
async fn kill_endpoint_sets_killed() {
    let a = api().await;
    let i = pending_entry("AAPL");
    a.d.store.insert_intent(&i).await.unwrap();
    let mut rx = a.d.bus.subscribe();

    let (st, body) = a.raw(kill_request(false)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["broker_side"], "scheduled");
    assert!(!body["message"].as_str().unwrap().is_empty());
    assert!(matches!(a.mode().await, EngineMode::Killed { .. }));
    // Брокерська частина — у фоні: намір скасовується трохи згодом, підсумок — у журналі.
    wait_until("намір скасовано", || async {
        intent_status(&a, &i.id).await == IntentStatus::Cancelled
    })
    .await;
    wait_until(
        "підсумок аварійної зупинки в журналі",
        || async {
            journal_messages(&a)
                .await
                .iter()
                .any(|m| m.starts_with("Аварійна зупинка: скасовано ордерів"))
        },
    )
    .await;
    let mut killed = 0;
    let mut status_changed = 0;
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(5);
    while status_changed == 0 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(StdDuration::from_millis(100), rx.recv()).await {
            Ok(Ok(EngineEventDto::Killed { .. })) => killed += 1,
            Ok(Ok(EngineEventDto::StatusChanged)) => status_changed += 1,
            _ => {}
        }
    }
    while let Ok(e) = rx.try_recv() {
        if matches!(e, EngineEventDto::Killed { .. }) {
            killed += 1;
        }
    }
    assert_eq!(killed, 1, "подія Killed має бути одна");
    assert_eq!(
        status_changed, 1,
        "після брокерської частини — StatusChanged"
    );

    let (st, body) = a.call(Method::POST, "/engine/resume", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["engine_mode"]["state"], "running");
}

#[tokio::test]
async fn kill_returns_promptly_while_job_lock_is_held() {
    let a = api().await;
    let i = pending_entry("AAPL");
    a.d.store.insert_intent(&i).await.unwrap();
    let guard = a.engine.exclusive().await;

    // Відповідь не чекає на замок кроків (довгий аналіз).
    let (st, body) = tokio::time::timeout(StdDuration::from_secs(5), a.raw(kill_request(false)))
        .await
        .expect("відповідь на kill не чекає на замок");
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["broker_side"], "scheduled");
    assert!(matches!(a.mode().await, EngineMode::Killed { .. }));
    // Поки замок зайнято, скасування ще не відбулося.
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert_eq!(
        intent_status(&a, &i.id).await,
        IntentStatus::PendingApproval
    );

    // Запит уже завершено (клієнт міг і відключитись) — робота однаково доводиться до кінця.
    drop(guard);
    wait_until(
        "брокерська частина після звільнення замка",
        || async { intent_status(&a, &i.id).await == IntentStatus::Cancelled },
    )
    .await;
}

#[tokio::test]
async fn kill_with_alpaca_keys_but_missing_filter_keys_closes_positions() {
    let a = api().await;
    a.h.clock.set(et(session(), 10, 30));
    let (_, held) = held_aapl(&a.d, &a.h).await;
    let stop_id = held.stop_order_id.clone().unwrap();
    // Ключів Anthropic і Finnhub немає, а фільтри ввімкнені: повні залежності не збираються.
    a.factory.set_missing_filter_keys(true);
    a.engine.invalidate_deps();
    let (_, status) = a.get("/status").await;
    assert_eq!(
        status["needs_setup"],
        json!(["anthropic_api_key", "finnhub_api_key"])
    );

    let (st, body) = a.raw(kill_request(true)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["broker_side"], "scheduled");
    wait_until(
        "позицію продано аварійним закриттям",
        || async {
            a.h.broker.submitted().iter().any(|r| {
                r.side == Side::Sell
                    && r.symbol == Symbol::new("AAPL")
                    && r.qty == 10
                    && r.client_order_id.starts_with("fond-kill-")
            })
        },
    )
    .await;
    assert!(a.h.broker.cancels().contains(&stop_id), "стоп скасовано");
    assert!(
        journal_messages(&a)
            .await
            .iter()
            .all(|m| !m.contains("Ключів Alpaca немає")),
        "ключі Alpaca є"
    );
}

#[tokio::test]
async fn kill_without_alpaca_keys_is_unavailable() {
    let a = api().await;
    a.factory.set_missing(true);
    let (st, body) = a.raw(kill_request(true)).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["broker_side"], "unavailable");
    assert_eq!(body["message"], "Ключів Alpaca немає: ордери не скасовано");
    assert!(matches!(a.mode().await, EngineMode::Killed { .. }));
    let messages = journal_messages(&a).await;
    assert!(
        messages
            .iter()
            .any(|m| m == "Ключів Alpaca немає: ордери не скасовано"),
        "{messages:?}"
    );
}

#[tokio::test]
async fn pause_and_resume_by_user() {
    let a = api().await;
    let (st, body) = a.call(Method::POST, "/engine/pause", None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["engine_mode"]["state"], "paused");
    assert!(matches!(a.mode().await, EngineMode::Paused { .. }));
    let (st, body) = a.call(Method::POST, "/engine/resume", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["engine_mode"]["state"], "running");
}

#[tokio::test]
async fn set_secret_writes_store_and_invalidates_deps() {
    let a = api().await;
    a.engine.deps().await.unwrap();
    a.engine.deps().await.unwrap();
    assert_eq!(a.factory.builds(), 1);

    let (st, body) = a
        .call(
            Method::PUT,
            "/secrets/alpaca_key_id",
            Some(json!({ "value": " PKTEST1234 " })),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        a.secrets
            .get(SecretKey::AlpacaKeyId)
            .unwrap()
            .unwrap()
            .expose(),
        "PKTEST1234"
    );
    a.engine.deps().await.unwrap();
    assert_eq!(a.factory.builds(), 2);

    let (st, list) = a.get("/secrets").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        list,
        json!([
            { "key": "alpaca_key_id", "present": true },
            { "key": "alpaca_secret", "present": false },
            { "key": "anthropic_api_key", "present": false },
            { "key": "finnhub_api_key", "present": false },
        ])
    );
    let journal = a.d.store.events(&EventQuery::default()).await.unwrap();
    assert!(
        journal.iter().all(
            |e| !e.message.contains("PKTEST1234") && !e.data.to_string().contains("PKTEST1234")
        )
    );
}

#[tokio::test]
async fn secrets_endpoint_rejects_engine_api_token() {
    let a = api().await;
    let (st, body) = a
        .call(
            Method::PUT,
            "/secrets/engine_api_token",
            Some(json!({ "value": "hijack" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!error_text(&body).is_empty());
    assert!(a.secrets.get(SecretKey::EngineApiToken).unwrap().is_none());

    let (st, _) = a
        .call(
            Method::PUT,
            "/secrets/alpaca-key-id",
            Some(json!({ "value": "x" })),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = a
        .call(
            Method::PUT,
            "/secrets/alpaca_secret",
            Some(json!({ "value": "   " })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(a.secrets.get(SecretKey::AlpacaSecret).unwrap().is_none());

    let (_, list) = a.get("/secrets").await;
    assert!(!list.to_string().contains("engine_api_token"));
}

#[tokio::test]
async fn verify_without_keys_reports_missing_key() {
    let a = api().await;
    let (st, body) = a.call(Method::POST, "/secrets/verify", None).await;
    assert_eq!(st, StatusCode::OK);
    for service in ["alpaca", "finnhub", "anthropic"] {
        assert_eq!(body[service]["ok"], false, "{body}");
        assert_eq!(body[service]["message"], "немає ключа", "{body}");
    }
}

#[tokio::test]
async fn ws_streams_journal_events() {
    let a = api().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = a.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("ws://{addr}/events");

    // Без токена рукостискання відхиляється.
    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED)
        }
        Err(other) => panic!("очікувалась відмова 401, є {other}"),
        Ok(_) => panic!("очікувалась відмова 401"),
    }

    let mut req = url.as_str().into_client_request().unwrap();
    req.headers_mut()
        .insert(header::AUTHORIZATION, bearer().parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let event = EngineEventDto::Journal {
        event: JournalEventDto {
            id: 42,
            at: Utc.with_ymd_and_hms(2026, 11, 4, 21, 31, 0).unwrap(),
            kind: "scan".into(),
            symbol: Some("AAPL".into()),
            intent_id: None,
            message: "Сканер".into(),
            data: json!({ "passed": 2 }),
        },
    };
    a.d.bus.publish(event.clone());
    let msg = tokio::time::timeout(StdDuration::from_secs(5), ws.next())
        .await
        .expect("подія не надійшла")
        .unwrap()
        .unwrap();
    let got: EngineEventDto = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(got, event);
}

fn backtest_request(a: &Api) -> BacktestRequest {
    BacktestRequest {
        from: a.h.last_date() - Duration::days(60),
        to: a.h.last_date(),
        split: None,
        initial_equity: 100_000.0,
        strategies: StrategiesConfig::default(),
        risk: RiskConfig::default(),
        scanner: ScannerConfig::default(),
        costs: CostModel::default(),
    }
}

#[tokio::test]
async fn backtest_job_roundtrip() {
    let a = api().await;
    let start = a.h.start_date();
    for (s, bars) in [
        ("SPY", uptrend_then_pullback(start, N_BARS, 0)),
        ("AAPL", uptrend_then_pullback(start, N_BARS - 3, 3)),
        ("MSFT", flat_bars(start, N_BARS)),
    ] {
        a.d.store.upsert_bars(&Symbol::new(s), &bars).await.unwrap();
    }

    let req = serde_json::to_value(backtest_request(&a)).unwrap();
    let (st, run) = a.call(Method::POST, "/backtests", Some(req.clone())).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{run}");
    assert_eq!(run["status"], "queued");
    assert_eq!(run["request"], req);
    let id = run["id"].as_str().unwrap().to_string();

    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(30);
    let detail = loop {
        let (st, detail) = a.get(&format!("/backtests/{id}")).await;
        assert_eq!(st, StatusCode::OK);
        if matches!(detail["run"]["status"].as_str(), Some("done" | "failed")) {
            break detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "бектест не завершився"
        );
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    };
    assert_eq!(detail["run"]["status"], "done", "{}", detail["run"]);
    assert!(detail["result"]["metrics"]["full"].is_object());
    assert!(detail["result"]["metrics"]["benchmark"].is_object());

    let (_, list) = a.get("/backtests").await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], id);
    assert!(list[0].get("result").is_none());

    let (st, _) = a.get("/backtests/missing").await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Один бектест за раз.
    let held = a.engine.jobs().try_backtest().unwrap();
    let (st, body) = a.call(Method::POST, "/backtests", Some(req.clone())).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(!error_text(&body).is_empty());
    drop(held);

    let mut bad = backtest_request(&a);
    bad.from = bad.to + Duration::days(1);
    let (st, _) = a
        .call(
            Method::POST,
            "/backtests",
            Some(serde_json::to_value(bad).unwrap()),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn backtest_without_market_history_fails_with_message() {
    let a = api().await;
    let req = serde_json::to_value(backtest_request(&a)).unwrap();
    let (st, run) = a.call(Method::POST, "/backtests", Some(req)).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let id = run["id"].as_str().unwrap().to_string();
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(10);
    let detail = loop {
        let (_, detail) = a.get(&format!("/backtests/{id}")).await;
        if matches!(detail["run"]["status"].as_str(), Some("done" | "failed")) {
            break detail;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    };
    assert_eq!(detail["run"]["status"], "failed");
    assert!(detail["run"]["error"].as_str().unwrap().contains("SPY"));
    assert!(detail["result"].is_null());
}

#[tokio::test]
async fn universe_add_manual_uses_industry_or_unknown() {
    let a = api().await;
    let (st, e) = a
        .call(
            Method::POST,
            "/universe",
            Some(json!({ "symbol": " nvda " })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    assert_eq!(
        e,
        json!({
            "symbol": "NVDA",
            "name": "NVDA",
            "sector": "Technology",
            "source": "manual",
            "excluded": false,
        })
    );
    let (st, _) = a
        .call(Method::POST, "/universe", Some(json!({ "symbol": "AAPL" })))
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = a
        .call(
            Method::POST,
            "/universe",
            Some(json!({ "symbol": "BAD SYM!" })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Галузь недоступна (фільтр звітності вимкнено) — «Unknown», запит не падає.
    let b = api_with(|d| d.industry = Arc::new(NoIndustry)).await;
    let (st, e) = b
        .call(Method::POST, "/universe", Some(json!({ "symbol": "TSLA" })))
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(e["sector"], "Unknown");
    // Без ключів — так само.
    b.factory.set_missing(true);
    let (st, e) = b
        .call(Method::POST, "/universe", Some(json!({ "symbol": "AMD" })))
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(e["sector"], "Unknown");
}

#[tokio::test]
async fn universe_exclude_delete_and_import() {
    let a = api().await;
    sqlx::query(
        "INSERT INTO universe (symbol, name, sector, source, excluded) \
         VALUES ('KO', 'Coca-Cola', 'Consumer Staples', 'sp500', 0)",
    )
    .execute(a.d.store.pool())
    .await
    .unwrap();

    let (st, e) = a
        .call(
            Method::PUT,
            "/universe/ko",
            Some(json!({ "excluded": true })),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(e["excluded"], true);
    assert_eq!(e["source"], "sp500");
    let (st, _) = a
        .call(
            Method::PUT,
            "/universe/NOPE",
            Some(json!({ "excluded": true })),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = a.call(Method::DELETE, "/universe/KO", None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = a.call(Method::DELETE, "/universe/MSFT", None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = a.call(Method::DELETE, "/universe/MSFT", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // KO уже є (вбудований S&P 500, виключений) — пропускається без змін; порожній сектор —
    // «Unknown».
    let (st, res) = a
        .raw(csv_request(
            "symbol,name,sector\nAMZN,\"Amazon.com, Inc.\",Consumer Discretionary\n\
             KO,Coca-Cola Co,Beverages\nNFLX,Netflix,\n",
        ))
        .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    assert_eq!(res, json!({ "added": 2, "skipped": 1 }));
    let (_, all) = a.get("/universe").await;
    let entry = |s: &str| {
        all.as_array()
            .unwrap()
            .iter()
            .find(|e| e["symbol"] == s)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(entry("AMZN")["name"], "Amazon.com, Inc.");
    assert_eq!(entry("AMZN")["source"], "manual");
    assert_eq!(entry("NFLX")["sector"], "Unknown");
    assert_eq!(
        entry("KO"),
        json!({
            "symbol": "KO",
            "name": "Coca-Cola",
            "sector": "Consumer Staples",
            "source": "sp500",
            "excluded": true,
        })
    );

    // Некоректний рядок — 422, і нічого не вставлено (навіть коректні рядки перед ним).
    let (st, body) = a
        .raw(csv_request(
            "symbol,name,sector\nORCL,Oracle,Technology\nBROKEN,only-two\n",
        ))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!body["details"].as_array().unwrap().is_empty());
    let (st, _) = a
        .raw(csv_request(
            "symbol,name,sector\nIBM,IBM,Technology\nBAD SYM,x,y\n",
        ))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = a.raw(csv_request("ticker\nAMZN\n")).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, all) = a.get("/universe").await;
    let symbols: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["symbol"].as_str())
        .collect();
    assert_eq!(symbols, ["AAPL", "AMZN", "KO", "NFLX", "SPY"]);
}

fn csv_request(csv: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/universe/import")
        .header(header::AUTHORIZATION, bearer())
        .header(header::CONTENT_TYPE, "text/csv")
        .body(Body::from(csv.to_string()))
        .unwrap()
}

#[tokio::test]
async fn account_and_positions_come_from_broker() {
    let a = api().await;
    a.h.broker.set_account(Account {
        equity: dec!(101000.00),
        cash: dec!(50000.00),
        buying_power: dec!(100000.00),
        last_equity: dec!(100500.00),
    });
    let (st, acc) = a.get("/account").await;
    assert_eq!(st, StatusCode::OK, "{acc}");
    assert_eq!(acc["equity"], "101000.00");
    assert_eq!(acc["day_pnl"], "500.00");
    assert!(acc["week_pnl"].is_null());

    // Остання Close-точка до понеділка 2026-11-02 — п'ятниця 2026-10-30.
    a.d.store
        .insert_equity(&EquitySnapshot {
            at: et_to_utc(d(2026, 10, 30), NaiveTime::from_hms_opt(16, 5, 0).unwrap()),
            kind: EquityKind::Close,
            session_date: d(2026, 10, 30),
            equity: 99_000.0,
            cash: 99_000.0,
        })
        .await
        .unwrap();
    let (_, acc) = a.get("/account").await;
    assert_eq!(acc["week_pnl"], "2000.00");

    let p = tracked("AAPL", "trend_pullback", 10, 95.5, Some("stop-1"));
    a.d.store
        .upsert_position(&p, a.d.clock.now())
        .await
        .unwrap();
    a.h.broker
        .set_position(&Symbol::new("AAPL"), 10, dec!(104.00));
    let (st, list) = a.get("/positions").await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(
        list,
        json!([{
            "symbol": "AAPL",
            "strategy": "trend_pullback",
            "qty": 10,
            "entry_price": "100.00",
            "entry_date": "2026-11-01",
            "current_price": "104.00",
            "stop_price": "95.50",
            "take_profit": null,
            "unrealized_pnl": "0.00",
            "sector": "Technology",
        }])
    );
    let (st, orders) = a.get("/orders").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(orders, json!([]));

    // Без ключів — помилка з повідомленням, а не паніка.
    a.factory.set_missing(true);
    a.engine.invalidate_deps();
    let (st, body) = a.get("/account").await;
    assert!(st.is_server_error());
    assert!(!error_text(&body).is_empty());
}

#[tokio::test]
async fn journal_bars_signals_and_scan_queries() {
    let a = api().await;
    let i = pending_entry("AAPL");
    a.d.store.insert_intent(&i).await.unwrap();
    a.call(Method::POST, &format!("/intents/{}/approve", i.id), None)
        .await;
    a.call(Method::POST, "/engine/pause", None).await;

    let (st, all) = a.get("/journal").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, pauses) = a.get("/journal?kinds=pause&limit=10").await;
    assert_eq!(pauses.as_array().unwrap().len(), 1);
    assert_eq!(pauses[0]["kind"], "pause");
    let (_, by_symbol) = a.get("/journal?symbol=aapl").await;
    assert_eq!(by_symbol.as_array().unwrap().len(), 1);
    let (_, today) = a.get("/journal?from=2026-11-04&to=2026-11-04").await;
    assert_eq!(today.as_array().unwrap().len(), 2);
    let (_, later) = a.get("/journal?from=2026-11-05").await;
    assert_eq!(later, json!([]));
    let (st, body) = a.get("/journal?kinds=nonsense").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!error_text(&body).is_empty());
    let (st, _) = a.get("/journal?from=04.11.2026").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let start = a.h.start_date();
    a.d.store
        .upsert_bars(&Symbol::new("MSFT"), &flat_bars(start, N_BARS))
        .await
        .unwrap();
    let (st, bars) = a.get("/bars/msft?from=2026-11-02&to=2026-11-04").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(bars.as_array().unwrap().len(), 3);
    assert_eq!(bars[0]["date"], "2026-11-02");

    let (st, signals) = a.get("/signals?date=2026-11-04").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(signals, json!([]));
    let (st, _) = a.get("/signals").await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, scan) = a.get("/scan?date=2026-11-04").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(scan, json!([]));
}

#[tokio::test]
async fn history_job_is_idle_then_single() {
    let a = api().await;
    let (st, job) = a.get("/history/job").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(job["status"], "idle");

    let held = a.engine.jobs().try_history().unwrap();
    let (st, _) = a
        .call(
            Method::POST,
            "/history/download",
            Some(json!({ "from": "2016-01-01" })),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    drop(held);

    let mut rx = a.d.bus.subscribe();
    let (st, job) = a
        .call(
            Method::POST,
            "/history/download",
            Some(json!({ "from": "2026-01-02" })),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["status"], "running");
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(10);
    let job = loop {
        let (_, job) = a.get("/history/job").await;
        if job["status"] != "running" {
            break job;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    };
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["progress"], 1.0);
    // Свічки фейкових даних збережено.
    let spy =
        a.d.store
            .bars(&Symbol::new("SPY"), d(2026, 1, 2), a.h.last_date())
            .await
            .unwrap();
    assert!(!spy.is_empty());
    let mut progress = 0;
    while let Ok(e) = rx.try_recv() {
        if matches!(e, EngineEventDto::HistoryProgress { .. }) {
            progress += 1;
        }
    }
    assert!(progress >= 2, "події прогресу: {progress}");

    let (st, _) = a
        .call(
            Method::POST,
            "/history/download",
            Some(json!({ "from": "2030-01-01" })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
}
