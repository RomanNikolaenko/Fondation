//! Tauri-оболонка Fond: життєвий цикл, трей, автозапуск і команди до рушія.

mod autostart;
mod commands;
mod engine_client;
mod events;
mod sidecar;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Emitter, Manager};
use tokio::sync::RwLock;

use engine_client::EngineClient;

/// Стан застосунку: клієнт рушія з'являється після успішного `ensure_engine`.
pub struct AppState {
    pub client: RwLock<Option<EngineClient>>,
    pub notifications: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            client: RwLock::new(None),
            notifications: AtomicBool::new(true),
        }
    }
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_account,
            commands::get_positions,
            commands::get_orders,
            commands::get_signals,
            commands::get_scan,
            commands::get_ai_proposals,
            commands::run_ai_analysis,
            commands::get_intents,
            commands::approve_intent,
            commands::reject_intent,
            commands::close_position,
            commands::pause_engine,
            commands::resume_engine,
            commands::kill_engine,
            commands::get_config,
            commands::put_config,
            commands::get_bars,
            commands::get_journal,
            commands::start_backtest,
            commands::list_backtests,
            commands::get_backtest,
            commands::start_history_download,
            commands::get_history_job,
            commands::get_universe,
            commands::add_symbol,
            commands::set_excluded,
            commands::remove_symbol,
            commands::import_universe,
            commands::get_secrets,
            commands::set_secret,
            commands::verify_secrets,
            commands::get_autostart,
            commands::set_autostart,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tray::build_tray(&handle)?;

            if std::env::args().any(|arg| arg == "--minimized")
                && let Some(window) = handle.get_webview_window("main")
            {
                let _ = window.hide();
            }

            tauri::async_runtime::spawn(async move {
                match sidecar::ensure_engine(&sidecar::data_dir()).await {
                    Ok(client) => {
                        let notifications = client
                            .get::<fond_api::config::EngineConfig>("/config")
                            .await
                            .ok()
                            .map(|cfg| cfg.ui.notifications_enabled)
                            .unwrap_or(true);
                        {
                            let state = handle.state::<AppState>();
                            state.notifications.store(notifications, Ordering::Relaxed);
                            *state.client.write().await = Some(client);
                        }
                        tray::refresh_tooltip(&handle).await;
                    }
                    Err(message) => {
                        eprintln!("{message}");
                        let _ = handle.emit("engine-unavailable", message);
                    }
                }
                events::forward_events(handle).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
