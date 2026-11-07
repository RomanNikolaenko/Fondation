use chrono::NaiveDate;
use fond_adapters::finnhub::FinnhubClient;
use fond_adapters::http::Secret;
use fond_core::error::DataError;
use fond_core::traits::EarningsCalendar;
use fond_core::types::Symbol;
use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> FinnhubClient {
    FinnhubClient::new(
        Url::parse(&server.uri()).unwrap(),
        Secret::new("TOKEN123456"),
    )
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn aapl() -> Symbol {
    Symbol::new("AAPL")
}

#[tokio::test]
async fn earnings_returns_earliest_date() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/earnings"))
        .and(query_param("from", "2026-11-09"))
        .and(query_param("to", "2026-11-23"))
        .and(query_param("symbol", "AAPL"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "earningsCalendar": [
                {"symbol": "AAPL", "date": "2026-11-20"},
                {"symbol": "MSFT", "date": "2026-11-10"},
                {"symbol": "AAPL", "date": "not-a-date"},
                {"symbol": "AAPL", "date": "2026-11-12"},
                {"symbol": "AAPL", "date": "2026-11-01"},
                {"symbol": "AAPL", "date": "2026-12-30"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let r = client(&server)
        .next_earnings(&aapl(), d(2026, 11, 9), d(2026, 11, 23))
        .await
        .unwrap();
    assert_eq!(r, Some(d(2026, 11, 12)));
}

#[tokio::test]
async fn empty_calendar_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/earnings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"earningsCalendar": []})))
        .mount(&server)
        .await;
    let r = client(&server)
        .next_earnings(&aapl(), d(2026, 11, 9), d(2026, 11, 23))
        .await
        .unwrap();
    assert_eq!(r, None);
}

#[tokio::test]
async fn premium_403_is_not_available() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/earnings"))
        .respond_with(ResponseTemplate::new(403).set_body_string("You don't have access"))
        .expect(1)
        .mount(&server)
        .await;
    let e = client(&server)
        .next_earnings(&aapl(), d(2026, 11, 9), d(2026, 11, 23))
        .await
        .unwrap_err();
    assert_eq!(e, DataError::NotAvailable("premium".into()));
}

#[tokio::test]
async fn unauthorized_on_401_and_retries_429() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stock/profile2"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let e = client(&server).industry(&aapl()).await.unwrap_err();
    assert_eq!(e, DataError::Unauthorized);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stock/profile2"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stock/profile2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"finnhubIndustry": "Technology"})),
        )
        .with_priority(5)
        .mount(&server)
        .await;
    let r = client(&server).industry(&aapl()).await.unwrap();
    assert_eq!(r.as_deref(), Some("Technology"));
}

#[tokio::test]
async fn token_in_header_not_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/earnings"))
        .and(header("X-Finnhub-Token", "TOKEN123456"))
        .and(query_param_is_missing("token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"earningsCalendar": []})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .next_earnings(&aapl(), d(2026, 11, 9), d(2026, 11, 23))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs[0].url.query().unwrap_or("").contains("TOKEN123456"));
}

#[tokio::test]
async fn industry_parse() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stock/profile2"))
        .and(query_param("symbol", "AAPL"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"finnhubIndustry": "Technology"})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).industry(&aapl()).await.unwrap().as_deref(),
        Some("Technology")
    );

    for (sym, body) in [
        ("EMPTY", json!({})),
        ("BLANK", json!({"finnhubIndustry": ""})),
    ] {
        Mock::given(method("GET"))
            .and(path("/stock/profile2"))
            .and(query_param("symbol", sym))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        assert_eq!(
            client(&server).industry(&Symbol::new(sym)).await.unwrap(),
            None
        );
    }
}
