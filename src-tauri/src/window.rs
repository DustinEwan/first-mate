use tauri::{Manager, PhysicalPosition};

/// The anchor glyph (icons/window.png, 256px), the same artwork as the tray
/// icon. The bundle icons are generated from that source, so windows inherit
/// it by default. The settings window sets the icon explicitly as well.
pub(crate) fn anchor_icon() -> tauri::image::Image<'static> {
    tauri::image::Image::from_bytes(include_bytes!("../icons/window.png"))
        .expect("icons/window.png must be a valid PNG")
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

/// Open or focus the settings window, creating the window on first use.
///
/// The command wrapper MUST be async. A sync command runs on the main thread.
/// Building a WebviewWindow there deadlocks the event loop. The async form
/// runs on the tokio runtime, which proxies window creation correctly. The
/// tray handler already runs on the main thread, so the tray handler calls
/// this function directly.
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

/// Frontend entry (the chat console gear). Async by necessity: see
/// `open_settings_inner`.
#[tauri::command]
pub(crate) async fn open_settings(app: tauri::AppHandle) {
    open_settings_inner(&app);
}
