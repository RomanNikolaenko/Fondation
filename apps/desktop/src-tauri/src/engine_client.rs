//! HTTP/WebSocket-клієнт локального API рушія.

use std::sync::Arc;
use std::time::Duration;

use fond_api::dto::ApiErrorDto;
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, HeaderValue};

/// Помилка клієнта рушія. Серіалізується в UI як `{ "kind": ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientError {
    /// Рушій не запущено або він не відповідає.
    Unreachable { message: String },
    /// Рушій повернув код помилки з [`ApiErrorDto`].
    Http {
        status: u16,
        message: String,
        details: Vec<String>,
    },
    /// Відповідь не вдалося розібрати.
    Decode { message: String },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable { message } => write!(f, "рушій недоступний: {message}"),
            Self::Http {
                status,
                message,
                details,
            } => {
                write!(f, "HTTP {status}: {message}")?;
                if !details.is_empty() {
                    write!(f, " ({})", details.join("; "))?;
                }
                Ok(())
            }
            Self::Decode { message } => write!(f, "некоректна відповідь рушія: {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Клієнт до `http://127.0.0.1:{port}` з токеном `Authorization: Bearer`.
#[derive(Clone)]
pub struct EngineClient {
    port: u16,
    token: Arc<str>,
    http: Client,
}

impl std::fmt::Debug for EngineClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineClient")
            .field("port", &self.port)
            .field("token", &"****")
            .finish()
    }
}

impl EngineClient {
    pub fn new(port: u16, token: String) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client будується завжди");
        Self {
            port,
            token: Arc::from(token),
            http,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.send::<(), T>(Method::GET, path, None).await
    }

    pub async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ClientError> {
        let mut req = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&*self.token);
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await.map_err(|e| ClientError::Unreachable {
            message: e.to_string(),
        })?;
        decode(resp).await
    }

    /// `text/csv` без JSON-обгортки (`POST /universe/import`).
    pub async fn send_text<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &str,
    ) -> Result<T, ClientError> {
        let resp = self
            .http
            .post(self.url(path))
            .bearer_auth(&*self.token)
            .header(reqwest::header::CONTENT_TYPE, "text/csv; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ClientError::Unreachable {
                message: e.to_string(),
            })?;
        decode(resp).await
    }

    /// Запит на WebSocket `/events` з тим самим токеном.
    pub fn ws_request(&self) -> Request<()> {
        let uri = format!("ws://127.0.0.1:{}/events", self.port);
        let mut req = uri
            .into_client_request()
            .expect("ws-адреса з коректним портом завжди валідна");
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            req.headers_mut().insert(AUTHORIZATION, value);
        }
        req
    }
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, ClientError> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| ClientError::Unreachable {
        message: e.to_string(),
    })?;
    if !status.is_success() {
        let (message, details) = match serde_json::from_slice::<ApiErrorDto>(&bytes) {
            Ok(err) => (err.error, err.details),
            Err(_) => (String::from_utf8_lossy(&bytes).into_owned(), Vec::new()),
        };
        return Err(ClientError::Http {
            status: status.as_u16(),
            message,
            details,
        });
    }
    if status == StatusCode::NO_CONTENT || bytes.is_empty() {
        // `()` і `Option<T>` розбираються з `null`.
        return serde_json::from_str("null").map_err(|e| ClientError::Decode {
            message: e.to_string(),
        });
    }
    serde_json::from_slice(&bytes).map_err(|e| ClientError::Decode {
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use fond_api::dto::StatusDto;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn status_body() -> serde_json::Value {
        json!({
            "engine_mode": { "state": "running", "reason": null },
            "account_mode": "paper",
            "execution_mode": "approval",
            "needs_setup": [],
            "next_step": null,
            "last_reconcile_at": null,
            "version": "0.1.0",
            "now": "2026-11-05T14:00:00Z"
        })
    }

    #[tokio::test]
    async fn client_sends_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .and(header("authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_body()))
            .mount(&server)
            .await;

        let client = EngineClient::new(server.address().port(), "t".into());
        let status: StatusDto = client.get("/status").await.unwrap();
        assert_eq!(status.version, "0.1.0");
    }

    #[tokio::test]
    async fn client_maps_422_to_http_error_with_details() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/config"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "error": "Некоректна конфігурація",
                "details": ["api_port = 0"]
            })))
            .mount(&server)
            .await;

        let client = EngineClient::new(server.address().port(), "t".into());
        let err = client
            .send::<_, serde_json::Value>(Method::PUT, "/config", Some(&json!({})))
            .await
            .unwrap_err();
        match err {
            ClientError::Http {
                status,
                message,
                details,
            } => {
                assert_eq!(status, 422);
                assert_eq!(message, "Некоректна конфігурація");
                assert_eq!(details, ["api_port = 0"]);
            }
            other => panic!("очікувалася Http, отримано {other:?}"),
        }
    }

    #[tokio::test]
    async fn client_maps_204_to_unit() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/universe/AAA"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = EngineClient::new(server.address().port(), "t".into());
        client
            .send::<(), ()>(Method::DELETE, "/universe/AAA", None)
            .await
            .unwrap();
    }
}
