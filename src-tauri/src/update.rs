//! Self-update via tauri-plugin-updater, served from GitHub Releases.
//!
//! The first check runs on a delay, so the tray menu exists before the
//! check updates the menu.
//!
//! Windows menus mutate unreliably while open, so the code changes only
//! `enabled` and the title of the tray item. The item set never changes.

use std::time::Duration;

use tauri::{menu::MenuItem, AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

/// The first check waits. The tray menu must exist before the check updates
/// the menu, and startup must stay fast.
const CHECK_DELAY: Duration = Duration::from_secs(5);

/// The tray "update" menu item, held as app state. The background check
/// needs the item to enable and rename the entry.
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

/// Tray menu "update" click. The install runs in a spawned task so the menu
/// handler returns at once.
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
