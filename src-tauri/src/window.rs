use tauri::{Manager, PhysicalPosition};

/// The anchor glyph (icons/window.png, 256px) — same artwork as the tray.
/// Windows without an explicit icon inherit the exe's embedded icon.ico,
/// which used to be the Tauri default; both windows set this instead so
/// taskbar buttons show the anchor.
pub(crate) fn anchor_icon() -> tauri::image::Image<'static> {
    tauri::image::Image::from_bytes(include_bytes!("../icons/window.png"))
        .expect("icons/window.png must be a valid PNG")
}

/// Set the anchor on the main window (called from setup).
pub(crate) fn set_main_icon(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_icon(anchor_icon());
    }
}

pub(crate) fn toggle_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    match window.is_visible() {
        Ok(true) => {
            let _ = window.hide();
        }
        _ => {
            // Dock to the top edge of the monitor, horizontally centered.
            if let Ok(Some(monitor)) = window.current_monitor() {
                let win_size = window.outer_size().unwrap_or_else(|_| *monitor.size());
                let x = monitor.position().x + ((monitor.size().width as i64 - win_size.width as i64) / 2) as i32;
                let _ = window.set_position(PhysicalPosition::new(x, monitor.position().y));
            }
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

/// Open (or focus) the settings window. Created on first use.
///
/// The command wrapper MUST be async: sync commands execute on the main
/// thread, and building a WebviewWindow from there deadlocks the event
/// loop. The async form runs on the tokio runtime, which proxies window
/// creation correctly. The tray handler (already on the main thread)
/// calls the inner function directly.
pub(crate) fn open_settings_inner(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let built = tauri::WebviewWindow::builder(
        app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("First Mate — Settings")
    .inner_size(480.0, 600.0)
    .decorations(false)
    .shadow(true)
    .icon(anchor_icon())
    .and_then(|b| b.build());
    match built {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(e) => eprintln!("failed to open settings window: {e}"),
    }
}

/// Frontend entry (chat console gear). Async by necessity — see
/// `open_settings_inner`.
#[tauri::command]
pub(crate) async fn open_settings(app: tauri::AppHandle) {
    open_settings_inner(&app);
}
