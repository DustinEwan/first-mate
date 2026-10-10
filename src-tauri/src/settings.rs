use tauri::Manager;
use crate::crypto;
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

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub(crate) struct Settings {
    llm: LlmSettings,
    /// The `Hide sensitive information` choice per provider id. A provider with
    /// no entry takes the roster default.
    hide_sensitive: std::collections::HashMap<String, bool>,
    /// The address of the decision server the user runs. First Mate neither
    /// starts nor downloads it, so an empty address is the off position.
    decision_server_url: String,
    decision_server_enabled: bool,
    /// The probability that replaces a span with one token. The setup probe
    /// reports the measured points, and the user picks one.
    decision_threshold: f64,
    /// A host off this machine holds every span the gate sends it. The gate
    /// sends nothing there until you tick the box in Settings.
    decision_server_remote_ok: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llm: LlmSettings::default(),
            hide_sensitive: std::collections::HashMap::new(),
            decision_server_url: String::new(),
            decision_server_enabled: false,
            decision_threshold: 0.5,
            decision_server_remote_ok: false,
        }
    }
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

    /// The decision server for this run, or `None` when the gate must send
    /// spans nowhere. A host off this machine needs the acknowledgement,
    /// because that host then holds every span the gate inspected.
    pub(crate) fn decision_server(&self) -> Option<(String, f64)> {
        if !self.decision_server_enabled {
            return None;
        }
        let base = self.decision_server_url.trim();
        if base.is_empty() {
            return None;
        }
        if !is_loopback(base) && !self.decision_server_remote_ok {
            return None;
        }
        Some((base.to_string(), self.decision_threshold))
    }
}

fn settings_file(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    let dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

fn read_text(path: &std::path::Path) -> tauri::Result<String> {
    let bytes = std::fs::read(path)?;
    let plain = crypto::open_store_bytes(path, bytes).map_err(std::io::Error::other)?;
    Ok(String::from_utf8_lossy(&plain).to_string())
}

/// Read the stored settings. A missing or unreadable file yields defaults,
/// and `Settings::hide_sensitive_for` redacts an unknown provider, so a broken
/// settings file fails toward redaction.
pub(crate) fn load(app: &tauri::AppHandle) -> tauri::Result<Settings> {
    let path = settings_file(app)?;
    if path.exists() {
        let contents = read_text(&path)?;
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
    std::fs::write(&path, crypto::seal(contents.as_bytes()).map_err(std::io::Error::other)?)?;
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
    match read_text(&path) {
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

/// The decision server for one run, read where the request is built.
pub(crate) fn decision_server(app: &tauri::AppHandle) -> Option<(String, f64)> {
    load(app).ok()?.decision_server()
}

/// The host behind an address, so Settings can name it before you consent to
/// send spans there.
#[tauri::command]
pub(crate) fn decision_host(url: String) -> serde_json::Value {
    serde_json::json!({ "host": host_of(&url), "loopback": is_loopback(&url) })
}

/// Whether an address names this machine. An address this parser cannot read
/// counts as remote, so it needs the acknowledgement.
fn is_loopback(url: &str) -> bool {
    let host = host_of(url);
    host.eq_ignore_ascii_case("localhost") || host.starts_with("127.") || host == "::1"
}

/// The address without a scheme, a port, credentials, or a path.
fn host_of(url: &str) -> String {
    let authority = url.split("://").nth(1).unwrap_or(url);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

/// The setup probe: two spans, no real value, and the two probabilities that
/// show the operating points. A server that floors its answer is refused,
/// because a floor flags every span.
#[tauri::command]
pub(crate) async fn probe_decision_server(url: String) -> Result<serde_json::Value, String> {
    let probe = crate::decide::probe(url.trim()).await?;
    Ok(serde_json::json!({
        "benign": probe.benign,
        "credential": probe.credential,
        "usable": probe.usable(),
    }))
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

    #[test]
    fn an_older_settings_file_keeps_the_threshold() {
        let stored = r#"{"llm":{"provider":"openai"}}"#;
        let settings: Settings = serde_json::from_str(stored).unwrap();
        assert_eq!(settings.decision_threshold, 0.5);
        assert!(!settings.decision_server_enabled);
        assert!(!settings.decision_server_remote_ok);
    }

    #[test]
    fn a_remote_scorer_needs_the_acknowledgement() {
        let mut settings = Settings {
            decision_server_enabled: true,
            decision_server_url: "http://192.168.1.20:8741".into(),
            ..Default::default()
        };
        assert_eq!(settings.decision_server(), None);
        settings.decision_server_remote_ok = true;
        assert!(settings.decision_server().is_some());
    }

    #[test]
    fn a_loopback_scorer_needs_no_acknowledgement() {
        for url in [
            "http://127.0.0.1:8741",
            "http://localhost:8741",
            "http://[::1]:8741",
        ] {
            let settings = Settings {
                decision_server_enabled: true,
                decision_server_url: url.into(),
                ..Default::default()
            };
            assert!(
                settings.decision_server().is_some(),
                "{url} names this machine"
            );
        }
    }
}
