//! Команди Tauri: тонка обгортка над локальним API рушія.

use chrono::NaiveDate;
use fond_api::config::EngineConfig;
use fond_api::dto::{
    AccountDto, AddSymbolRequest, AiProposalDto, AiRunQueuedDto, ExcludeRequest, HistoryJobDto,
    ImportResultDto, KillRequest, KillResultDto, PositionDto, RebalanceRunQueuedDto, ScanResultDto,
    SecretStatusDto, SetSecretRequest, SignalDto, StatusDto, UniverseEntryDto, VerifyResultDto,
};
use fond_api::events::JournalEventDto;
use fond_backtest::BacktestRequest;
use fond_core::intent::Intent;
use fond_core::orders::BrokerOrder;
use fond_core::types::Bar;
use reqwest::Method;
use serde::Deserialize;
use tauri::State;

use crate::AppState;
use crate::engine_client::{ClientError, EngineClient};

async fn client(state: &State<'_, AppState>) -> Result<EngineClient, ClientError> {
    state
        .client
        .read()
        .await
        .clone()
        .ok_or(ClientError::Unreachable {
            message: "рушій ще запускається".into(),
        })
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Стан, рахунок, позиції, ордери.
#[tauri::command]
pub async fn get_status(state: State<'_, AppState>) -> Result<StatusDto, ClientError> {
    client(&state).await?.get("/status").await
}

#[tauri::command]
pub async fn get_account(state: State<'_, AppState>) -> Result<AccountDto, ClientError> {
    client(&state).await?.get("/account").await
}

#[tauri::command]
pub async fn get_positions(state: State<'_, AppState>) -> Result<Vec<PositionDto>, ClientError> {
    client(&state).await?.get("/positions").await
}

#[tauri::command]
pub async fn get_orders(state: State<'_, AppState>) -> Result<Vec<BrokerOrder>, ClientError> {
    client(&state).await?.get("/orders").await
}

#[tauri::command]
pub async fn get_signals(
    state: State<'_, AppState>,
    date: NaiveDate,
) -> Result<Vec<SignalDto>, ClientError> {
    let path = format!("/signals?date={date}");
    client(&state).await?.get(&path).await
}

#[tauri::command]
pub async fn get_scan(
    state: State<'_, AppState>,
    date: NaiveDate,
) -> Result<Vec<ScanResultDto>, ClientError> {
    let path = format!("/scan?date={date}");
    client(&state).await?.get(&path).await
}

// ---------------------------------------------------------------- AI-аналітик

#[tauri::command]
pub async fn get_ai_proposals(
    state: State<'_, AppState>,
    days: Option<u32>,
) -> Result<Vec<AiProposalDto>, ClientError> {
    let path = match days {
        Some(days) => format!("/proposals/ai?days={days}"),
        None => "/proposals/ai".to_string(),
    };
    client(&state).await?.get(&path).await
}

#[tauri::command]
pub async fn run_ai_analysis(state: State<'_, AppState>) -> Result<AiRunQueuedDto, ClientError> {
    client(&state)
        .await?
        .send::<(), AiRunQueuedDto>(Method::POST, "/proposals/ai/run", None)
        .await
}

/// Ручний запуск ребалансу портфеля: 202 (сесія відправки) або помилка
/// конфлікту (портфель вимкнено / запуск уже йде).
#[tauri::command]
pub async fn run_portfolio_rebalance(
    state: State<'_, AppState>,
) -> Result<RebalanceRunQueuedDto, ClientError> {
    client(&state)
        .await?
        .send::<(), RebalanceRunQueuedDto>(Method::POST, "/portfolio/rebalance/run", None)
        .await
}

#[tauri::command]
pub async fn get_intents(
    state: State<'_, AppState>,
    statuses: Vec<String>,
) -> Result<Vec<Intent>, ClientError> {
    let path = if statuses.is_empty() {
        "/intents".to_string()
    } else {
        format!(
            "/intents?status={}",
            statuses
                .iter()
                .map(|s| encode(s))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    client(&state).await?.get(&path).await
}

#[tauri::command]
pub async fn approve_intent(state: State<'_, AppState>, id: String) -> Result<Intent, ClientError> {
    client(&state)
        .await?
        .send::<(), Intent>(
            Method::POST,
            &format!("/intents/{}/approve", encode(&id)),
            None,
        )
        .await
}

#[tauri::command]
pub async fn reject_intent(state: State<'_, AppState>, id: String) -> Result<Intent, ClientError> {
    client(&state)
        .await?
        .send::<(), Intent>(
            Method::POST,
            &format!("/intents/{}/reject", encode(&id)),
            None,
        )
        .await
}

#[tauri::command]
pub async fn close_position(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<Intent, ClientError> {
    client(&state)
        .await?
        .send::<(), Intent>(
            Method::POST,
            &format!("/positions/{}/close", encode(&symbol)),
            None,
        )
        .await
}

#[tauri::command]
pub async fn pause_engine(state: State<'_, AppState>) -> Result<StatusDto, ClientError> {
    client(&state)
        .await?
        .send::<(), StatusDto>(Method::POST, "/engine/pause", None)
        .await
}

#[tauri::command]
pub async fn resume_engine(state: State<'_, AppState>) -> Result<StatusDto, ClientError> {
    client(&state)
        .await?
        .send::<(), StatusDto>(Method::POST, "/engine/resume", None)
        .await
}

#[tauri::command]
pub async fn kill_engine(
    state: State<'_, AppState>,
    close_positions: bool,
) -> Result<KillResultDto, ClientError> {
    let body = KillRequest { close_positions };
    client(&state)
        .await?
        .send(Method::POST, "/engine/kill", Some(&body))
        .await
}

/// Конфігурація.
#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<EngineConfig, ClientError> {
    client(&state).await?.get("/config").await
}

#[tauri::command]
pub async fn put_config(
    state: State<'_, AppState>,
    config: EngineConfig,
) -> Result<EngineConfig, ClientError> {
    client(&state)
        .await?
        .send(Method::PUT, "/config", Some(&config))
        .await
}

/// Свічки.
#[tauri::command]
pub async fn get_bars(
    state: State<'_, AppState>,
    symbol: String,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Vec<Bar>, ClientError> {
    let mut path = format!("/bars/{}", encode(&symbol));
    let mut sep = '?';
    if let Some(from) = from {
        path.push(sep);
        path.push_str(&format!("from={from}"));
        sep = '&';
    }
    if let Some(to) = to {
        path.push(sep);
        path.push_str(&format!("to={to}"));
    }
    client(&state).await?.get(&path).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct JournalParams {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub symbol: Option<String>,
    pub kinds: Option<String>,
    pub limit: Option<u32>,
}

#[tauri::command]
pub async fn get_journal(
    state: State<'_, AppState>,
    query: JournalParams,
) -> Result<Vec<JournalEventDto>, ClientError> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(v) = query.from {
        pairs.push(("from", v.to_string()));
    }
    if let Some(v) = query.to {
        pairs.push(("to", v.to_string()));
    }
    if let Some(v) = query.symbol {
        pairs.push(("symbol", v));
    }
    if let Some(v) = query.kinds {
        pairs.push(("kinds", v));
    }
    if let Some(v) = query.limit {
        pairs.push(("limit", v.to_string()));
    }
    let mut path = "/journal".to_string();
    if !pairs.is_empty() {
        let qs = pairs
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        path.push('?');
        path.push_str(&qs);
    }
    client(&state).await?.get(&path).await
}

/// Бектести.
#[tauri::command]
pub async fn start_backtest(
    state: State<'_, AppState>,
    request: BacktestRequest,
) -> Result<fond_api::dto::BacktestRunDto, ClientError> {
    client(&state)
        .await?
        .send(Method::POST, "/backtests", Some(&request))
        .await
}

#[tauri::command]
pub async fn list_backtests(
    state: State<'_, AppState>,
) -> Result<Vec<fond_api::dto::BacktestRunDto>, ClientError> {
    client(&state).await?.get("/backtests").await
}

#[tauri::command]
pub async fn get_backtest(
    state: State<'_, AppState>,
    id: String,
) -> Result<fond_api::dto::BacktestDetailDto, ClientError> {
    client(&state)
        .await?
        .get(&format!("/backtests/{}", encode(&id)))
        .await
}

/// Завантаження історії.
#[tauri::command]
pub async fn start_history_download(
    state: State<'_, AppState>,
    from: NaiveDate,
) -> Result<HistoryJobDto, ClientError> {
    let body = fond_api::dto::HistoryRequest { from };
    client(&state)
        .await?
        .send(Method::POST, "/history/download", Some(&body))
        .await
}

#[tauri::command]
pub async fn get_history_job(state: State<'_, AppState>) -> Result<HistoryJobDto, ClientError> {
    client(&state).await?.get("/history/job").await
}

/// Список інструментів.
#[tauri::command]
pub async fn get_universe(
    state: State<'_, AppState>,
) -> Result<Vec<UniverseEntryDto>, ClientError> {
    client(&state).await?.get("/universe").await
}

#[tauri::command]
pub async fn add_symbol(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<UniverseEntryDto, ClientError> {
    let body = AddSymbolRequest { symbol };
    client(&state)
        .await?
        .send(Method::POST, "/universe", Some(&body))
        .await
}

#[tauri::command]
pub async fn set_excluded(
    state: State<'_, AppState>,
    symbol: String,
    excluded: bool,
) -> Result<UniverseEntryDto, ClientError> {
    let body = ExcludeRequest { excluded };
    client(&state)
        .await?
        .send(
            Method::PUT,
            &format!("/universe/{}", encode(&symbol)),
            Some(&body),
        )
        .await
}

#[tauri::command]
pub async fn remove_symbol(state: State<'_, AppState>, symbol: String) -> Result<(), ClientError> {
    client(&state)
        .await?
        .send::<(), ()>(
            Method::DELETE,
            &format!("/universe/{}", encode(&symbol)),
            None,
        )
        .await
}

#[tauri::command]
pub async fn import_universe(
    state: State<'_, AppState>,
    csv: String,
) -> Result<ImportResultDto, ClientError> {
    client(&state)
        .await?
        .send_text("/universe/import", &csv)
        .await
}

/// Секрети.
#[tauri::command]
pub async fn get_secrets(state: State<'_, AppState>) -> Result<Vec<SecretStatusDto>, ClientError> {
    client(&state).await?.get("/secrets").await
}

#[tauri::command]
pub async fn set_secret(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), ClientError> {
    let body = SetSecretRequest { value };
    client(&state)
        .await?
        .send::<_, ()>(
            Method::PUT,
            &format!("/secrets/{}", encode(&key)),
            Some(&body),
        )
        .await
}

#[tauri::command]
pub async fn verify_secrets(state: State<'_, AppState>) -> Result<VerifyResultDto, ClientError> {
    client(&state)
        .await?
        .send::<(), VerifyResultDto>(Method::POST, "/secrets/verify", None)
        .await
}

/// Автозапуск.
#[tauri::command]
pub async fn get_autostart() -> Result<bool, ClientError> {
    crate::autostart::get_autostart().map_err(|e| ClientError::Unreachable {
        message: e.to_string(),
    })
}

#[tauri::command]
pub async fn set_autostart(enabled: bool) -> Result<(), ClientError> {
    crate::autostart::set_autostart(enabled).map_err(|e| ClientError::Unreachable {
        message: e.to_string(),
    })
}
