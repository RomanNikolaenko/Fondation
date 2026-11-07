//! `Broker` поверх REST API Alpaca. Повторюються лише GET; `submit`/`cancel`/`replace_stop`
//! виконуються однією спробою: ідемпотентністю опікується рушій.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fond_core::error::BrokerError;
use fond_core::orders::{BrokerOrder, OrderClass, OrderRequest, OrderType, Side, TimeInForce};
use fond_core::portfolio::{Account, BrokerPosition};
use fond_core::traits::{Broker, Clock};
use reqwest::{Method, StatusCode};
use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use url::Url;

use super::models::{WireAccount, WireOrder, WirePosition};
use crate::http::{
    RateLimiter, RawError, RetryPolicy, Secret, http_client, join_url, send, with_retry,
};

const PAPER_TRADING: &str = "https://paper-api.alpaca.markets";
const DATA: &str = "https://data.alpaca.markets";
const DUPLICATE_MARKER: &str = "client_order_id must be unique";

#[derive(Clone)]
pub struct AlpacaConfig {
    pub trading_base: Url,
    pub data_base: Url,
    pub key_id: Secret,
    pub secret: Secret,
}

impl AlpacaConfig {
    pub fn paper(key_id: Secret, secret: Secret) -> Self {
        Self {
            trading_base: Url::parse(PAPER_TRADING).expect("static url"),
            data_base: Url::parse(DATA).expect("static url"),
            key_id,
            secret,
        }
    }
}

impl fmt::Debug for AlpacaConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlpacaConfig")
            .field("trading_base", &self.trading_base.as_str())
            .field("data_base", &self.data_base.as_str())
            .field("key_id", &self.key_id)
            .field("secret", &self.secret)
            .finish()
    }
}

pub struct AlpacaClient {
    http: reqwest::Client,
    cfg: AlpacaConfig,
    clock: Arc<dyn Clock>,
    limiter: RateLimiter,
    retry: RetryPolicy,
}

impl AlpacaClient {
    pub fn new(cfg: AlpacaConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            http: http_client(Duration::from_secs(15)),
            cfg,
            clock,
            limiter: RateLimiter::new(190),
            retry: RetryPolicy::default(),
        }
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, BrokerError> {
        join_url(&self.cfg.trading_base, path, query).map_err(BrokerError::Other)
    }

    pub(super) fn trading_base(&self) -> &Url {
        &self.cfg.trading_base
    }

    pub(super) fn data_base(&self) -> &Url {
        &self.cfg.data_base
    }

    pub(super) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    /// Одна спроба запиту до готового URL; повертає тіло успішної відповіді.
    pub(super) async fn exec(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<String, RawError> {
        let headers = [
            ("APCA-API-KEY-ID", self.cfg.key_id.expose()),
            ("APCA-API-SECRET-KEY", self.cfg.secret.expose()),
        ];
        send(&self.http, &self.limiter, method, url, &headers, body).await
    }

    /// GET з повторами для тимчасових збоїв; помилку відображає виклик.
    pub(super) async fn get_raw(&self, url: &Url) -> Result<String, RawError> {
        with_retry(&self.retry, || self.exec(Method::GET, url.clone(), None)).await
    }

    async fn send_once(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(StatusCode, String), BrokerError> {
        let text = self
            .exec(method, self.url(path, query)?, body)
            .await
            .map_err(into_broker)?;
        Ok((StatusCode::OK, text))
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, BrokerError> {
        let text = self
            .get_raw(&self.url(path, query)?)
            .await
            .map_err(into_broker)?;
        parse(&text)
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &Value,
    ) -> Result<T, BrokerError> {
        let (_, text) = self.send_once(method, path, &[], Some(body)).await?;
        parse(&text)
    }
}

fn into_broker(e: RawError) -> BrokerError {
    match e {
        RawError::Network(m) => BrokerError::Transient(m),
        RawError::Status(s, t) => map_error(s, &t),
    }
}

pub(super) fn message_text(text: &str) -> String {
    message_of(text)
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, BrokerError> {
    serde_json::from_str(text)
        .map_err(|e| BrokerError::Other(format!("unparseable broker response: {e}")))
}

fn message_of(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| text.to_string())
}

fn map_error(status: StatusCode, text: &str) -> BrokerError {
    let code = status.as_u16();
    match code {
        401 | 403 => BrokerError::Unauthorized,
        404 => BrokerError::NotFound,
        422 if message_of(text).to_lowercase().contains(DUPLICATE_MARKER) => {
            BrokerError::DuplicateClientOrderId
        }
        429 | 500..=599 => BrokerError::Transient(format!("http {code}: {}", message_of(text))),
        _ => BrokerError::Rejected {
            status: code,
            message: message_of(text),
        },
    }
}

fn side_str(s: Side) -> &'static str {
    match s {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn type_str(t: &OrderType) -> String {
    String::from(t.clone())
}

fn tif_str(t: TimeInForce) -> &'static str {
    match t {
        TimeInForce::Day => "day",
        TimeInForce::Gtc => "gtc",
        TimeInForce::Opg => "opg",
    }
}

fn order_body(req: &OrderRequest) -> Value {
    let mut m = Map::new();
    m.insert("symbol".into(), json!(req.symbol.as_str()));
    m.insert("qty".into(), json!(req.qty.to_string()));
    m.insert("side".into(), json!(side_str(req.side)));
    m.insert("type".into(), json!(type_str(&req.order_type)));
    m.insert("time_in_force".into(), json!(tif_str(req.tif)));
    if let Some(p) = req.limit_price {
        m.insert("limit_price".into(), json!(p.to_string()));
    }
    if let Some(p) = req.stop_price {
        m.insert("stop_price".into(), json!(p.to_string()));
    }
    m.insert("client_order_id".into(), json!(req.client_order_id));
    match req.class {
        OrderClass::Simple => {}
        OrderClass::Oto => {
            m.insert("order_class".into(), json!("oto"));
        }
        OrderClass::Bracket => {
            m.insert("order_class".into(), json!("bracket"));
        }
    }
    if let Some(p) = req.stop_loss {
        m.insert("stop_loss".into(), json!({"stop_price": p.to_string()}));
    }
    if let Some(p) = req.take_profit {
        m.insert("take_profit".into(), json!({"limit_price": p.to_string()}));
    }
    Value::Object(m)
}

#[async_trait]
impl Broker for AlpacaClient {
    async fn account(&self) -> Result<Account, BrokerError> {
        let w: WireAccount = self.get("v2/account", &[]).await?;
        Ok(w.into())
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let w: Vec<WirePosition> = self.get("v2/positions", &[]).await?;
        w.into_iter().map(BrokerPosition::try_from).collect()
    }

    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError> {
        let w: Vec<WireOrder> = self
            .get(
                "v2/orders",
                &[("status", "open"), ("nested", "true"), ("limit", "500")],
            )
            .await?;
        w.into_iter().map(BrokerOrder::try_from).collect()
    }

    async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError> {
        let w: WireOrder = self
            .get(&format!("v2/orders/{id}"), &[("nested", "true")])
            .await?;
        w.try_into()
    }

    async fn order_by_client_id(
        &self,
        client_order_id: &str,
    ) -> Result<Option<BrokerOrder>, BrokerError> {
        let r: Result<WireOrder, _> = self
            .get(
                "v2/orders:by_client_order_id",
                &[("client_order_id", client_order_id), ("nested", "true")],
            )
            .await;
        match r {
            Ok(w) => Ok(Some(w.try_into()?)),
            Err(BrokerError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
        let w: WireOrder = self
            .send_json(Method::POST, "v2/orders", &order_body(req))
            .await?;
        w.try_into()
    }

    async fn cancel(&self, order_id: &str) -> Result<(), BrokerError> {
        // 200 і 204 — успіх; тіло не потрібне.
        self.send_once(Method::DELETE, &format!("v2/orders/{order_id}"), &[], None)
            .await
            .map(|_| ())
    }

    async fn replace_stop(
        &self,
        order_id: &str,
        new_stop: Decimal,
    ) -> Result<BrokerOrder, BrokerError> {
        let body = json!({"stop_price": new_stop.to_string()});
        let w: WireOrder = self
            .send_json(Method::PATCH, &format!("v2/orders/{order_id}"), &body)
            .await?;
        w.try_into()
    }
}
