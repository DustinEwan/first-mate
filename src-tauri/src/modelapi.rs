use crate::provider_catalog::{self, Api};

/// The frontend renders these rows verbatim.
#[tauri::command]
pub(crate) fn list_providers() -> Vec<provider_catalog::Spec> {
    provider_catalog::catalog().to_vec()
}

fn api_of(provider: &str) -> Api {
    provider_catalog::lookup(provider).map(|s| s.api).unwrap_or(Api::Openai)
}

#[tauri::command]
pub(crate) async fn list_models(provider: String, base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::new();
    let models: Vec<String> = match api_of(&provider) {
        Api::Ollama => {
            let body: serde_json::Value = client
                .get(format!("{base}/api/tags"))
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            body.get("models")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        Api::Gemini => {
            let mut url = format!("{base}/models");
            if !api_key.is_empty() {
                url.push_str(&format!("?key={api_key}"));
            }
            let body: serde_json::Value = client
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            body.get("models")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                        .map(|n| n.strip_prefix("models/").map_or(n.clone(), String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        Api::Anthropic => {
            let mut req = client
                .get(format!("{base}/v1/models"))
                .header("anthropic-version", "2023-06-01");
            if !api_key.is_empty() {
                req = req.header("x-api-key", api_key);
            }
            let body: serde_json::Value = req
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            body.get("data")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        Api::Openai => {
            let mut req = client.get(format!("{}/models", provider_catalog::root(base)));
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }
            let body: serde_json::Value = req
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            body.get("data")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
    };
    Ok(models)
}

type WindowMap = std::collections::HashMap<(String, String), Option<u32>>;

/// Endpoint answers, including "no window reported". A model that reports
/// no window must not cost a listing round trip on every chat turn.
static WINDOW_CACHE: std::sync::LazyLock<std::sync::Mutex<WindowMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(WindowMap::new()));

/// The context window an endpoint reports for a model. Compaction needs a
/// window before compaction can state a threshold. llama.cpp and its clones
/// report `max_model_len`. Ollama nests the value inside `model_info`. A
/// silent host leaves the caller to the configured fallback.
pub(crate) async fn model_context(
    provider: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Option<u32> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() || model.is_empty() {
        return None;
    }
    let key = (base.to_string(), model.to_string());
    if let Some(hit) = WINDOW_CACHE.lock().ok()?.get(&key).copied() {
        return hit;
    }
    let found = fetch_context(provider, base, api_key, model).await;
    if let Ok(mut c) = WINDOW_CACHE.lock() {
        c.insert(key, found);
    }
    found
}

async fn fetch_context(provider: &str, base: &str, api_key: &str, model: &str) -> Option<u32> {
    let client = reqwest::Client::new();
    match api_of(provider) {
        Api::Ollama => {
            let body: serde_json::Value = client
                .post(format!("{base}/api/show"))
                .json(&serde_json::json!({ "model": model }))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            body.get("model_info")
                .and_then(|i| i.as_object())?
                .iter()
                .find(|(k, _)| k.ends_with("context_length"))
                .and_then(|(_, v)| v.as_u64())
                .map(|v| v as u32)
        }
        Api::Openai => {
            let mut req = client.get(format!("{}/models", provider_catalog::root(base)));
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }
            let body: serde_json::Value = req.send().await.ok()?.json().await.ok()?;
            body.get("data")
                .and_then(|d| d.as_array())?
                .iter()
                .find(|m| m.get("id").and_then(|i| i.as_str()) == Some(model))
                .and_then(window_of_entry)
        }
        // Anthropic and Google report no per-model window on their listing.
        _ => None,
    }
}

fn window_of_entry(entry: &serde_json::Value) -> Option<u32> {
    ["max_model_len", "n_ctx", "context_length", "context_window"]
        .iter()
        .find_map(|k| entry.get(*k).and_then(|v| v.as_u64()))
        .map(|v| v as u32)
}

#[tauri::command]
pub(crate) async fn test_llm(provider: String, base_url: String, api_key: String, model: String) -> Result<String, String> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("base URL is empty".into());
    }
    if model.is_empty() {
        return Err("no model selected".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let (status, text) = match api_of(&provider) {
        Api::Ollama => {
            let body = serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "stream": false,
            });
            let resp = client
                .post(format!("{base}/api/chat"))
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            (resp.status(), resp.text().await.map_err(|e| e.to_string())?)
        }
        Api::Gemini => {
            let mut url = format!("{base}/models/{model}:generateContent");
            if !api_key.is_empty() {
                url.push_str(&format!("?key={api_key}"));
            }
            let body = serde_json::json!({ "contents": [{ "parts": [{ "text": "ping" }] }] });
            let resp = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            (resp.status(), resp.text().await.map_err(|e| e.to_string())?)
        }
        Api::Anthropic => {
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "ping"}],
            });
            let mut req = client
                .post(format!("{base}/v1/messages"))
                .header("anthropic-version", "2023-06-01")
                .json(&body);
            if !api_key.is_empty() {
                req = req.header("x-api-key", api_key);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            (resp.status(), resp.text().await.map_err(|e| e.to_string())?)
        }
        Api::Openai => {
            let body = serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
            });
            let mut req = client
                .post(format!("{}/chat/completions", provider_catalog::root(base)))
                .json(&body);
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            (resp.status(), resp.text().await.map_err(|e| e.to_string())?)
        }
    };
    if !status.is_success() {
        return Err(format!("HTTP {status}: {text}"));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let reply = match api_of(&provider) {
        Api::Ollama => v.pointer("/message/content").and_then(|c| c.as_str()).unwrap_or(""),
        Api::Gemini => v
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|c| c.as_str())
            .unwrap_or(""),
        Api::Anthropic => v
            .pointer("/content/0/text")
            .and_then(|c| c.as_str())
            .unwrap_or(""),
        Api::Openai => v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .unwrap_or(""),
    };
    Ok(format!("OK — {model} replied: {reply}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// The server accepts one request, replies with `resp_body`, and sends
    /// the raw request head on the channel.
    fn mock(resp_body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = tx.send(head);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    fn head(rx: &std::sync::mpsc::Receiver<String>) -> String {
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn openai_listing_appends_v1_when_base_unversioned() {
        let (base, rx) = mock(r#"{"data":[{"id":"gpt-x"}]}"#);
        let models = list_models("openai".into(), base.clone(), "sk-k".into()).await.unwrap();
        assert_eq!(models, vec!["gpt-x".to_string()]);
        let h = head(&rx);
        assert!(h.starts_with("GET /v1/models"), "{h}");
        assert!(h.contains("authorization: Bearer sk-k"), "{h}");
    }

    #[tokio::test]
    async fn versioned_base_is_used_verbatim() {
        // zhipu's /v4 root must not get a /v1 inserted.
        let (base, rx) = mock(r#"{"data":[{"id":"glm-4"}]}"#);
        let models = list_models("zhipu".into(), format!("{base}/api/paas/v4"), "k".into()).await.unwrap();
        assert_eq!(models, vec!["glm-4".to_string()]);
        assert!(head(&rx).starts_with("GET /api/paas/v4/models"));
    }

    #[tokio::test]
    async fn unversioned_path_root_still_gets_v1() {
        let (base, rx) = mock(r#"{"data":[{"id":"mix"}]}"#);
        list_models("deepinfra".into(), format!("{base}/v1/openai"), "k".into()).await.unwrap();
        assert!(head(&rx).starts_with("GET /v1/openai/v1/models"));
    }

    #[tokio::test]
    async fn gemini_listing_uses_query_key_and_strips_models_prefix() {
        let (base, rx) = mock(r#"{"models":[{"name":"models/gemini-2.5-flash"}]}"#);
        let models = list_models("google".into(), format!("{base}/v1beta"), "AIzaK".into()).await.unwrap();
        assert_eq!(models, vec!["gemini-2.5-flash".to_string()]);
        assert!(head(&rx).starts_with("GET /v1beta/models?key=AIzaK"));
    }

    #[tokio::test]
    async fn anthropic_listing_uses_x_api_key_headers() {
        let (base, rx) = mock(r#"{"data":[{"id":"claude-y"}]}"#);
        let models = list_models("anthropic".into(), base, "sk-ant-k".into()).await.unwrap();
        assert_eq!(models, vec!["claude-y".to_string()]);
        let h = head(&rx);
        assert!(h.starts_with("GET /v1/models"), "{h}");
        assert!(h.contains("x-api-key: sk-ant-k"), "{h}");
        assert!(h.contains("anthropic-version: 2023-06-01"), "{h}");
    }

    #[tokio::test]
    async fn ollama_listing_uses_native_tags_endpoint() {
        let (base, rx) = mock(r#"{"models":[{"name":"llama3:latest"}]}"#);
        let models = list_models("ollama".into(), base, String::new()).await.unwrap();
        assert_eq!(models, vec!["llama3:latest".to_string()]);
        assert!(head(&rx).starts_with("GET /api/tags"));
    }

    #[tokio::test]
    async fn a_llama_style_listing_reports_the_context_window() {
        let (base, _rx) = mock(
            r#"{"data":[{"id":"other","max_model_len":8192},{"id":"glm","max_model_len":200000}]}"#,
        );
        assert_eq!(
            model_context("custom", &base, "", "glm").await,
            Some(200_000),
            "the window of the selected model, not the first row"
        );
    }

    #[tokio::test]
    async fn a_silent_endpoint_is_asked_once_not_every_turn() {
        let (base, rx) = mock(r#"{"data":[{"id":"gpt-x"}]}"#);
        assert_eq!(model_context("openai", &base, "", "gpt-x").await, None);
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
            "the first lookup asks the endpoint"
        );
        assert!(
            rx.try_recv().is_err(),
            "a model that reports nothing is cached as reporting nothing"
        );
    }

    #[tokio::test]
    async fn test_llm_gemini_posts_generate_content_and_reads_candidate() {
        let (base, rx) = mock(r#"{"candidates":[{"content":{"parts":[{"text":"pong"}]}}]}"#);
        let reply = test_llm("google".into(), format!("{base}/v1beta"), "AIzaK".into(), "gemini-x".into())
            .await
            .unwrap();
        assert!(reply.contains("pong"), "{reply}");
        assert!(head(&rx).starts_with("POST /v1beta/models/gemini-x:generateContent?key=AIzaK"));
    }

    #[tokio::test]
    async fn test_llm_openai_posts_chat_completions() {
        let (base, rx) = mock(r#"{"choices":[{"message":{"content":"pong"}}]}"#);
        let reply = test_llm("groq".into(), format!("{base}/openai/v1"), "gsk-k".into(), "llama".into())
            .await
            .unwrap();
        assert!(reply.contains("pong"), "{reply}");
        let h = head(&rx);
        assert!(h.starts_with("POST /openai/v1/chat/completions"), "{h}");
        assert!(h.contains("authorization: Bearer gsk-k"), "{h}");
    }
}
