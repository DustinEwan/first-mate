//! Self-update via tauri-plugin-updater, served from GitHub Releases.
//!
//! On launch (after a short delay, so the tray exists) we check
//! `latest.json`. When an update is found we notify via a toast and enable
//! the tray menu's "Update" entry (renamed to "Update to X"); clicking it
//! downloads, installs and restarts. The item is created up front but
//! disabled — Windows menus mutate unreliably while open, so we only flip
//! `enabled` and the title, never the item set.

use std::time::Duration;

use tauri::{menu::MenuItem, AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

/// Delay before the first update check — keeps launch snappy and lets the
/// tray menu be built before we touch it.
const CHECK_DELAY: Duration = Duration::from_secs(5);

/// The tray "update" item, managed as state so the background check can
/// enable/rename it when an update is found.
pub(crate) struct UpdateItem(pub MenuItem<tauri::Wry>);

pub(crate) fn spawn_check(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CHECK_DELAY).await;
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                crate::log::log(&format!("UPDATE: updater unavailable: {e}"));
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                crate::log::log(&format!("UPDATE: {} available", update.version));
                notify(&app, &format!("Version {} is available — right-click the tray icon and pick \"Update to {}\"", update.version, update.version));
                if let Some(item) = app.try_state::<UpdateItem>() {
                    let _ = item.0.set_text(format!("Update to {}", update.version));
                    let _ = item.0.set_enabled(true);
                }
            }
            Ok(None) => crate::log::log("UPDATE: up to date"),
            Err(e) => crate::log::log(&format!("UPDATE: check failed: {e}")),
        }
    });
}

/// Tray menu "update" click: spawn the install so the handler returns.
pub(crate) fn handle_menu_click(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = install(handle.clone()).await {
            crate::log::log(&format!("UPDATE: install failed: {e}"));
            notify(&handle, &format!("First Mate update failed: {e}"));
        }
    });
}

async fn install(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    let version = update.version.clone();
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    crate::log::log(&format!("UPDATE: installed {version}; restarting"));
    app.restart();
}

fn notify(app: &AppHandle, body: &str) {
    let _ = app.notification().builder().title("First Mate").body(body).show();
}
