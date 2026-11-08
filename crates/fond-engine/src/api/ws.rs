//! `WS /events`: потік `EngineEventDto` у JSON (по одному текстовому повідомленню на подію).

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use fond_api::events::EngineEventDto;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use super::ApiState;

/// Підписка на шину відбувається до відповіді 101: подія, опублікована одразу після
/// рукостискання, не губиться.
pub async fn events(State(st): State<ApiState>, ws: WebSocketUpgrade) -> Response {
    let rx = st.engine.bus().subscribe();
    let shutdown = st.shutdown.clone();
    ws.on_upgrade(move |socket| forward(socket, rx, shutdown))
}

/// Завершується, коли рушій зупиняється; без відправника — ніколи.
async fn stopping(shutdown: &mut watch::Receiver<bool>) {
    if shutdown.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn send(socket: &mut WebSocket, e: &EngineEventDto) -> bool {
    match serde_json::to_string(e) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(err) => {
            tracing::error!(error = %err, "не вдалося серіалізувати подію");
            true
        }
    }
}

async fn forward(
    mut socket: WebSocket,
    mut rx: Receiver<EngineEventDto>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(e) => {
                    if !send(&mut socket, &e).await {
                        break;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    // Клієнт не встигав: частину подій втрачено, нехай перечитає стан.
                    tracing::warn!(skipped, "WebSocket-клієнт пропустив події");
                    if !send(&mut socket, &EngineEventDto::StatusChanged).await {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
            () = stopping(&mut shutdown) => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
        }
    }
}
