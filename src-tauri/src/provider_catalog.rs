//! Provider roster. The rows live in `providers.json` (embedded at compile
//! time); this module only parses and indexes them. Adding a provider means
//! editing the JSON — no Rust changes. Request building switches on `Api`
//! and `Auth` only; there is no per-provider code.

use std::sync::OnceLock;

/// Wire protocol family the endpoint speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Api {
    /// OpenAI chat/completions + /models listing.
    Openai,
    /// Anthropic Messages API (x-api-key + anthropic-version headers).
    Anthropic,
    /// Google Generative Language API (:generateContent, ?key= auth).
    Gemini,
    /// Ollama native /api/chat + /api/tags.
    Ollama,
}

/// How the request carries credentials.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Auth {
    /// `Authorization: Bearer <key>` header.
    Bearer,
    /// Anthropic-style `x-api-key` (+ version) headers.
    XApiKey,
    /// `?key=<key>` query parameter (Google).
    QueryKey,
    /// No credentials (local engines).
    None,
}
/// One row of the roster.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Spec {
    pub id: String,
    pub label: String,
    pub api: Api,
    pub auth: Auth,
    /// Default endpoint; empty = user must supply one (custom).
    pub base: String,
    /// Placeholder text for the key field.
    pub key_hint: String,
    /// True when an empty key is valid (local engines).
    #[serde(default)]
    pub key_optional: bool,
    /// True when the endpoint is user-owned (local engine or custom URL):
    /// the UI shows the Base URL field only for these rows.
    #[serde(default)]
    pub is_local: bool,
}

fn rows() -> &'static Vec<Spec> {
    static CACHE: OnceLock<Vec<Spec>> = OnceLock::new();
    CACHE.get_or_init(|| {
        serde_json::from_str(include_str!("providers.json"))
            .expect("providers.json must be a valid provider roster")
    })
}

/// All selectable providers, in display order.
pub(crate) fn catalog() -> &'static [Spec] {
    rows()
}

/// Look up one provider by id.
pub(crate) fn lookup(id: &str) -> Option<&'static Spec> {
    rows().iter().find(|s| s.id == id)
}

/// True when `base` already ends at a versioned root (`/v1`, `/v4`, …), so
/// request paths append only `/chat/completions` / `/models`.
pub(crate) fn base_is_versioned(base: &str) -> bool {

    base.rsplit('/').next().is_some_and(|seg| {
        let rest = seg.strip_prefix('v').unwrap_or("");
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Request root: the base itself when it already ends at a version segment,
/// else base + `/v1`. Paths (`/chat/completions`, `/models`) append to this.
pub(crate) fn root(base: &str) -> String {
    if base_is_versioned(base) {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_parses_with_unique_ids_and_valid_bases() {
        let mut seen = std::collections::HashSet::new();
        for s in catalog() {
            assert!(seen.insert(s.id.as_str()), "duplicate provider id {}", s.id);
            assert!(!s.label.is_empty(), "{} has no label", s.id);
            assert!(
                s.base.is_empty() || s.base.starts_with("http://") || s.base.starts_with("https://"),
                "{} base must be a URL or empty",
                s.id
            );
        }
        // Local rows own their endpoint: the UI only shows Base URL for
        // them, so each must carry a default (custom is the user's entry).
        for id in ["ollama", "lm-studio", "llama-cpp", "custom"] {
            let s = lookup(id).unwrap_or_else(|| panic!("{id} missing from roster"));
            assert!(s.is_local, "{id} must be marked isLocal");
        }
        for s in catalog() {
            if s.is_local && s.id != "custom" {
                assert!(!s.base.is_empty(), "{} is local but has no default base", s.id);
            }
        }
        // The three protocol families the app routes must all be present.
        for api in [Api::Openai, Api::Anthropic, Api::Gemini] {
            assert!(catalog().iter().any(|s| s.api == api), "roster lacks api {api:?}");
        }
    }

    #[test]
    fn versioned_base_detection() {
        assert!(base_is_versioned("https://api.openai.com/v1"));
        assert!(base_is_versioned("https://open.bigmodel.cn/api/paas/v4"));
        assert!(!base_is_versioned("https://api.deepinfra.com/v1/openai"));
        assert!(!base_is_versioned("https://models.github.ai/inference"));
        assert!(!base_is_versioned(""));
    }
}
