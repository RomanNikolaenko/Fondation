//! Локальний HTTP/WebSocket API рушія (`127.0.0.1`, `Authorization: Bearer <token>`).
//!
//! Через нього з рушієм говорить лише бекенд Tauri. Усі маршрути, крім `/health`, вимагають
//! токен; кожна помилка — JSON [`ApiErrorDto`].

pub mod auth;
pub mod handlers;
pub mod ws;

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router, middleware};
use fond_adapters::http::Secret;
use fond_adapters::secrets::SecretError;
use fond_api::dto::ApiErrorDto;
use fond_store::StoreError;
use serde::de::DeserializeOwned;
use tokio::sync::watch;

use crate::cycle::CycleError;
use crate::deps::SetupError;
use crate::runtime::Engine;

/// Спільний стан обробників.
#[derive(Clone)]
pub struct ApiState {
    pub engine: Arc<Engine>,
    token: Arc<Secret>,
    /// `true` — рушій зупиняється (WebSocket-з'єднання закриваються).
    shutdown: watch::Receiver<bool>,
}

impl ApiState {
    pub fn new(engine: Arc<Engine>, token: Secret, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            engine,
            token: Arc::new(token),
            shutdown,
        }
    }
}

/// Усі маршрути API. `/health` — без токена, решта — через [`auth::require_token`].
pub fn router(state: ApiState) -> Router {
    use handlers as h;
    let protected = Router::new()
        .route("/status", get(h::status))
        .route("/account", get(h::account))
        .route("/positions", get(h::positions))
        .route("/orders", get(h::orders))
        .route("/signals", get(h::signals))
        .route("/scan", get(h::scan))
        .route("/intents", get(h::intents))
        .route("/intents/{id}/approve", post(h::approve))
        .route("/intents/{id}/reject", post(h::reject))
        .route("/positions/{symbol}/close", post(h::close_position))
        .route("/engine/pause", post(h::pause))
        .route("/engine/resume", post(h::resume))
        .route("/engine/kill", post(h::kill))
        .route("/config", get(h::get_config).put(h::put_config))
        .route("/bars/{symbol}", get(h::bars))
        .route("/journal", get(h::journal))
        .route("/backtests", post(h::start_backtest).get(h::list_backtests))
        .route("/backtests/{id}", get(h::get_backtest))
        .route("/history/download", post(h::start_history))
        .route("/history/job", get(h::history_job))
        .route("/universe", get(h::universe).post(h::add_symbol))
        .route(
            "/universe/{symbol}",
            put(h::set_excluded).delete(h::remove_symbol),
        )
        .route("/universe/import", post(h::import_universe))
        .route("/secrets", get(h::secrets))
        .route("/secrets/verify", post(h::verify_secrets))
        .route("/secrets/{key}", put(h::set_secret))
        .route("/events", get(ws::events))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));
    Router::new()
        .route("/health", get(h::health))
        .merge(protected)
        .with_state(state)
}

/// Помилка обробника: код HTTP і тіло [`ApiErrorDto`].
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ApiErrorDto,
}

impl ApiError {
    pub fn new(status: StatusCode, error: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorDto {
                error: error.into(),
                details: Vec::new(),
            },
        }
    }

    /// 422 зі списком порушень.
    pub fn validation(error: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: ApiErrorDto {
                error: error.into(),
                details,
            },
        }
    }

    pub fn bad_request(error: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error)
    }

    pub fn unauthorized(error: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, error)
    }

    pub fn not_found(error: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, error)
    }

    pub fn conflict(error: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, error)
    }

    pub fn internal(error: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::InvalidTransition { from, to } => Self::conflict(format!(
                "Недопустима зміна статусу наміру: {} → {}",
                status_name(&from),
                status_name(&to)
            )),
            StoreError::NotFound(what) => Self::not_found(format!("Не знайдено: {what}")),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<CycleError> for ApiError {
    fn from(e: CycleError) -> Self {
        match e {
            CycleError::Store(s) => s.into(),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<SetupError> for ApiError {
    fn from(e: SetupError) -> Self {
        Self::internal(format!("Рушій не налаштовано: {e}"))
    }
}

impl From<SecretError> for ApiError {
    fn from(e: SecretError) -> Self {
        Self::internal(format!("Сховище секретів недоступне: {e}"))
    }
}

/// snake_case-назва значення з serde (`IntentStatus::PendingApproval` → `pending_approval`).
pub(crate) fn status_name<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "?".into(),
    }
}

/// `Json<T>`, що відхиляє некоректне тіло помилкою [`ApiErrorDto`] (той самий код HTTP).
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(r) => Err(json_rejection(r)),
        }
    }
}

fn json_rejection(r: JsonRejection) -> ApiError {
    ApiError {
        status: r.status(),
        body: ApiErrorDto {
            error: "Некоректне тіло запиту".into(),
            details: vec![r.body_text()],
        },
    }
}

/// `Query<T>`, що відхиляє некоректні параметри помилкою 422 [`ApiErrorDto`].
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(v)) => Ok(Self(v)),
            Err(r) => Err(query_rejection(r)),
        }
    }
}

fn query_rejection(r: QueryRejection) -> ApiError {
    ApiError::validation("Некоректні параметри запиту", vec![r.body_text()])
}

#[cfg(test)]
mod tests;
