//! Токен доступу до API і адреса, на якій слухає сервер.

use std::net::{Ipv4Addr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fond_adapters::http::Secret;
use fond_adapters::secrets::{SecretError, SecretStore};
use fond_core::keys::SecretKey;
use subtle::ConstantTimeEq;

use super::{ApiError, ApiState};

/// Сервер слухає лише loopback: `127.0.0.1:port`.
pub fn bind_addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Токен API з Credential Manager; якщо його немає (або він порожній) — 32 випадкові байти в
/// hex, які зберігаються там же.
pub async fn ensure_api_token(s: &dyn SecretStore) -> Result<Secret, SecretError> {
    if let Some(t) = s.get(SecretKey::EngineApiToken)?
        && !t.expose().trim().is_empty()
    {
        return Ok(t);
    }
    let bytes: [u8; 32] = rand::random();
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    s.set(SecretKey::EngineApiToken, &token)?;
    Ok(Secret::new(token))
}

/// Значення після `Bearer ` у заголовку `Authorization`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Middleware: без правильного `Authorization: Bearer <token>` — 401. Порівняння токенів —
/// за сталий час (`subtle`). Діє й на запит оновлення до WebSocket.
pub async fn require_token(State(st): State<ApiState>, req: Request, next: Next) -> Response {
    let ok = bearer(req.headers())
        .is_some_and(|t| bool::from(t.as_bytes().ct_eq(st.token.expose().as_bytes())));
    if ok {
        return next.run(req).await;
    }
    let mut resp = ApiError::unauthorized("Потрібен правильний токен доступу").into_response();
    resp.headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    resp
}
