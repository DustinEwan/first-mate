use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

mod audit;
mod conversations;
mod context;
mod crypto;
mod decide;
mod exec;
mod elevate;
mod execute;
mod fileedit;
mod harness;
mod llm;
mod log;
mod modelapi;
mod openpath;
mod provider_catalog;
mod psrep;
mod redact;
mod report;
mod route;
mod settings;
mod skills;
mod tooldefs;
mod update;
mod window;

use crate::conversations::{delete_conversation, list_conversations, load_conversation, save_conversation};
use crate::elevate::{self as elevate_mod, ElevationState};
use crate::harness::{bootstrap_status, cli_version, install_winapp};
use crate::llm::{chat_with_llm, stop_chat};
use crate::log::{fm_out_dir, fm_tmp_dir, log, trim_dir};
use crate::modelapi::{list_models, list_providers, test_llm};
use crate::openpath::open_path;
use crate::settings::{
    HOTKEY, decision_host, get_hotkey, get_settings, probe_decision_server, save_settings,
};
use crate::skills::{get_system_prompt, list_skills};
use crate::window::{hide_window, open_settings, open_settings_inner, toggle_window};

#[tauri::command]
fn elevation_status(app: tauri::AppHandle) -> elevate_mod::ElevStatus {
    elevate_mod::status_snapshot(&app)
}

#[tauri::command]
fn elevate(app: tauri::AppHandle) -> Result<String, String> {
    elevate_mod::elevate_core(&app)
}

#[tauri::command]
fn recovery_key_status() -> bool {
    crypto::recovery_key_pending()
}

#[tauri::command]
fn get_recovery_key() -> Option<String> {
    crypto::recovery_key()
}

#[tauri::command]
fn ack_recovery_key() -> bool {
    crypto::ack_recovery_key()
}

#[tauri::command]
fn delevate(app: tauri::AppHandle) -> Result<String, String> {
    Ok(elevate_mod::delevate_core(&app, false))
}

#[tauri::command]
fn answer_audit(approved: bool, redactions: Vec<String>) -> bool {
    audit::resolve(approved, redactions)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, probe_decision_server, decision_host, answer_audit, list_models, list_providers, test_llm, chat_with_llm, stop_chat, list_skills, list_conversations, load_conversation, delete_conversation, save_conversation, get_system_prompt, open_path, bootstrap_status, install_winapp, open_settings, elevate, delevate, elevation_status, recovery_key_status, get_recovery_key, ack_recovery_key])
        .setup(|app| {
            app.manage(ElevationState::default());
            let config_dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
            crypto::init(config_dir.clone());
            redact::init(config_dir.join(crypto::TOKEN_MAP));
            let (sealed, removed) = crypto::upgrade_stores(&config_dir);
            if sealed > 0 || removed > 0 {
                log(&format!("ENVELOPE: sealed {sealed} store files, removed {removed} plaintext request dumps"));
            }
            // Generated scripts and spilled output from earlier sessions
            // would otherwise accumulate in both scratch directories.
            trim_dir(&fm_tmp_dir(), 50);
            trim_dir(&fm_out_dir(), 50);
            // The winapp skill drives the desktop through the winapp CLI
            // (microsoft/winappCli).
            // Installing software is the user's decision, taken in the Setup
            // wizard, which offers the unattended winget command on demand.
            tauri::async_runtime::spawn(async {
                match cli_version("winapp").await {
                    Some(v) => log(&format!("BOOTSTRAP: winapp CLI present ({v})")),
                    None => log("BOOTSTRAP: winapp CLI missing; offer install in Setup wizard"),
                }
            });
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Show / Hide", true, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let setup_item =
                MenuItem::with_id(app, "setup", "Setup Wizard", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            // When the background check finds an update, update.rs enables and
            // renames the item.
            let update_item = MenuItem::with_id(app, "update", "Update…", false, None::<&str>)?;
            app.manage(update::UpdateItem(update_item.clone()));
            let menu = Menu::with_items(app, &[&toggle_item, &setup_item, &settings_item, &update_item, &quit_item])?;

            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            TrayIconBuilder::new()
                .icon(tray_icon)
                .tooltip("First Mate")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "update" {
                        update::handle_menu_click(app);
                        return;
                    }
                    match event.id.as_ref() {
                        "toggle" => toggle_window(app),
                        "setup" => {
                            toggle_window(app);
                            let _ = app.emit("open_setup", ());
                        }
                        "settings" => open_settings_inner(app),
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .on_tray_icon_event({
                    // One tray click can emit several events. The debounce
                    // stops a second event from hiding the window again.
                    let last_toggle = Arc::new(AtomicU64::new(0));
                    move |tray, event| {
                        if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            let last = last_toggle.load(Ordering::Relaxed);
                            if now.saturating_sub(last) < 300 {
                                return;
                            }
                            last_toggle.store(now, Ordering::Relaxed);
                            toggle_window(tray.app_handle());
                        }
                    }
                })
                .build(app)?;

            update::spawn_check(app.handle());

            app.global_shortcut()
                .on_shortcut(HOTKEY, |app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })?;

            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        hide_window(win.app_handle());
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running First Mate");
}
