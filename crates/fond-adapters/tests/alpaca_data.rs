use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use fond_adapters::alpaca::{AlpacaClient, AlpacaConfig};
use fond_adapters::http::Secret;
use fond_core::error::DataError;
use fond_core::testkit::FakeClock;
use fond_core::traits::{MarketData, NewsSource};
use fond_core::types::Symbol;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_at(server: &MockServer, now: DateTime<Utc>) -> AlpacaClient {
    let base = Url::parse(&server.uri()).unwrap();
    let cfg = AlpacaConfig {
        trading_base: base.clone(),
        data_base: base,
        key_id: Secret::new("KEY123456"),
        secret: Secret::new("SECRET987654"),
    };
    AlpacaClient::new(cfg, Arc::new(FakeClock::new(now)))
}

fn client(server: &MockServer) -> AlpacaClient {
    client_at(server, Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap())
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn bar(t: &str, c: f64) -> serde_json::Value {
    json!({"t": t, "o": 1.0, "h": 2.0, "l": 0.5, "c": c, "v": 1000})
}

#[tokio::test]
async fn bars_pagination_merges_pages() {
    let server = MockServer::start().await;
    // Second page (narrower matcher, higher priority).
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("page_token", "tok2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"AAPL": [bar("2026-11-03T05:00:00Z", 11.0), bar("2026-11-02T05:00:00Z", 10.0)]},
            "next_page_token": null
        })))
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(header("APCA-API-KEY-ID", "KEY123456"))
        .and(query_param("symbols", "AAPL,MSFT"))
        .and(query_param("timeframe", "1Day"))
        .and(query_param("adjustment", "all"))
        .and(query_param("feed", "sip"))
        .and(query_param("limit", "10000"))
        .and(query_param("start", "2026-11-02T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {
                "AAPL": [bar("2026-11-04T05:00:00Z", 12.0)],
                "MSFT": [bar("2026-11-04T05:00:00Z", 20.0)]
            },
            "next_page_token": "tok2"
        })))
        .expect(1)
        .with_priority(5)
        .mount(&server)
        .await;
    let c = client(&server);
    let syms = [Symbol::new("MSFT"), Symbol::new("AAPL")];
    let out = c
        .daily_bars(&syms, d(2026, 11, 2), d(2026, 11, 3))
        .await
        .unwrap();
    let aapl = &out[&Symbol::new("AAPL")];
    let dates: Vec<_> = aapl.iter().map(|b| b.date).collect();
    assert_eq!(dates, vec![d(2026, 11, 2), d(2026, 11, 3), d(2026, 11, 4)]);
    assert_eq!(aapl[0].close, 10.0);
    assert_eq!(out[&Symbol::new("MSFT")].len(), 1);
    assert!(!out.contains_key(&Symbol::new("ZZZ")));
}

#[tokio::test]
async fn bars_end_is_at_least_16_minutes_old() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("end", "2026-11-04T21:04:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"AAPL": [bar("2026-11-04T05:00:00Z", 12.0)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    // 16:20 ET (EST, UTC-5) = 21:20Z
    let now = Utc.with_ymd_and_hms(2026, 11, 4, 21, 20, 0).unwrap();
    let c = client_at(&server, now);
    let out = c
        .daily_bars(&[Symbol::new("AAPL")], d(2026, 11, 3), d(2026, 11, 4))
        .await
        .unwrap();
    assert_eq!(out[&Symbol::new("AAPL")].len(), 1);
}

#[tokio::test]
async fn bars_end_before_start_returns_empty_without_request() {
    let server = MockServer::start().await;
    let now = Utc.with_ymd_and_hms(2026, 11, 4, 0, 5, 0).unwrap();
    let c = client_at(&server, now);
    let out = c
        .daily_bars(&[Symbol::new("AAPL")], d(2026, 11, 4), d(2026, 11, 4))
        .await
        .unwrap();
    assert!(out.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn bars_dates_are_new_york_dates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"AAPL": [
                bar("2026-11-04T05:00:00Z", 1.0),
                bar("2026-11-04T05:00:00Z", 2.0),
                bar("2026-07-01T04:00:00Z", 3.0)
            ]},
            "next_page_token": null
        })))
        .mount(&server)
        .await;
    let c = client(&server);
    let out = c
        .daily_bars(&[Symbol::new("AAPL")], d(2026, 7, 1), d(2026, 11, 4))
        .await
        .unwrap();
    let bars = &out[&Symbol::new("AAPL")];
    // 05:00Z in winter = 00:00 EST; 04:00Z in summer = 00:00 EDT; duplicate date dropped
    let dates: Vec<_> = bars.iter().map(|b| b.date).collect();
    assert_eq!(dates, vec![d(2026, 7, 1), d(2026, 11, 4)]);
}

#[tokio::test]
async fn dotted_symbols_are_url_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", "AAPL,BRK.B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"BRK.B": [bar("2026-11-03T05:00:00Z", 5.0)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/BF.B/trades/latest"))
        .and(query_param("feed", "iex"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "symbol": "BF.B", "trade": {"p": 42.5, "t": "2026-11-04T15:00:00Z"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    // The request order is sorted regardless of the input order.
    let syms = [Symbol::new("BRK.B"), Symbol::new("AAPL")];
    let out = c
        .daily_bars(&syms, d(2026, 11, 3), d(2026, 11, 3))
        .await
        .unwrap();
    assert_eq!(out[&Symbol::new("BRK.B")].len(), 1);
    assert!(!out.contains_key(&Symbol::new("AAPL")));
    let p = c.latest_trade(&Symbol::new("BF.B")).await.unwrap();
    assert_eq!(p, Some(42.5));
}

#[tokio::test]
async fn calendar_parses_half_day() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/calendar"))
        .and(query_param("start", "2026-11-25"))
        .and(query_param("end", "2026-11-27"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"date": "2026-11-25", "open": "09:30", "close": "16:00"},
            {"date": "2026-11-27", "open": "09:30", "close": "13:00"}
        ])))
        .mount(&server)
        .await;
    let c = client(&server);
    let s = c.calendar(d(2026, 11, 25), d(2026, 11, 27)).await.unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].close, NaiveTime::from_hms_opt(16, 0, 0).unwrap());
    assert_eq!(s[1].date, d(2026, 11, 27));
    assert_eq!(s[1].open, NaiveTime::from_hms_opt(9, 30, 0).unwrap());
    assert_eq!(s[1].close, NaiveTime::from_hms_opt(13, 0, 0).unwrap());
}

#[tokio::test]
async fn news_403_is_not_available() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta1/news"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "forbidden"})))
        .mount(&server)
        .await;
    let c = client(&server);
    let since = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let r = c.news(&Symbol::new("AAPL"), since, 5).await;
    assert_eq!(
        r,
        Err(DataError::NotAvailable(
            "news not available on this plan".into()
        ))
    );
}

#[tokio::test]
async fn news_parses_items_and_caps_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta1/news"))
        .and(query_param("symbols", "AAPL"))
        .and(query_param("limit", "50"))
        .and(query_param("sort", "desc"))
        .and(query_param("start", "2026-11-01T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "news": [
                {"headline": "H1", "summary": "S1", "created_at": "2026-11-03T10:00:00Z", "source": "benzinga"},
                {"headline": "H2", "created_at": "2026-11-02T10:00:00Z", "source": "x"}
            ],
            "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let since = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let items = c.news(&Symbol::new("AAPL"), since, 500).await.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].headline, "H1");
    assert_eq!(items[0].summary, "S1");
    assert_eq!(items[0].source, "benzinga");
    assert_eq!(items[1].summary, "");
    assert_eq!(
        items[0].created_at,
        Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap()
    );
}

#[tokio::test]
async fn latest_trade_404_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/ZZZ/trades/latest"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "nope"})))
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(c.latest_trade(&Symbol::new("ZZZ")).await.unwrap(), None);
}

#[tokio::test]
async fn data_401_is_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(c.assets().await, Err(DataError::Unauthorized));
}

#[tokio::test]
async fn assets_parse_tradable_and_active() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/assets"))
        .and(query_param("status", "active"))
        .and(query_param("asset_class", "us_equity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"symbol": "AAPL", "tradable": true, "status": "active"},
            {"symbol": "OLD", "tradable": false, "status": "inactive"}
        ])))
        .mount(&server)
        .await;
    let c = client(&server);
    let a = c.assets().await.unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].symbol, Symbol::new("AAPL"));
    assert!(a[0].tradable && a[0].active);
    assert!(!a[1].tradable && !a[1].active);
}

fn many_symbols(n: usize) -> Vec<Symbol> {
    (0..n).map(|i| Symbol::new(&format!("S{i:03}"))).collect()
}

fn joined(syms: &[Symbol]) -> String {
    syms.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[tokio::test]
async fn bars_batches_of_100_symbols() {
    let server = MockServer::start().await;
    let mut syms = many_symbols(101);
    let first = joined(&syms[..100]);
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", first.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"S000": [bar("2026-11-03T05:00:00Z", 1.0)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", "S100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"S100": [bar("2026-11-03T05:00:00Z", 2.0)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    syms.reverse(); // input order is irrelevant: chunking follows the sorted order
    let out = client(&server)
        .daily_bars(&syms, d(2026, 11, 3), d(2026, 11, 3))
        .await
        .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[&Symbol::new("S000")][0].close, 1.0);
    assert_eq!(out[&Symbol::new("S100")][0].close, 2.0);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn bars_page_token_does_not_leak_into_next_chunk() {
    let server = MockServer::start().await;
    let syms = many_symbols(101);
    let first = joined(&syms[..100]);
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", first.as_str()))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"S000": [bar("2026-11-02T05:00:00Z", 1.0)]}, "next_page_token": "t1"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", first.as_str()))
        .and(query_param("page_token", "t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"S000": [bar("2026-11-03T05:00:00Z", 1.5)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("symbols", "S100"))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"S100": [bar("2026-11-03T05:00:00Z", 2.0)]}, "next_page_token": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let out = client(&server)
        .daily_bars(&syms, d(2026, 11, 2), d(2026, 11, 3))
        .await
        .unwrap();
    assert_eq!(out[&Symbol::new("S000")].len(), 2);
    assert_eq!(out[&Symbol::new("S100")].len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn bars_same_date_on_two_pages_is_deduplicated() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .and(query_param("page_token", "t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"AAPL": [bar("2026-11-03T05:00:00Z", 99.0), bar("2026-11-04T05:00:00Z", 3.0)]},
            "next_page_token": null
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/stocks/bars"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bars": {"AAPL": [bar("2026-11-03T05:00:00Z", 2.0)]}, "next_page_token": "t1"
        })))
        .with_priority(5)
        .mount(&server)
        .await;
    let out = client(&server)
        .daily_bars(&[Symbol::new("AAPL")], d(2026, 11, 3), d(2026, 11, 4))
        .await
        .unwrap();
    let bars = &out[&Symbol::new("AAPL")];
    let dates: Vec<_> = bars.iter().map(|b| b.date).collect();
    assert_eq!(dates, vec![d(2026, 11, 3), d(2026, 11, 4)]);
    assert_eq!(bars[0].close, 2.0);
}
