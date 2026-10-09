//! Provider roster. The rows live in `providers.json`, embedded at compile
//! time. Adding a provider means editing that JSON, never Rust. Request
//! building switches on `Api` and `Auth` only, so no code branch may test a
//! provider id.

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
    /// Default endpoint. An empty base means the user must supply a base
    /// URL.
    pub base: String,
    pub key_hint: String,
    /// An empty API key is valid for this provider. Local engines have no
    /// key.
    #[serde(default)]
    pub key_optional: bool,
    /// The endpoint is user-owned: a local engine or a custom URL. The UI
    /// shows the Base URL field only for these rows.
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

pub(crate) fn lookup(id: &str) -> Option<&'static Spec> {
    rows().iter().find(|s| s.id == id)
}

pub(crate) fn base_is_versioned(base: &str) -> bool {

    base.rsplit('/').next().is_some_and(|seg| {
        let rest = seg.strip_prefix('v').unwrap_or("");
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    })
}

/// A base that ends at a version segment (`/v1`, `/v4`) is already a complete
/// request root. Such a base takes only `/chat/completions` and `/models`
/// appended. Any other base gets a `/v1` segment inserted.
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
        // The UI shows the Base URL field only for local rows, so each
        // local row must carry a default base. `custom` is the user's entry.
        for id in ["ollama", "lm-studio", "llama-cpp", "custom"] {
            let s = lookup(id).unwrap_or_else(|| panic!("{id} missing from roster"));
            assert!(s.is_local, "{id} must be marked isLocal");
        }
        for s in catalog() {
            if s.is_local && s.id != "custom" {
                assert!(!s.base.is_empty(), "{} is local but has no default base", s.id);
            }
        }
        // The roster must carry every protocol family the app routes.
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
