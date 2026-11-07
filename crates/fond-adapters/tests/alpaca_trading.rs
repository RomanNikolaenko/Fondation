use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use fond_adapters::alpaca::{AlpacaClient, AlpacaConfig};
use fond_adapters::http::Secret;
use fond_core::error::BrokerError;
use fond_core::orders::{OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce};
use fond_core::traits::{Broker, Clock};
use fond_core::types::Symbol;
use rust_decimal_macros::dec;
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 5, 12, 0, 0).unwrap()
    }
}

fn client(server: &MockServer) -> AlpacaClient {
    let base = Url::parse(&server.uri()).unwrap();
    let cfg = AlpacaConfig {
        trading_base: base.clone(),
        data_base: base,
        key_id: Secret::new("KEY123456"),
        secret: Secret::new("SECRET987654"),
    };
    AlpacaClient::new(cfg, Arc::new(FixedClock))
}

fn order_json(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "client_order_id": "fond-20261105-trend_pullback-AAPL-entry",
        "symbol": "AAPL",
        "side": "buy",
        "type": "limit",
        "qty": "148",
        "filled_qty": "0",
        "filled_avg_price": null,
        "limit_price": "101.00",
        "stop_price": null,
        "status": status,
        "legs": null,
        "filled_at": null
    })
}

fn oto_req() -> OrderRequest {
    OrderRequest {
        client_order_id: "fond-20261105-trend_pullback-AAPL-entry".into(),
        symbol: Symbol::new("AAPL"),
        side: Side::Buy,
        qty: 148,
        order_type: OrderType::Limit,
        limit_price: Some(dec!(101.00)),
        stop_price: None,
        tif: TimeInForce::Day,
        class: OrderClass::Oto,
        stop_loss: Some(dec!(95.00)),
        take_profit: None,
    }
}

fn account_body() -> Value {
    json!({"equity": "1", "cash": "1", "buying_power": "1", "last_equity": "1"})
}

#[tokio::test]
async fn account_parses_string_numbers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "equity": "100000.5", "cash": "50000", "buying_power": "200000.25",
            "last_equity": "99000", "status": "ACTIVE"
        })))
        .mount(&server)
        .await;
    let a = client(&server).account().await.unwrap();
    assert_eq!(a.equity, dec!(100000.5));
    assert_eq!(a.cash, dec!(50000));
    assert_eq!(a.buying_power, dec!(200000.25));
    assert_eq!(a.last_equity, dec!(99000));
}

#[tokio::test]
async fn positions_parse() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "symbol": "AAPL", "qty": "10", "avg_entry_price": "150.25",
            "market_value": "1600", "current_price": "160"
        }])))
        .mount(&server)
        .await;
    let p = client(&server).positions().await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].symbol, Symbol::new("AAPL"));
    assert_eq!(p[0].qty, 10);
    assert_eq!(p[0].avg_entry_price, dec!(150.25));
    assert_eq!(p[0].current_price, dec!(160));
}

#[tokio::test]
async fn open_orders_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/orders"))
        .and(query_param("status", "open"))
        .and(query_param("nested", "true"))
        .and(query_param("limit", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([order_json("o1", "new")])))
        .expect(1)
        .mount(&server)
        .await;
    let o = client(&server).open_orders().await.unwrap();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].status, OrderStatus::New);
    assert_eq!(o[0].qty, 148);
    assert_eq!(o[0].limit_price, Some(dec!(101.00)));
    assert!(o[0].legs.is_empty());
}

#[tokio::test]
async fn submit_oto_sends_expected_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .and(body_json(json!({
            "symbol": "AAPL", "qty": "148", "side": "buy", "type": "limit",
            "time_in_force": "day", "limit_price": "101.00",
            "client_order_id": "fond-20261105-trend_pullback-AAPL-entry",
            "order_class": "oto", "stop_loss": {"stop_price": "95.00"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o1", "accepted")))
        .expect(1)
        .mount(&server)
        .await;
    let o = client(&server).submit(&oto_req()).await.unwrap();
    assert_eq!(o.id, "o1");
    assert_eq!(o.status, OrderStatus::Accepted);
}

#[tokio::test]
async fn submit_bracket_includes_take_profit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .and(body_json(json!({
            "symbol": "AAPL", "qty": "148", "side": "buy", "type": "limit",
            "time_in_force": "day", "limit_price": "101.00",
            "client_order_id": "fond-20261105-trend_pullback-AAPL-entry",
            "order_class": "bracket",
            "stop_loss": {"stop_price": "95.00"},
            "take_profit": {"limit_price": "110.00"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o1", "new")))
        .expect(1)
        .mount(&server)
        .await;
    let mut r = oto_req();
    r.class = OrderClass::Bracket;
    r.take_profit = Some(dec!(110.00));
    client(&server).submit(&r).await.unwrap();
}

#[tokio::test]
async fn exit_is_market_opg_sell() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .and(body_json(json!({
            "symbol": "AAPL", "qty": "148", "side": "sell", "type": "market",
            "time_in_force": "opg", "client_order_id": "fond-20261106-x-AAPL-exit"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o2", "accepted")))
        .expect(1)
        .mount(&server)
        .await;
    let r = OrderRequest {
        client_order_id: "fond-20261106-x-AAPL-exit".into(),
        symbol: Symbol::new("AAPL"),
        side: Side::Sell,
        qty: 148,
        order_type: OrderType::Market,
        limit_price: None,
        stop_price: None,
        tif: TimeInForce::Opg,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    };
    client(&server).submit(&r).await.unwrap();
}

#[tokio::test]
async fn simple_stop_sends_stop_price() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .and(body_json(json!({
            "symbol": "AAPL", "qty": "5", "side": "sell", "type": "stop",
            "time_in_force": "gtc", "stop_price": "95.50", "client_order_id": "c1"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o3", "new")))
        .expect(1)
        .mount(&server)
        .await;
    let r = OrderRequest {
        client_order_id: "c1".into(),
        symbol: Symbol::new("AAPL"),
        side: Side::Sell,
        qty: 5,
        order_type: OrderType::Stop,
        limit_price: None,
        stop_price: Some(dec!(95.50)),
        tif: TimeInForce::Gtc,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    };
    client(&server).submit(&r).await.unwrap();
}

#[tokio::test]
async fn duplicate_client_order_id_maps() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(
                json!({"code": 40010001, "message": "client_order_id must be unique"}),
            ),
        )
        .mount(&server)
        .await;
    let e = client(&server).submit(&oto_req()).await.unwrap_err();
    assert_eq!(e, BrokerError::DuplicateClientOrderId);
}

#[tokio::test]
async fn other_422_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_json(json!({"message": "insufficient buying power"})),
        )
        .mount(&server)
        .await;
    let e = client(&server).submit(&oto_req()).await.unwrap_err();
    assert_eq!(
        e,
        BrokerError::Rejected {
            status: 422,
            message: "insufficient buying power".into()
        }
    );
}

#[tokio::test]
async fn status_codes_map() {
    for (code, expect) in [
        (401u16, BrokerError::Unauthorized),
        (403, BrokerError::Unauthorized),
        (404, BrokerError::NotFound),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/orders/x"))
            .respond_with(ResponseTemplate::new(code).set_body_json(json!({"message": "m"})))
            .mount(&server)
            .await;
        assert_eq!(
            client(&server).order("x").await.unwrap_err(),
            expect,
            "{code}"
        );
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).submit(&oto_req()).await.unwrap_err(),
        BrokerError::Transient(_)
    ));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad"))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).submit(&oto_req()).await.unwrap_err(),
        BrokerError::Rejected {
            status: 400,
            message: "bad".into()
        }
    );
}

#[tokio::test]
async fn connect_error_is_transient() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let base = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let cfg = AlpacaConfig {
        trading_base: base.clone(),
        data_base: base,
        key_id: Secret::new("KEY123456"),
        secret: Secret::new("SECRET987654"),
    };
    let c = AlpacaClient::new(cfg, Arc::new(FixedClock));
    let e = c.submit(&oto_req()).await.unwrap_err();
    assert!(matches!(e, BrokerError::Transient(_)), "{e:?}");
}

#[tokio::test]
async fn order_by_client_id_404_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/orders:by_client_order_id"))
        .and(query_param("client_order_id", "nope"))
        .and(query_param("nested", "true"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"message": "order not found"})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).order_by_client_id("nope").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn order_by_client_id_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/orders:by_client_order_id"))
        .and(query_param("client_order_id", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o9", "filled")))
        .mount(&server)
        .await;
    let o = client(&server)
        .order_by_client_id("abc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(o.id, "o9");
    assert_eq!(o.status, OrderStatus::Filled);
}

#[tokio::test]
async fn unknown_status_maps_to_other() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/orders/o1"))
        .and(query_param("nested", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o1", "pending_replace")))
        .mount(&server)
        .await;
    let o = client(&server).order("o1").await.unwrap();
    assert_eq!(o.status, OrderStatus::Other("pending_replace".into()));
}

#[tokio::test]
async fn nested_legs_parsed() {
    let server = MockServer::start().await;
    let mut parent = order_json("p1", "filled");
    parent["filled_qty"] = json!("148");
    parent["filled_avg_price"] = json!("100.75");
    parent["filled_at"] = json!("2026-11-05T14:30:01.123456Z");
    parent["legs"] = json!([{
        "id": "l1", "client_order_id": "leg-1", "symbol": "AAPL", "side": "sell",
        "type": "stop", "qty": "148", "filled_qty": "0", "filled_avg_price": null,
        "limit_price": null, "stop_price": "95.00", "status": "held", "legs": null,
        "filled_at": null
    }]);
    Mock::given(method("GET"))
        .and(path("/v2/orders/p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(parent))
        .mount(&server)
        .await;
    let o = client(&server).order("p1").await.unwrap();
    assert_eq!(o.filled_qty, 148);
    assert_eq!(o.filled_avg_price, Some(dec!(100.75)));
    assert_eq!(
        o.filled_at,
        Some(
            Utc.with_ymd_and_hms(2026, 11, 5, 14, 30, 1).unwrap()
                + chrono::Duration::microseconds(123_456)
        )
    );
    assert_eq!(o.legs.len(), 1);
    assert_eq!(o.legs[0].order_type, OrderType::Stop);
    assert_eq!(o.legs[0].side, Side::Sell);
    assert_eq!(o.legs[0].stop_price, Some(dec!(95.00)));
    assert_eq!(o.legs[0].status, OrderStatus::Held);
}

#[tokio::test]
async fn unknown_order_type_maps_to_other() {
    let server = MockServer::start().await;
    let mut o = order_json("o1", "new");
    o["type"] = json!("trailing_stop");
    Mock::given(method("GET"))
        .and(path("/v2/orders/o1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(o))
        .mount(&server)
        .await;
    let o = client(&server).order("o1").await.unwrap();
    assert_eq!(o.order_type, OrderType::Other("trailing_stop".into()));
}

#[tokio::test]
async fn open_orders_survives_unknown_type_and_notional() {
    let server = MockServer::start().await;
    let mut oto = order_json("o1", "new");
    oto["legs"] = json!([{
        "id": "l1", "client_order_id": "leg-1", "symbol": "AAPL", "side": "sell",
        "type": "stop", "qty": "148", "filled_qty": "0", "limit_price": null,
        "stop_price": "95.00", "status": "held", "legs": null
    }]);
    let mut stop_limit = order_json("o2", "accepted");
    stop_limit["type"] = json!("stop_limit");
    let mut notional = order_json("o3", "accepted");
    notional["type"] = json!("market");
    notional["qty"] = Value::Null;
    notional["filled_qty"] = Value::Null;
    notional["notional"] = json!("500");
    Mock::given(method("GET"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([oto, stop_limit, notional])))
        .mount(&server)
        .await;
    let o = client(&server).open_orders().await.unwrap();
    assert_eq!(o.len(), 3);
    assert_eq!(o[0].order_type, OrderType::Limit);
    assert_eq!(o[0].legs.len(), 1);
    assert_eq!(o[1].order_type, OrderType::Other("stop_limit".into()));
    assert_eq!(o[1].qty, 148);
    assert_eq!(o[2].order_type, OrderType::Market);
    assert_eq!(o[2].qty, 0);
    assert_eq!(o[2].filled_qty, 0);
}

#[tokio::test]
async fn auth_headers_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .and(header("APCA-API-KEY-ID", "KEY123456"))
        .and(header("APCA-API-SECRET-KEY", "SECRET987654"))
        .respond_with(ResponseTemplate::new(200).set_body_json(account_body()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).account().await.unwrap();
}

#[tokio::test]
async fn cancel_accepts_204_and_200() {
    for code in [204u16, 200] {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v2/orders/o1"))
            .respond_with(ResponseTemplate::new(code))
            .expect(1)
            .mount(&server)
            .await;
        client(&server).cancel("o1").await.unwrap();
    }
}

#[tokio::test]
async fn cancel_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/v2/orders/o1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).cancel("o1").await.unwrap_err(),
        BrokerError::NotFound
    );
}

#[tokio::test]
async fn replace_stop_patches_and_returns_new_order() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/v2/orders/o1"))
        .and(body_json(json!({"stop_price": "96.50"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(order_json("o1b", "accepted")))
        .expect(1)
        .mount(&server)
        .await;
    let o = client(&server)
        .replace_stop("o1", dec!(96.50))
        .await
        .unwrap();
    assert_eq!(o.id, "o1b");
}

#[tokio::test]
async fn get_retries_on_503_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(account_body()))
        .with_priority(2)
        .mount(&server)
        .await;
    client(&server).account().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn submit_not_retried_on_503() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/orders"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let e = client(&server).submit(&oto_req()).await.unwrap_err();
    assert!(matches!(e, BrokerError::Transient(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn config_debug_masks_secrets() {
    let c = AlpacaConfig::paper(Secret::new("KEY123456"), Secret::new("SECRET987654"));
    let d = format!("{c:?}");
    assert!(!d.contains("KEY123456") && !d.contains("SECRET987654"));
    assert_eq!(c.trading_base.as_str(), "https://paper-api.alpaca.markets/");
    assert_eq!(c.data_base.as_str(), "https://data.alpaca.markets/");
}

#[tokio::test]
#[ignore = "real Alpaca paper; needs ALPACA_KEY_ID/ALPACA_SECRET"]
async fn paper_oto_accepted_outside_hours() {
    let key = std::env::var("ALPACA_KEY_ID").expect("ALPACA_KEY_ID");
    let secret = std::env::var("ALPACA_SECRET").expect("ALPACA_SECRET");
    let cfg = AlpacaConfig::paper(Secret::new(key.clone()), Secret::new(secret.clone()));
    let http = reqwest::Client::new();
    let url = cfg.data_base.join("v2/stocks/SPY/trades/latest").unwrap();
    let v: Value = http
        .get(url)
        .header("APCA-API-KEY-ID", &key)
        .header("APCA-API-SECRET-KEY", &secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let last = v["trade"]["p"].as_f64().expect("last trade price");
    let limit = rust_decimal::Decimal::from_f64_retain(last * 0.70)
        .unwrap()
        .round_dp(2);
    let stop = (limit * dec!(0.95)).round_dp(2);
    let client = AlpacaClient::new(cfg, Arc::new(FixedClock));
    let req = OrderRequest {
        client_order_id: format!("fond-test-{}", Utc::now().timestamp()),
        symbol: Symbol::new("SPY"),
        side: Side::Buy,
        qty: 1,
        order_type: OrderType::Limit,
        limit_price: Some(limit),
        stop_price: None,
        tif: TimeInForce::Day,
        class: OrderClass::Oto,
        stop_loss: Some(stop),
        take_profit: None,
    };
    let o = client.submit(&req).await.unwrap();
    assert!(
        matches!(
            o.status,
            OrderStatus::Accepted | OrderStatus::New | OrderStatus::PendingNew
        ),
        "{:?}",
        o.status
    );
    client.cancel(&o.id).await.unwrap();
}
