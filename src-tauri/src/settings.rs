use tauri::Manager;

/// `run` registers `HOTKEY` once, so this constant is the only place to
/// change the binding.
pub(crate) const HOTKEY: &str = "Super+Alt+Space";

/// The UI shows the real binding instead of a copy.
#[tauri::command]
pub(crate) fn get_hotkey() -> &'static str {
    HOTKEY
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
struct LlmSettings {
    provider: String,
    model: String,
    api_key: String,
    base_url: String,
    /// Context window in tokens. A value of 0 means: ask the endpoint,
    /// which reports `max_model_len` (llama.cpp) or nothing. An endpoint
    /// that reports nothing falls back to `context::DEFAULT_WINDOW_TOKENS`.
    context_window: u32,
    /// Compaction starts when the request reaches this percentage of the
    /// context window.
    compact_at_percent: u8,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            context_window: 0,
            compact_at_percent: 85,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
#[serde(default)]
pub(crate) struct Settings {
    llm: LlmSettings,
}

fn settings_file(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    let dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

#[tauri::command]
pub(crate) fn get_settings(app: tauri::AppHandle) -> tauri::Result<Settings> {
    let path = settings_file(&app)?;
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents)?)
    } else {
        Ok(Settings::default())
    }
}

#[tauri::command]
pub(crate) fn save_settings(app: tauri::AppHandle, settings: Settings) -> tauri::Result<()> {
    let path = settings_file(&app)?;
    let contents = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&path, contents)?;
    Ok(())
}
