//! Значок у системному треї та підказка зі станом рушія.

use chrono_tz::America::New_York;
use fond_api::dto::StatusDto;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent, TrayIconId};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::AppState;

const TRAY_ID: &str = "main";

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Підказка трею: режим рушія і наступний крок у часі Нью-Йорка.
pub fn tray_tooltip(status: &StatusDto) -> String {
    match status.engine_mode.state.as_str() {
        "running" => match &status.next_step {
            Some(step) => format!(
                "Fond — працює · наступний крок {} ET",
                step.at
                    .with_timezone(&New_York)
                    .format("%H:%M")
            ),
            None => "Fond — працює".into(),
        },
        "paused" => format!(
            "Fond — пауза: {}",
            status.engine_mode.reason.as_deref().unwrap_or("причина невідома")
        ),
        "killed" => "Fond — аварійна зупинка".into(),
        other => format!("Fond — невідомий стан ({other})"),
    }
}

/// Оновити підказку з поточного `/status` (тихо ігнорує недоступний рушій).
pub async fn refresh_tooltip(app: &AppHandle) {
    let client = app.state::<AppState>().client.read().await.clone();
    let Some(client) = client else {
        return;
    };
    let Ok(status) = client.get::<StatusDto>("/status").await else {
        return;
    };
    if let Some(tray) = app.tray_by_id(&TrayIconId::new(TRAY_ID)) {
        let _ = tray.set_tooltip(Some(tray_tooltip(&status)));
    }
}

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Показати", true, None::<&str>)?;
    let kill = MenuItem::with_id(app, "kill", "Аварійна зупинка…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Вийти із застосунку", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &kill, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TrayIconId::new(TRAY_ID))
        .menu(&menu)
        .tooltip("Fond");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "kill" => {
                show_main(app);
                let _ = app.emit("open-kill-dialog", ());
            }
            "quit" => {
                let _ = app
                    .notification()
                    .builder()
                    .title("Fond")
                    .body("Рушій продовжує роботу у фоні")
                    .show();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(value: serde_json::Value) -> StatusDto {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn tray_tooltip_texts() {
        let running = status(serde_json::json!({
            "engine_mode": {"state": "running", "reason": null},
            "account_mode": "paper",
            "execution_mode": "auto",
            "needs_setup": [],
            "next_step": {"name": "analysis", "at": "2026-11-05T21:30:00Z"},
            "last_reconcile_at": null,
            "version": "0.1.0",
            "now": "2026-11-05T14:00:00Z"
        }));
        assert_eq!(
            tray_tooltip(&running),
            "Fond — працює · наступний крок 16:30 ET"
        );

        let paused = status(serde_json::json!({
            "engine_mode": {"state": "paused", "reason": "тижневий ліміт"},
            "account_mode": "paper",
            "execution_mode": "auto",
            "needs_setup": [],
            "next_step": null,
            "last_reconcile_at": null,
            "version": "0.1.0",
            "now": "2026-11-05T14:00:00Z"
        }));
        assert_eq!(tray_tooltip(&paused), "Fond — пауза: тижневий ліміт");

        let killed = status(serde_json::json!({
            "engine_mode": {"state": "killed", "reason": null},
            "account_mode": "paper",
            "execution_mode": "auto",
            "needs_setup": [],
            "next_step": null,
            "last_reconcile_at": null,
            "version": "0.1.0",
            "now": "2026-11-05T14:00:00Z"
        }));
        assert_eq!(tray_tooltip(&killed), "Fond — аварійна зупинка");
    }
}
