use std::collections::HashMap;
use tauri::Emitter;
use crate::execute::execute_tool;
use crate::log::log;
use crate::report::tool_report;
use crate::route::looks_like_unexecuted_intent;
use crate::tooldefs::{core_tools, enable_skill_tools, to_tool_defs, tool_is_disclosed};

/// User interrupt for the running agent loop. The tool loop is uncapped
/// (real work takes many turns), so the stop button is the brake: checked
/// between turns and between tool calls.
static CHAT_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn chat_stop_requested() -> bool {
    CHAT_STOP.load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
pub(crate) fn stop_chat() {
    if !chat_stop_requested() {
        CHAT_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        log("STOP REQUESTED");
    }
}

/// Build a serializable view of the request (ChatRequest itself doesn't impl Serialize).
fn request_to_json(
    history: &[lmkit::ChatMessage],
    tools: &[lmkit::ToolDefinition],
) -> serde_json::Value {
    let msgs: Vec<serde_json::Value> = history.iter().map(|m| {
        let role = match m.role {
            lmkit::Role::System => "system",
            lmkit::Role::User => "user",
            lmkit::Role::Assistant => "assistant",
            lmkit::Role::Tool => "tool",
        };
        let mut obj = serde_json::json!({ "role": role });
        if let Some(c) = &m.content {
            obj["content"] = serde_json::Value::String(c.clone());
        }
        if let Some(tcs) = &m.tool_calls {
            let arr: Vec<serde_json::Value> = tcs.iter().map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.function.name, "arguments": tc.function.arguments }
                })
            }).collect();
            obj["tool_calls"] = serde_json::Value::Array(arr);
        }
        if let Some(id) = &m.tool_call_id {
            obj["tool_call_id"] = serde_json::Value::String(id.clone());
        }
        if let Some(n) = &m.name {
            obj["name"] = serde_json::Value::String(n.clone());
        }
        obj
    }).collect();
    let tools_arr: Vec<serde_json::Value> = tools.iter().map(|t| {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": t.function.name,
                "description": t.function.description,
                "parameters": t.function.parameters
            }
        })
    }).collect();
    serde_json::json!({ "messages": msgs, "tools": tools_arr })
}

/// A prior conversation turn sent by the frontend so the agent has memory
/// across chat_with_llm invocations.
#[derive(serde::Deserialize)]
pub(crate) struct HistoryItem {
    role: String,
    content: String,
}

/// Send a message to the LLM and return the response.
/// OpenAI-compatible endpoints (custom/openai/ollama) use the native client,
/// which supports image parts so screenshots are actually visible to the
/// model. lmkit handles Anthropic only (text-only chat).
/// Everything one chat turn needs, assembled once by the command and moved
/// into whichever provider path runs.
struct ChatParams {
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
}

// The 8 parameters are the IPC contract with the frontend (one JSON field
// each); collapsing them would change the wire format. Grouping happens in
// ChatParams below.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn chat_with_llm(
    app: tauri::AppHandle,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
) -> Result<String, String> {
    CHAT_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
    let params = ChatParams {
        provider,
        base_url,
        api_key,
        model,
        message,
        prior_history,
        system_prompt,
    };
    if params.provider == "anthropic" {
        chat_via_lmkit(app, params).await
    } else {
        chat_via_openai(app, params).await
    }
}

/// lmkit-backed chat (text-only; used for Anthropic).
async fn chat_via_lmkit(app: tauri::AppHandle, p: ChatParams) -> Result<String, String> {
    let ChatParams {
        provider,
        base_url,
        api_key,
        model,
        message,
        prior_history,
        system_prompt,
    } = p;
    use futures_util::StreamExt;
    use lmkit::{
        create_chat_provider, merge_tool_call_deltas, ChatEvent, ChatMessage,
        ChatRequest, Provider, ProviderConfig, Role, ToolCallDelta,
        ToolDefinition,
    };

    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    if base.is_empty() {
        return Err("base URL is empty".into());
    }
    if model.is_empty() {
        return Err("no model selected".into());
    }
    log(&format!(
        "CHAT START: provider={} base={} model={} msg_len={} sys_len={} prior={}",
        provider, base, model, message.len(), system_prompt.len(), prior_history.len()
    ));

    // Map the provider string to a lmkit Provider.
    let provider_enum = match provider.as_str() {
        "ollama" => Provider::Ollama,
        "anthropic" => Provider::Anthropic,
        _ => Provider::OpenAI, // openai / custom / any OpenAI-compatible endpoint
    };

    // Create the provider with a custom base URL.
    let config = ProviderConfig::with_base_url(provider_enum, api_key, format!("{base}/v1"), model);
    let llm = create_chat_provider(&config).map_err(|e| e.to_string())?;

    // Active tool schemas: core only until a skill's `tools:` enables more
    // (tool availability is skill-driven; see docs/SKILLS.md).
    let mut active: Vec<serde_json::Value> = core_tools();

    // Build the conversation history: system prompt, prior turns, new message.
    let mut history: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt)];
    for h in &prior_history {
        match h.role.as_str() {
            "user" => history.push(ChatMessage::user(&h.content)),
            "assistant" => history.push(ChatMessage::assistant(&h.content)),
            _ => {}
        }
    }
    history.push(ChatMessage::user(message));


    // Per-run memory of tool results for output budgeting (dedupe/truncate).
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut turn = 0;
    let mut nudged = false;
    loop {
        if chat_stop_requested() {
            log(&format!("CHAT STOPPED after {turn} turns"));
            let _ = app.emit("stream_done", serde_json::json!({}));
            return Ok("(stopped)".to_string());
        }
        // Rebuilt each turn: loading a skill mid-run grows the visible set.
        let tools: Vec<ToolDefinition> = to_tool_defs(&active);
        log(&format!("TURN {}: history_msgs={} tools={}", turn, history.len(), tools.len()));
        let request = ChatRequest {
            messages: history.clone(),
            tools: Some(tools.clone()),
            ..Default::default()
        };
        // Serialize the request once; log the head, and dump the full body on error.
        let req_json = match serde_json::to_string(&request_to_json(&history, &tools)) {
            Ok(j) => j,
            Err(e) => {
                log(&format!("TURN {} REQUEST SERIALIZE ERROR: {}", turn, e));
                return Err(e.to_string());
            }
        };
        log(&format!(
            "TURN {} REQUEST: len={} head={:?}",
            turn,
            req_json.len(),
            req_json.chars().take(300).collect::<String>()
        ));

        // Stream the response, accumulating text and tool-call deltas.
        let mut text = String::new();
        let mut deltas: Vec<ToolCallDelta> = Vec::new();
        let mut stream = match llm.complete_stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                log(&format!("TURN {} ERROR: complete_stream failed: {}", turn, e));
                let dump = std::env::temp_dir().join(format!("firstmate_req_t{}.json", turn));
                let _ = std::fs::write(&dump, &req_json);
                log(&format!("TURN {} REQUEST DUMPED to {}", turn, dump.display()));
                return Err(e.to_string());
            }
        };
        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| e.to_string())?;
            match event {
                ChatEvent::Delta(t) => {
                    text.push_str(&t);
                    let _ = app.emit("stream_chunk", serde_json::json!({ "text": t }));
                }
                ChatEvent::ToolCallDelta(d) => deltas.extend(d),
                ChatEvent::Finish(_) => {}
            }
        }
        log(&format!(
            "TURN {} STREAM DONE: text_len={} tool_deltas={} history_msgs={}",
            turn, text.len(), deltas.len(), history.len()
        ));

        // Tool-call turn: execute, feed back, loop.
        let calls = merge_tool_call_deltas(&deltas);
        if !calls.is_empty() {
            let _ = app.emit("stream_reset", serde_json::json!({}));
            history.push(ChatMessage {
                role: Role::Assistant,
                content: Some(text.clone()),
                tool_calls: Some(calls.clone()),
                tool_call_id: None,
                name: None,
            });
            for call in &calls {
                if chat_stop_requested() {
                    log(&format!("CHAT STOPPED mid-turn after {turn} turns"));
                    let _ = app.emit("stream_done", serde_json::json!({}));
                    return Ok("(stopped)".to_string());
                }
                let name = call.function.name.clone();
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                let _ = app.emit(
                    "tool_call",
                    serde_json::json!({
                        "name": &name,
                        "args": &args,
                    }),
                );
                let t0 = std::time::Instant::now();
                let result = if tool_is_disclosed(&active, &name) {
                    execute_tool(&app, &active, &name, &args, &mut seen)
                        .await
                        .unwrap_or_else(|e| format!("Error: {e}"))
                } else {
                    format!("Error: tool '{name}' is not available in this conversation. Its implementation exists but its schema is not disclosed; load the skill whose 'tools:' list enables it, then call it again.")
                };
                let ok = !result.starts_with("Error");
                let report = tool_report(&name, &args, &result, t0.elapsed().as_millis());
                let _ = app.emit("tool_result", serde_json::json!({
                    "name": &name,
                    "transcript": report.transcript(),
                    "report": &report,
                }));
                history.push(ChatMessage::tool(call.id.clone(), result));
                if name == "load_skill" && ok {
                    let skill = args.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let added = enable_skill_tools(&mut active, skill);
                    if !added.is_empty() {
                        log(&format!(
                            "TOOLS ENABLED by skill {skill}: {} (active={})",
                            added.join(", "),
                            active.len()
                        ));
                    }
                }
            }
            log(&format!(
                "TURN {} TOOL TURN: {} calls, history_msgs={}",
                turn, calls.len(), history.len()
            ));
            turn += 1;
            continue;
        }

        // Text-only intent without a tool call: nudge once so "Let me
        // verify X" is actually followed by the verification.
        if !nudged && calls.is_empty() && looks_like_unexecuted_intent(&text) {
            nudged = true;
            log(&format!("INTENT NUDGE: turn {turn} declared action without a tool call: {text:?}"));
            history.push(ChatMessage {
                role: Role::Assistant,
                content: Some(text.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            history.push(ChatMessage {
                role: Role::User,
                content: Some("(continue — execute that now with a tool call)".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            turn += 1;
            continue;
        }

        // Final turn: emit done, return.
        let _ = app.emit("stream_done", serde_json::json!({}));
        return Ok(text);
    }
}

/// Accumulated streaming tool call (OpenAI wire shape).
#[derive(Default)]
struct CallAcc {
    id: String,
    name: String,
    args: String,
}

/// Merge one streamed `tool_calls[]` delta into the accumulator list.
fn merge_tool_call_delta(calls: &mut Vec<CallAcc>, tc: &serde_json::Value) {
    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    while calls.len() <= idx {
        calls.push(CallAcc::default());
    }
    if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
        if !id.is_empty() {
            calls[idx].id = id.to_string();
        }
    }
    if let Some(f) = tc.get("function") {
        if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
            if !n.is_empty() {
                calls[idx].name.push_str(n);
            }
        }
        if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
            calls[idx].args.push_str(a);
        }
    }
}

/// If a tool result reports a saved screenshot, return its path. Handles
/// winapp's variants: `Saved composite: <path>`, `Saved: <path>`, and
/// `Screenshot of "X" ... saved to <path> (WxH, NNNKB)`.
fn extract_screenshot_path(result: &str) -> Option<String> {
    for line in result.lines() {
        if !line.to_lowercase().contains("saved") {
            continue;
        }
        if let Some(tok) = line
            .split_whitespace()
            .rev()
            .find(|t| t.to_lowercase().ends_with(".png"))
        {
            return Some(tok.to_string());
        }
    }
    None
}

/// Native OpenAI-compatible chat: streaming, tool calls, and image parts so
/// screenshots the agent captures are attached and actually seen.
async fn chat_via_openai(app: tauri::AppHandle, p: ChatParams) -> Result<String, String> {
    let ChatParams {
        provider,
        base_url,
        api_key,
        model,
        message,
        prior_history,
        system_prompt,
    } = p;
    use base64::Engine;
    use futures_util::StreamExt;

    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    if base.is_empty() {
        return Err("base URL is empty".into());
    }
    if model.is_empty() {
        return Err("no model selected".into());
    }
    let url = format!("{base}/v1/chat/completions");
    log(&format!(
        "CHAT START: provider={} base={} model={} msg_len={} sys_len={} prior={}",
        provider, base, model, message.len(), system_prompt.len(), prior_history.len()
    ));

    let mut msgs: Vec<serde_json::Value> =
        vec![serde_json::json!({ "role": "system", "content": system_prompt })];
    for h in &prior_history {
        match h.role.as_str() {
            "user" | "assistant" => {
                msgs.push(serde_json::json!({ "role": h.role, "content": h.content }))
            }
            _ => {}
        }
    }
    msgs.push(serde_json::json!({ "role": "user", "content": message }));

    // Core tools only; skills' `tools:` lists grow this set mid-run.
    let mut tools: Vec<serde_json::Value> = core_tools();
    let mut seen: HashMap<String, String> = HashMap::new();
    let client = reqwest::Client::new();
    let mut turn = 0;
    let mut nudged = false;
    loop {
        if chat_stop_requested() {
            log(&format!("CHAT STOPPED after {turn} turns"));
            let _ = app.emit("stream_done", serde_json::json!({}));
            return Ok("(stopped)".to_string());
        }
        let body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "tools": tools,
            "stream": true,
        });
        let body_json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        log(&format!(
            "TURN {} REQUEST: len={} tools={} head={:?}",
            turn,
            body_json.len(),
            tools.len(),
            body_json.chars().take(300).collect::<String>()
        ));
        let mut req = client.post(&url).header("Content-Type", "application/json");
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
        }
        let resp = match req.body(body_json.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                log(&format!("TURN {} ERROR: request failed: {}", turn, e));
                let dump = std::env::temp_dir().join(format!("firstmate_req_t{}.json", turn));
                let _ = std::fs::write(&dump, &body_json);
                log(&format!("TURN {} REQUEST DUMPED to {}", turn, dump.display()));
                return Err(format!("request failed: {e}"));
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            log(&format!(
                "TURN {} ERROR: API {status}: {}",
                turn,
                text.chars().take(400).collect::<String>()
            ));
            let dump = std::env::temp_dir().join(format!("firstmate_req_t{}.json", turn));
            let _ = std::fs::write(&dump, &body_json);
            log(&format!("TURN {} REQUEST DUMPED to {}", turn, dump.display()));
            return Err(format!(
                "API error ({status}): {}",
                text.chars().take(600).collect::<String>()
            ));
        }

        // Parse the SSE stream: text deltas and tool-call deltas.
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut text = String::new();
        let mut calls: Vec<CallAcc> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    log(&format!(
                        "TURN {} SSE parse error: {:?}",
                        turn,
                        data.chars().take(120).collect::<String>()
                    ));
                    continue;
                };
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { continue };
                let delta = choice.get("delta");
                if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                    if !t.is_empty() {
                        text.push_str(t);
                        let _ = app.emit("stream_chunk", serde_json::json!({ "text": t }));
                    }
                }
                if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
                    for tc in tcs {
                        merge_tool_call_delta(&mut calls, tc);
                    }
                }
            }
        }
        log(&format!(
            "TURN {} STREAM DONE: text_len={} tool_calls={} history_msgs={}",
            turn,
            text.len(),
            calls.len(),
            msgs.len()
        ));

        if !calls.is_empty() {
            let _ = app.emit("stream_reset", serde_json::json!({}));
            let tcs: Vec<serde_json::Value> = calls
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.args },
                    })
                })
                .collect();
            msgs.push(serde_json::json!({
                "role": "assistant",
                "content": if text.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(text.clone())
                },
                "tool_calls": tcs,
            }));
            for call in &calls {
                if chat_stop_requested() {
                    log(&format!("CHAT STOPPED mid-turn after {turn} turns"));
                    let _ = app.emit("stream_done", serde_json::json!({}));
                    return Ok("(stopped)".to_string());
                }
                let name = call.name.clone();
                let args: serde_json::Value =
                    serde_json::from_str(&call.args).unwrap_or(serde_json::json!({}));
                let _ = app.emit(
                    "tool_call",
                    serde_json::json!({
                        "name": &name,
                        "args": &args,
                    }),
                );
                let t0 = std::time::Instant::now();
                let result = if tool_is_disclosed(&tools, &name) {
                    execute_tool(&app, &tools, &name, &args, &mut seen)
                        .await
                        .unwrap_or_else(|e| format!("Error: {e}"))
                } else {
                    format!("Error: tool '{name}' is not available in this conversation. Its implementation exists but its schema is not disclosed; load the skill whose 'tools:' list enables it, then call it again.")
                };
                let report = tool_report(&name, &args, &result, t0.elapsed().as_millis());
                let _ = app.emit("tool_result", serde_json::json!({
                    "name": &name,
                    "transcript": report.transcript(),
                    "report": &report,
                }));
                msgs.push(serde_json::json!({
                    "role": "tool", "tool_call_id": call.id, "content": result
                }));
                // Vision loop: if the command saved a screenshot, attach the
                // image so the model actually sees it on the next turn.
                if let Some(path) = extract_screenshot_path(&result) {
                    match tokio::fs::read(&path).await {
                        Ok(bytes) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            msgs.push(serde_json::json!({
                                "role": "user",
                                "content": [
                                    { "type": "text", "text": format!(
                                        "Screenshot captured by the previous command ({}). Look at it before deciding your next action.",
                                        path
                                    ) },
                                    { "type": "image_url", "image_url": {
                                        "url": format!("data:image/png;base64,{}", b64)
                                    } }
                                ]
                            }));
                            log(&format!("VISION: attached {} bytes from {}", bytes.len(), path));
                        }
                        Err(e) => log(&format!("VISION: cannot read {path}: {e}")),
                    }
                }
                if name == "load_skill" && !result.starts_with("Error") {
                    let skill = args.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let added = enable_skill_tools(&mut tools, skill);
                    if !added.is_empty() {
                        log(&format!(
                            "TOOLS ENABLED by skill {skill}: {} (active={})",
                            added.join(", "),
                            tools.len()
                        ));
                    }
                }
            }
            log(&format!(
                "TURN {} TOOL TURN: {} calls, history_msgs={}",
                turn,
                calls.len(),
                msgs.len()
            ));
            turn += 1;
            continue;
        }

        // Text-only intent without a tool call: nudge once (see lmkit path).
        if !nudged && looks_like_unexecuted_intent(&text) {
            nudged = true;
            log(&format!("INTENT NUDGE: turn {turn} declared action without a tool call: {text:?}"));
            msgs.push(serde_json::json!({"role": "assistant", "content": text}));
            msgs.push(serde_json::json!({
                "role": "user",
                "content": "(continue — execute that now with a tool call)"
            }));
            turn += 1;
            continue;
        }

        let _ = app.emit("stream_done", serde_json::json!({}));
        return Ok(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_streamed_tool_call_across_deltas() {
        let mut calls: Vec<CallAcc> = Vec::new();
        merge_tool_call_delta(
            &mut calls,
            &serde_json::json!({"index":0,"id":"call_1","type":"function",
                "function":{"name":"run_command","arguments":""}}),
        );
        merge_tool_call_delta(
            &mut calls,
            &serde_json::json!({"index":0,"function":{"arguments":"{\"com"}}),
        );
        merge_tool_call_delta(
            &mut calls,
            &serde_json::json!({"index":0,"function":{"arguments":"mand\":\"dir\"}"}}),
        );
        // Second call at a higher index.
        merge_tool_call_delta(
            &mut calls,
            &serde_json::json!({"index":1,"id":"call_2","type":"function",
                "function":{"name":"read_file","arguments":"{}"}}),
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "run_command");
        assert_eq!(calls[0].args, r#"{"command":"dir"}"#);
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].name, "read_file");
    }

    #[test]
    fn extract_screenshot_path_from_winapp_output() {
        let composite = "⚠  2 windows detected. Compositing into single image.\n  ✓ Saved composite: C:\\shots\\win.png";
        assert_eq!(
            extract_screenshot_path(composite).as_deref(),
            Some("C:\\shots\\win.png")
        );
        let plain = "  ✓ Saved: C:\\shots\\one.png";
        assert_eq!(
            extract_screenshot_path(plain).as_deref(),
            Some("C:\\shots\\one.png")
        );
        let single = "Screenshot of \"Google — Zen Browser\" (PID 4892) saved to \
                       \\\\wsl.localhost\\Ubuntu\\tmp\\screenshot.png (2560x1392, 185KB)";
        assert_eq!(
            extract_screenshot_path(single).as_deref(),
            Some("\\\\wsl.localhost\\Ubuntu\\tmp\\screenshot.png")
        );
        assert_eq!(extract_screenshot_path("no image here").as_deref(), None);
        // Deduped results never re-attach the image.
        assert_eq!(
            extract_screenshot_path("[identical to your earlier call: 152 bytes, unchanged]").as_deref(),
            None
        );
    }
}
