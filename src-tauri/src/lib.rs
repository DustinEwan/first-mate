use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

mod conversations;
mod exec;
mod execute;
mod fileedit;
mod harness;
mod llm;
mod log;
mod modelapi;
mod openpath;
mod provider_catalog;
mod psrep;
mod report;
mod route;
mod settings;
mod skills;
mod tooldefs;
mod window;

use crate::conversations::{delete_conversation, list_conversations, load_conversation, save_conversation};
use crate::harness::{bootstrap_status, cli_version, install_winapp};
use crate::llm::{chat_with_llm, stop_chat};
use crate::log::{fm_out_dir, fm_tmp_dir, log, trim_dir};
use crate::modelapi::{list_models, list_providers, test_llm};
use crate::openpath::open_path;
use crate::settings::{HOTKEY, get_hotkey, get_settings, save_settings};
use crate::skills::{get_system_prompt, list_skills};
use crate::window::{open_settings, open_settings_inner, set_main_icon, toggle_window};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, list_models, list_providers, test_llm, chat_with_llm, stop_chat, list_skills, list_conversations, load_conversation, delete_conversation, save_conversation, get_system_prompt, open_path, bootstrap_status, install_winapp, open_settings])
        .setup(|app| {
            // Taskbar buttons show the anchor, not the exe's embedded icon.
            set_main_icon(app.handle());
            // Scratch hygiene (P6): drop old generations of generated
            // scripts and spilled output from previous sessions.
            trim_dir(&fm_tmp_dir(), 50);
            trim_dir(&fm_out_dir(), 50);
            // Harness probe: the winapp skill drives the desktop through the
            // winapp CLI (microsoft/winappCli). We DETECT only - installing
            // software is the user's decision, made in the Setup wizard,
            // which offers the same unattended winget command on demand.
            tauri::async_runtime::spawn(async {
                match cli_version("winapp").await {
                    Some(v) => log(&format!("BOOTSTRAP: winapp CLI present ({v})")),
                    None => log("BOOTSTRAP: winapp CLI missing; offer install in Setup wizard"),
                }
            });
            // Tray: left-click toggles the console; menu for explicit actions.
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Show / Hide", true, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let setup_item =
                MenuItem::with_id(app, "setup", "Setup Wizard", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle_item, &setup_item, &settings_item, &quit_item])?;

            // The anchor glyph rendered from Segoe UI Emoji (icons/tray.png),
            // not the default Tauri icon.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            TrayIconBuilder::new()
                .icon(tray_icon)
                .tooltip("First Mate")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => toggle_window(app),
                    "setup" => {
                        toggle_window(app);
                        let _ = app.emit("open_setup", ());
                    }
                    "settings" => open_settings_inner(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event({
                    // A single tray click can emit more than one event; debounce so a
                    // double event doesn't show-then-hide (flash) the window.
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

            // Global hotkey toggles the console from anywhere.
            app.global_shortcut()
                .on_shortcut(HOTKEY, |app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })?;

            // Closing the window hides it; the app lives in the tray.
            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running First Mate");
}
