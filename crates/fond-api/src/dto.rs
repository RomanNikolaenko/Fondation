//! DTO локального API рушія (`127.0.0.1`, `Authorization: Bearer`).
//!
//! Суми й ціни передаються рядками (`Decimal` або округлені до центів), дати — `YYYY-MM-DD`,
//! моменти часу — RFC 3339 в UTC.

use chrono::{DateTime, NaiveDate, Utc};
use fond_backtest::{BacktestRequest, BacktestResult};
use fond_core::signal::EntrySignal;
use fond_core::types::{AccountMode, ExecutionMode};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use fond_core::keys::{KEYRING_SERVICE, SecretKey};

/// `GET /health` (без токена).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HealthDto {
    pub ok: bool,
    pub version: String,
}

/// Режим рушія: `state` — `"running"`, `"paused"` або `"killed"` (`"unknown"`, якщо режим не
/// вдалося прочитати зі сховища; причина — у `reason`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EngineModeDto {
    pub state: String,
    pub reason: Option<String>,
}

/// Наступний крок розкладу: `name` — `"analysis"`, `"approval_deadline"` або `"pre_open_submit"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NextStepDto {
    pub name: String,
    pub at: DateTime<Utc>,
}

/// `GET /status`. `needs_setup` — ключі (snake_case), без яких рушій не працює.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StatusDto {
    pub engine_mode: EngineModeDto,
    pub account_mode: AccountMode,
    pub execution_mode: ExecutionMode,
    pub needs_setup: Vec<String>,
    pub next_step: Option<NextStepDto>,
    pub last_reconcile_at: Option<DateTime<Utc>>,
    pub version: String,
    pub now: DateTime<Utc>,
}

/// `GET /account`. `week_pnl` — `None`, якщо немає Close-точки до понеділка поточного тижня.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AccountDto {
    pub equity: String,
    pub cash: String,
    pub buying_power: String,
    pub day_pnl: String,
    pub week_pnl: Option<String>,
}

/// Відстежувана позиція; `current_price` і `unrealized_pnl` — з позицій брокера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PositionDto {
    pub symbol: String,
    pub strategy: String,
    pub qty: u32,
    pub entry_price: String,
    pub entry_date: NaiveDate,
    pub current_price: String,
    pub stop_price: String,
    pub take_profit: Option<String>,
    pub unrealized_pnl: String,
    pub sector: String,
}

/// Сигнал дня: `status` — `"accepted"` або `"rejected"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SignalDto {
    pub signal: EntrySignal,
    pub status: String,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ScanResultDto {
    pub symbol: String,
    pub passed: bool,
    pub reason: Option<String>,
}

/// `source` — `"sp500"`, `"etf"` або `"manual"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct UniverseEntryDto {
    pub symbol: String,
    pub name: String,
    pub sector: String,
    pub source: String,
    pub excluded: bool,
}

/// `status` — `"queued"`, `"running"`, `"done"` або `"failed"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct BacktestRunDto {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub request: BacktestRequest,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct BacktestDetailDto {
    pub run: BacktestRunDto,
    pub result: Option<BacktestResult>,
}

/// Завантаження історії: `status` — `"idle"`, `"running"`, `"done"` або `"failed"`;
/// `progress` — частка в `[0, 1]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryJobDto {
    pub status: String,
    pub progress: f32,
    pub message: String,
}

/// Чи збережено ключ (`key` — snake_case). Значення ключа API ніколи не повертає.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SecretStatusDto {
    pub key: String,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CheckDto {
    pub ok: bool,
    pub message: Option<String>,
}

/// `POST /secrets/verify`: перевірка кожного сервісу збереженим ключем.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct VerifyResultDto {
    pub alpaca: CheckDto,
    pub finnhub: CheckDto,
    pub anthropic: CheckDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KillRequest {
    pub close_positions: bool,
}

/// Тіло `PUT /secrets/{key}`. `Debug` не показує значення.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SetSecretRequest {
    pub value: String,
}

impl std::fmt::Debug for SetSecretRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetSecretRequest")
            .field("value", &"****")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AddSymbolRequest {
    pub symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ExcludeRequest {
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HistoryRequest {
    pub from: NaiveDate,
}

/// Тіло кожної помилки API; `details` — окремі порушення (валідація конфігурації тощо).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiErrorDto {
    pub error: String,
    pub details: Vec<String>,
}

#[cfg(test)]
mod tests;
