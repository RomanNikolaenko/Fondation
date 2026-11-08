//! Пересилання подій рушія у вікно та системні сповіщення.

use std::sync::atomic::Ordering;
use std::time::Duration;

use fond_api::config::EngineConfig;
use fond_api::events::EngineEventDto;
use futures::StreamExt;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::AppState;

/// Український текст сповіщення для події, якщо вона варта уваги.
pub fn notification_for(event: &EngineEventDto) -> Option<(String, String)> {
    match event {
        EngineEventDto::IntentsPending { count, .. } => Some((
            "Нові угоди на схвалення".into(),
            format!("{count} намір(ів) чекає на підтвердження"),
        )),
        EngineEventDto::OrderFilled {
            symbol,
            side,
            qty,
            price,
        } => Some((
            "Ордер виконано".into(),
            format!("{symbol}: {side} {qty} @ {price}"),
        )),
        EngineEventDto::StopHit { symbol, price } => {
            Some(("Спрацював стоп".into(), format!("{symbol} @ {price}")))
        }
        EngineEventDto::AutoPaused { reason } => {
            Some(("Рушій на паузі".into(), reason.clone()))
        }
        EngineEventDto::Killed { reason } => {
            Some(("Аварійна зупинка".into(), reason.clone()))
        }
        EngineEventDto::StepFailed { step, error } => Some((
            "Крок не виконано".into(),
            format!("{step}: {error}"),
        )),
        EngineEventDto::BacktestStatus { id, status } => Some((
            "Бектест".into(),
            format!("{id}: {status}"),
        )),
        EngineEventDto::Journal { .. }
        | EngineEventDto::StatusChanged
        | EngineEventDto::HistoryProgress { .. } => None,
    }
}

/// Нескінченний цикл: підключається до `/events`, пересилає події, перепідключається
/// з експоненційною затримкою 1, 2, 4 … 30 с.
pub async fn forward_events(app: AppHandle) {
    let mut backoff = 1u64;
    loop {
        let client = app.state::<AppState>().client.read().await.clone();
        let Some(client) = client else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };

        match connect_async(client.ws_request()).await {
            Ok((mut ws, _)) => {
                backoff = 1;
                while let Some(message) = ws.next().await {
                    let Ok(Message::Text(text)) = message else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<EngineEventDto>(text.as_str()) else {
                        continue;
                    };
                    let _ = app.emit("engine-event", &event);

                    if let EngineEventDto::Journal { event } = &event
                        && event.kind == "config"
                        && let Ok(cfg) = client.get::<EngineConfig>("/config").await
                    {
                        app.state::<AppState>()
                            .notifications
                            .store(cfg.ui.notifications_enabled, Ordering::Relaxed);
                    }

                    if app.state::<AppState>().notifications.load(Ordering::Relaxed)
                        && let Some((title, body)) = notification_for(&event)
                    {
                        let _ = app.notification().builder().title(title).body(body).show();
                    }

                    if matches!(
                        event,
                        EngineEventDto::StatusChanged
                            | EngineEventDto::AutoPaused { .. }
                            | EngineEventDto::Killed { .. }
                    ) {
                        crate::tray::refresh_tooltip(&app).await;
                    }
                }
            }
            Err(e) => {
                eprintln!("WebSocket рушія недоступний: {e}");
            }
        }

        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::NaiveDate;

    #[test]
    fn notification_for_maps_events() {
        let pending = EngineEventDto::IntentsPending {
            session: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            count: 2,
        };
        assert_eq!(notification_for(&pending).unwrap().0, "Нові угоди на схвалення");

        let stop = EngineEventDto::StopHit {
            symbol: "AAPL".into(),
            price: "95.00".into(),
        };
        assert!(notification_for(&stop).is_some());

        let paused = EngineEventDto::AutoPaused { reason: "x".into() };
        assert!(notification_for(&paused).is_some());

        assert!(notification_for(&EngineEventDto::StatusChanged).is_none());
    }
}
