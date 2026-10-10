use tauri::Manager;
use crate::provider_catalog;

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
    /// The `Hide sensitive information` choice per provider id. A provider with
    /// no entry takes the roster default.
    hide_sensitive: std::collections::HashMap<String, bool>,
}

impl Settings {
    /// Whether the redaction pass runs for one provider. The stored choice wins;
    /// otherwise the roster row decides; an id outside the roster redacts.
    pub(crate) fn hide_sensitive_for(&self, provider: &str) -> bool {
        if let Some(on) = self.hide_sensitive.get(provider) {
            return *on;
        }
        match provider_catalog::lookup(provider) {
            Some(spec) => spec.hide_sensitive_default,
            None => true,
        }
    }
}

fn settings_file(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    let dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

/// Read the stored settings. A missing or unreadable file yields defaults,
/// and `Settings::hide_sensitive_for` redacts an unknown provider, so a broken
/// settings file fails toward redaction.
pub(crate) fn load(app: &tauri::AppHandle) -> tauri::Result<Settings> {
    let path = settings_file(app)?;
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents)?)
    } else {
        Ok(Settings::default())
    }
}

#[tauri::command]
pub(crate) fn get_settings(app: tauri::AppHandle) -> tauri::Result<Settings> {
    load(&app)
}

#[tauri::command]
pub(crate) fn save_settings(app: tauri::AppHandle, settings: Settings) -> tauri::Result<()> {
    let path = settings_file(&app)?;
    let contents = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Layer 2 of `docs/roadmap/REDACTION.md`: the values no shape identifies, such
/// as a US account number. The list lives in its own file, because a client that
/// posts a whole `Settings` object would otherwise erase it.
fn registry_file(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    Ok(app.path().app_config_dir()?.join("entity_registry.json"))
}

/// A missing or unreadable registry leaves layer 2 empty, so a broken file
/// costs one detection class and never a chat turn.
fn parse_registry(contents: &str) -> Vec<String> {
    serde_json::from_str(contents).unwrap_or_default()
}

pub(crate) fn registry(app: &tauri::AppHandle) -> Vec<String> {
    let Ok(path) = registry_file(app) else {
        return Vec::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_registry(&contents),
        Err(_) => Vec::new(),
    }
}

/// The redaction trigger for one provider, read where the request is built.
/// No caller may infer the trigger from the endpoint address.
pub(crate) fn redaction_for(app: &tauri::AppHandle, provider: &str) -> (bool, Vec<String>) {
    (
        load(app).unwrap_or_default().hide_sensitive_for(provider),
        registry(app),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The roster rows that start unredacted, because an engine on this
    /// machine has no receiver. Every other row starts redacted.
    const ON_MACHINE: &[&str] = &["ollama", "lm-studio", "llama-cpp"];

    #[test]
    fn a_broken_registry_file_costs_one_class_not_a_turn() {
        assert_eq!(
            parse_registry(r#"["90821447"]"#),
            vec!["90821447".to_string()]
        );
        assert!(parse_registry("not json").is_empty());
        assert!(parse_registry("{}").is_empty());
    }

    #[test]
    fn only_the_on_machine_engines_start_unredacted() {
        for id in ON_MACHINE {
            assert!(
                !Settings::default().hide_sensitive_for(id),
                "{id} must start unredacted"
            );
        }
        for spec in provider_catalog::catalog() {
            if ON_MACHINE.contains(&spec.id.as_str()) {
                continue;
            }
            assert!(
                Settings::default().hide_sensitive_for(&spec.id),
                "{} must start redacted",
                spec.id
            );
        }
    }

    #[test]
    fn stored_choice_beats_the_roster_default() {
        let mut settings = Settings::default();
        settings.hide_sensitive.insert("ollama".to_string(), true);
        settings.hide_sensitive.insert("openai".to_string(), false);
        assert!(settings.hide_sensitive_for("ollama"));
        assert!(!settings.hide_sensitive_for("openai"));
    }

    #[test]
    fn a_provider_outside_the_roster_starts_redacted() {
        assert!(Settings::default().hide_sensitive_for("no-such-provider"));
    }
}
