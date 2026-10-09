use std::collections::HashMap;
use tauri::Emitter;
use crate::context;
use crate::execute::execute_tool;
use crate::log::log;
use crate::provider_catalog::{self, Api};
use crate::report::tool_report;
use crate::route::looks_like_unexecuted_intent;
use crate::tooldefs::{agent_tools, to_tool_defs};

/// User interrupt for the running agent loop. The tool loop is uncapped
/// (real work takes many turns), so the stop button is the brake: checked
/// between turns and between tool calls.
pub(crate) static CHAT_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
#[derive(serde::Deserialize, Clone)]
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
    /// Skills already loaded in this conversation, derived by the frontend
    /// from the tool ledger. Seeds the dispatch gate, so capability earned
    /// earlier in a conversation holds on every later turn.
    loaded_skills: Vec<String>,
    /// Tokens the model can hold; 0 = ask the endpoint (docs/COMPACTION.md).
    context_window: u32,
    /// Share of that window at which the history gets compacted.
    compact_at_percent: u8,
}

// The 11 parameters are the IPC contract with the frontend (one JSON field
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
    loaded_skills: Vec<String>,
    context_window: u32,
    compact_at_percent: u8,
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
        loaded_skills,
        context_window,
        compact_at_percent,
    };
    // Native-protocol providers go through lmkit (it owns the Messages and
    // generateContent wire formats); everything else is OpenAI-compatible.
    let api = provider_catalog::lookup(&params.provider).map(|s| s.api);
    if matches!(api, Some(Api::Anthropic) | Some(Api::Gemini)) {
        chat_via_lmkit(app, params).await
    } else {
        chat_via_openai(app, params).await
    }
}

/// Skills this conversation already loaded ride in the system prompt.
///
/// The persisted ledger records THAT a skill was loaded — the dispatch gate
/// unlocks its tools from that line — but not WHAT it said: the SKILL.md
/// body was a tool result that never persisted. Without this, a resumed
/// conversation gets the tool back and loses the rules that govern it.
///
/// Sorted, deduped, and built once per run before the first request, so the
/// prompt prefix stays byte-stable while the run streams (a mutated system
/// prompt costs a fresh prefill every turn). A skill loaded mid-run needs no
/// injection here: its body arrives in that turn's tool result.
fn system_prompt_with_skills(base: &str, loaded: &[String]) -> String {
    let mut names: Vec<&str> = loaded
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut out = base.to_string();
    for name in names {
        if let Some(body) = crate::skills::skill_body(name) {
            out.push_str(&format!("\n\n## Loaded skill: {name}\n\n{body}\n"));
        }
    }
    out
}

/// The brief rules are content, not code: `prompts/COMPACT.md`, layered like
/// AGENTS.md so the memory policy is editable without a rebuild. With no copy
/// at any layer there is nothing to instruct the summarizer, so folding is
/// skipped and the oversized request goes out instead — and the log says why.
fn brief_rules() -> Option<String> {
    crate::skills::prompt_content("COMPACT.md")
}

/// The window this run must fit, and who said so: the user's setting beats
/// the endpoint's report, which beats the fallback.
async fn resolve_window(
    provider: &str,
    base: &str,
    api_key: &str,
    model: &str,
    configured: u32,
) -> (u32, &'static str) {
    if configured > 0 {
        return (configured, "configured");
    }
    match crate::modelapi::model_context(provider, base, api_key, model).await {
        Some(w) => (w, "endpoint"),
        None => (context::DEFAULT_WINDOW_TOKENS, "fallback"),
    }
}

/// History rows as the wire messages the window is measured in.
fn history_msgs(rows: &[HistoryItem]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|h| serde_json::json!({ "role": h.role, "content": h.content }))
        .collect()
}

/// Where the persisted transcript begins, and where the kept tail begins.
///
/// A `summary` row is framing, not a turn: it belongs in the system prompt,
/// and everything it already compressed never goes back on the wire. What is
/// left is cut by tokens, and only ever at a turn boundary.
fn plan_history(prior: &[HistoryItem], keep_budget: usize) -> (Option<String>, usize, usize) {
    let mut summary = None;
    let mut start = 0usize;
    for (i, h) in prior.iter().enumerate().rev() {
        if h.role == "summary" {
            summary = Some(h.content.clone());
            start = i + 1;
            break;
        }
    }
    let cut = context::turn_anchored_cut(&history_msgs(&prior[start..]), keep_budget);
    (summary, start, cut)
}

/// The instruction the conversation is working under: its first user row,
/// unless the rows being folded already carry it.
///
/// The brief rules require the request as the first line, and a summarizer
/// cannot copy a line it was never shown. The in-run cut keeps the opening
/// row on the wire, so that row is exactly what the dropped middle lacks;
/// the pre-run cut on a later fold sees only the previous brief, so the
/// request would otherwise be re-paraphrased each time until it drifts.
/// Every fold is therefore handed the original text, not the last summary.
fn pinned_opening(
    rows: &[serde_json::Value],
    drop_start: usize,
    drop_end: usize,
) -> Option<String> {
    let (i, row) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.get("role").and_then(|r| r.as_str()) == Some("user"))?;
    if (drop_start..drop_end).contains(&i) {
        return None;
    }
    Some(row_text(row)).filter(|s| !s.trim().is_empty())
}

/// The text of a wire row, whether the content is a string or content parts.
fn row_text(m: &serde_json::Value) -> String {
    match m.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The summarizer's input: the instruction it must carry, the brief it is
/// updating, and the rows being folded.
fn brief_transcript(
    opening: Option<&str>,
    previous: Option<&str>,
    dropped: &[serde_json::Value],
) -> String {
    let mut out = String::new();
    if let Some(t) = opening {
        out.push_str("The instruction this conversation is working under, verbatim. It is not one of the rows below; the brief must carry it as its first line:\n");
        out.push_str(t);
        out.push_str("\n\n---\n\n");
    }
    if let Some(p) = previous {
        out.push_str("Earlier brief — fold it in rather than repeating it:\n");
        out.push_str(p);
        out.push_str("\n\n---\n\n");
    }
    for m in dropped {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let text = row_text(m);
        let calls = m
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|tcs| {
                tcs.iter()
                    .filter_map(|t| t.pointer("/function/name"))
                    .filter_map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .unwrap_or_default();
        if text.trim().is_empty() && calls.is_empty() {
            continue;
        }
        let label = if calls.is_empty() {
            role.to_string()
        } else {
            format!("{role} {calls}")
        };
        out.push_str(&format!("[{label}] {text}\n"));
    }
    // The brief is bounded by its own request: keep the opening (the task)
    // and the tail (what is pending), drop the middle.
    let max = 60_000;
    let count = out.chars().count();
    if count <= max {
        return out;
    }
    let head: String = out.chars().take(max * 3 / 4).collect();
    let tail: String = out.chars().skip(count - max / 4).collect();
    format!("{head}\n[...middle of the dropped transcript omitted...]\n{tail}")
}

/// One non-streaming call that compresses dropped rows into the brief the
/// next turn has instead of them.
async fn summarize_http(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    rules: &str,
    transcript: &str,
) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": rules },
            { "role": "user", "content": transcript },
        ],
        "stream": false,
    });
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body).map_err(|e| e.to_string())?);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("API {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.pointer("/message/content"))
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "no summary in the response".to_string())
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
        loaded_skills,
        context_window,
        compact_at_percent,
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
    // Rules for already-loaded skills join the prompt before it is logged,
    // so sys_len below is what actually goes on the wire.
    let system_prompt = system_prompt_with_skills(&system_prompt, &loaded_skills);

    // Resolved before the endpoint handle takes ownership of the
    // credentials it needs to ask about the model.
    let (window, window_src) =
        resolve_window(&provider, base, &api_key, &model, context_window).await;

    log(&format!(
        "CHAT START: provider={} base={} model={} msg_len={} sys_len={} prior={}",
        provider, base, model, message.len(), system_prompt.len(), prior_history.len()
    ));

    // Map the provider string to a lmkit Provider.
    let provider_enum = match provider.as_str() {
        "ollama" => Provider::Ollama,
        "anthropic" => Provider::Anthropic,
        "google" => Provider::Google,
        _ => Provider::OpenAI, // openai / custom / any OpenAI-compatible endpoint
    };

    // Google's base URL already carries its version segment (/v1beta); the
    // others are rooted at bare hosts and take /v1.
    let config = if matches!(provider_enum, Provider::Google) {
        ProviderConfig::with_base_url(provider_enum, api_key, base, model)
    } else {
        ProviderConfig::with_base_url(provider_enum, api_key, format!("{base}/v1"), model)
    };
    let llm = create_chat_provider(&config).map_err(|e| e.to_string())?;

    // Static tool array: the full registry on every request. Availability
    // is not schema-gated — the dispatch gate in execute.rs enforces the
    // skill contract, and the identical array keeps provider prefix
    // caching warm (docs/SKILLS.md).
    let active: Vec<serde_json::Value> = agent_tools()
        .as_array()
        .expect("tool array")
        .clone();

    // The persisted transcript is windowed the same way on every provider:
    // the brief stands in for whatever is too old to send, and the tail is cut
    // only at a turn boundary (docs/COMPACTION.md).
    let threshold = context::threshold_tokens(window, compact_at_percent);
    let overhead = context::tools_tokens();
    let keep_budget =
        context::target_tokens(threshold).saturating_sub(context::estimate_str(&system_prompt) + overhead);
    let (mut summary, start, mut cut) = plan_history(&prior_history, keep_budget);
    let mut kept_from = start;
    let rules = if cut > 0 { brief_rules() } else { None };
    if cut > 0 && rules.is_none() {
        log("COMPACT: no COMPACT.md to instruct the summarizer; sending the full history");
        cut = 0;
    }
    if cut > 0 {
        let rules = rules.as_deref().expect("checked just above");
        let ledger = history_msgs(&prior_history);
        let opening = pinned_opening(&ledger, start, start + cut);
        let dropped = ledger[start..start + cut].to_vec();
        let brief_request = ChatRequest {
            messages: vec![
                ChatMessage::system(rules),
                ChatMessage::user(brief_transcript(
                    opening.as_deref(),
                    summary.as_deref(),
                    &dropped,
                )),
            ],
            tools: None,
            ..Default::default()
        };
        let brief = async {
            let mut stream = llm
                .complete_stream(&brief_request)
                .await
                .map_err(|e| e.to_string())?;
            let mut out = String::new();
            while let Some(event) = stream.next().await {
                if let Ok(ChatEvent::Delta(t)) = event {
                    out.push_str(&t);
                }
            }
            Ok::<String, String>(out.trim().to_string())
        }
        .await;
        match brief {
            Ok(b) if !b.is_empty() => {
                log(&format!(
                    "COMPACT: {cut} rows folded into the brief ({window_src} window of {window})"
                ));
                let _ = app.emit(
                    "compact",
                    serde_json::json!({ "summary": &b, "tokensBefore": threshold, "dropped": cut }),
                );
                summary = Some(b);
                kept_from = start + cut;
            }
            Ok(_) => log("COMPACT: the summarizer returned nothing; sending the full history"),
            Err(e) => log(&format!(
                "COMPACT: summary call failed ({e}); sending the full history"
            )),
        }
    }
    let system_prompt = context::summary_into(&system_prompt, summary.as_deref());
    log(&format!(
        "CONTEXT: window={window} ({window_src}) threshold={threshold} tools~{overhead} system~{} history_rows~{}",
        context::estimate_str(&system_prompt),
        prior_history.len() - kept_from
    ));

    // Build the conversation history: system prompt, the kept tail, new message.
    let mut history: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt)];
    for h in &prior_history[kept_from..] {
        match h.role.as_str() {
            "user" => history.push(ChatMessage::user(&h.content)),
            "assistant" => history.push(ChatMessage::assistant(&h.content)),
            _ => {}
        }
    }
    history.push(ChatMessage::user(message));


    // Per-run memory of tool results for output budgeting (dedupe/truncate).
    let mut seen: HashMap<String, String> = HashMap::new();
    // The gate's state for THIS conversation: seeded from the ledger the
    // frontend sent, so a skill loaded on an earlier turn keeps its gated
    // tools unlocked; a skill loaded during this run joins the set too.
    let mut loaded_skills: std::collections::HashSet<String> =
        loaded_skills.into_iter().collect();
    let mut turn = 0;
    let mut nudged = false;
    loop {
        if chat_stop_requested() {
            log(&format!("CHAT STOPPED after {turn} turns"));
            let _ = app.emit("stream_done", serde_json::json!({}));
            return Ok("(stopped)".to_string());
        }
        // Identical every turn: the tool array never changes mid-run.
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
                let result = execute_tool(&app, &loaded_skills, &name, &args, &mut seen)
                    .await
                    .unwrap_or_else(|e| format!("Error: {e}"));
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
                    loaded_skills.insert(skill.to_string());
                    log(&format!("SKILL LOADED: {skill} (its gated tools unlocked)"));
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
        loaded_skills,
        context_window,
        compact_at_percent,
    } = p;
    use base64::Engine;
    use futures_util::StreamExt;

    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("base URL is empty".into());
    }
    if model.is_empty() {
        return Err("no model selected".into());
    }
    let url = format!("{}/chat/completions", provider_catalog::root(base));
    // Already-loaded skills come back with their instructions attached.
    let system_prompt = system_prompt_with_skills(&system_prompt, &loaded_skills);
    // The window this run must fit, then the persisted transcript cut to it:
    // the brief stands in for the rows that no longer fit (docs/COMPACTION.md).
    let client = reqwest::Client::new();
    let (window, window_src) =
        resolve_window(&provider, base, &api_key, &model, context_window).await;
    let threshold = context::threshold_tokens(window, compact_at_percent);
    let overhead = context::tools_tokens();
    let keep_budget =
        context::target_tokens(threshold).saturating_sub(context::estimate_str(&system_prompt) + overhead);
    let (mut summary, start, mut cut) = plan_history(&prior_history, keep_budget);
    let mut kept_from = start;
    let rules = if cut > 0 { brief_rules() } else { None };
    if cut > 0 && rules.is_none() {
        log("COMPACT: no COMPACT.md to instruct the summarizer; sending the full history");
        cut = 0;
    }
    if cut > 0 {
        let rules = rules.as_deref().expect("checked just above");
        let ledger = history_msgs(&prior_history);
        let opening = pinned_opening(&ledger, start, start + cut);
        let dropped = ledger[start..start + cut].to_vec();
        let transcript = brief_transcript(opening.as_deref(), summary.as_deref(), &dropped);
        match summarize_http(&client, &url, &api_key, &model, rules, &transcript).await {
            Ok(b) => {
                log(&format!(
                    "COMPACT: {cut} rows folded into the brief ({}% of a {window}-token window, {window_src})",
                    compact_at_percent
                ));
                let _ = app.emit(
                    "compact",
                    serde_json::json!({ "summary": &b, "tokensBefore": threshold, "dropped": cut }),
                );
                summary = Some(b);
                kept_from = start + cut;
            }
            // No brief, no cut: dropping rows with nothing standing in for
            // them would lose the task silently. An oversized request the
            // endpoint refuses is at least visible.
            Err(e) => log(&format!(
                "COMPACT: summary call failed ({e}); sending the full history"
            )),
        }
    }
    let system_prompt = context::summary_into(&system_prompt, summary.as_deref());

    log(&format!(
        "CHAT START: provider={} base={} model={} msg_len={} sys_len={} prior={} sent={} window={window} ({window_src}) threshold={threshold} tools~{overhead}",
        provider,
        base,
        model,
        message.len(),
        system_prompt.len(),
        prior_history.len(),
        prior_history.len() - kept_from
    ));

    let mut msgs: Vec<serde_json::Value> =
        vec![serde_json::json!({ "role": "system", "content": system_prompt })];
    for h in &prior_history[kept_from..] {
        match h.role.as_str() {
            "user" | "assistant" => {
                msgs.push(serde_json::json!({ "role": h.role, "content": h.content }))
            }
            _ => {}
        }
    }
    msgs.push(serde_json::json!({ "role": "user", "content": message }));

    // Static full registry; the dispatch gate in execute.rs enforces the
    // skill contract.
    let tools: Vec<serde_json::Value> = agent_tools().as_array().expect("tool array").clone();
    let mut seen: HashMap<String, String> = HashMap::new();
    // Seeded from the conversation ledger, then extended by this run's loads.
    let mut loaded_skills: std::collections::HashSet<String> =
        loaded_skills.into_iter().collect();
    let mut turn = 0;
    let mut nudged = false;
    // The endpoint's own count of the last request, and how long `msgs` was
    // when it was taken. Silence is fine: the char estimate stands alone.
    let mut measured: Option<usize> = None;
    let mut measured_at = 0usize;
    loop {
        if chat_stop_requested() {
            log(&format!("CHAT STOPPED after {turn} turns"));
            let _ = app.emit("stream_done", serde_json::json!({}));
            return Ok("(stopped)".to_string());
        }
        // The run itself can outgrow the window: 8 KB tool results and
        // screenshots pile up turn by turn. Prune first — no model call, and
        // the spill pointers keep what it elides recoverable — then fold the
        // oldest whole turns into the brief.
        // The measured count floors the estimate: only the rows appended
        // since that request are still guesses.
        let est = {
            let local = context::estimate_tokens(&msgs) + overhead;
            match measured {
                Some(p) if p > local => {
                    p + context::estimate_tokens(&msgs[measured_at.min(msgs.len())..])
                }
                _ => local,
            }
        };
        if est > threshold {
            let freed = context::prune_old_tool_bodies(
                &mut msgs[1..],
                context::prune_protect_tokens(threshold),
                context::prune_savings_floor(threshold),
            );
            if freed > 0 {
                log(&format!(
                    "PRUNE: elided ~{freed} tokens of old tool output, ~{est} -> ~{} tokens",
                    context::estimate_tokens(&msgs) + overhead
                ));
            }
            let est = context::estimate_tokens(&msgs) + overhead;
            if est > threshold {
                let keep = context::target_tokens(threshold)
                    .saturating_sub(context::estimate_tokens(&msgs[..1]) + overhead);
                let (head, tail) = context::middle_out_cut(&msgs[1..], keep);
                if tail <= head {
                    log(&format!(
                        "COMPACT: ~{est} tokens over the window; the newest turn alone exceeds the budget, sending as is"
                    ));
                } else {
                    let dropped = msgs[1 + head..1 + tail].to_vec();
                    let freed = context::estimate_tokens(&dropped);
                    if freed < context::MIN_FOLD_SAVINGS_TOKENS {
                        // What the budget would drop is mostly short rows from
                        // the newest turn: a summariser call and a cold prefix
                        // cache buy nothing here, so send the oversized turn.
                        log(&format!(
                            "COMPACT: ~{est} tokens over the window, only ~{freed} recoverable — not worth a summariser call"
                        ));
                    } else if let Some(rules) = brief_rules() {
                        let opening = pinned_opening(&msgs[1..], head, tail);
                        let transcript = brief_transcript(
                            opening.as_deref(),
                            context::get_summary(&msgs[0]).as_deref(),
                            &dropped,
                        );
                        match summarize_http(&client, &url, &api_key, &model, &rules, &transcript).await {
                            Ok(b) => {
                                msgs.drain(1 + head..1 + tail);
                                context::set_summary(&mut msgs[0], Some(&b));
                                measured = None;
                                let _ = app.emit("compact", serde_json::json!({
                                    "summary": &b, "tokensBefore": est, "dropped": dropped.len()
                                }));
                                log(&format!(
                                    "COMPACT: {} rows folded into the brief, ~{est} -> ~{} tokens",
                                    dropped.len(),
                                    context::estimate_tokens(&msgs) + overhead
                                ));
                            }
                            Err(e) => log(&format!(
                                "COMPACT: summary call failed ({e}); sending the oversized request"
                            )),
                        }
                    } else {
                        log("COMPACT: no COMPACT.md to instruct the summarizer; sending the oversized request");
                    }
                }
            }
        }
        measured_at = msgs.len();
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
                // A usage block rides the final chunk on endpoints that
                // report one; most OpenAI-compatible servers stay silent
                // unless asked, so absence is normal, not an error.
                if let Some(p) = v.pointer("/usage/prompt_tokens").and_then(|p| p.as_u64()) {
                    measured = Some(p as usize);
                }
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
            "TURN {} STREAM DONE: text_len={} tool_calls={} history_msgs={} prompt_tokens={:?}",
            turn,
            text.len(),
            calls.len(),
            msgs.len(),
            measured
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
                let result = execute_tool(&app, &loaded_skills, &name, &args, &mut seen)
                    .await
                    .unwrap_or_else(|e| format!("Error: {e}"));
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
                    loaded_skills.insert(skill.to_string());
                    log(&format!("SKILL LOADED: {skill} (its gated tools unlocked)"));
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

    fn row(role: &str, text: &str) -> HistoryItem {
        HistoryItem { role: role.to_string(), content: text.to_string() }
    }

    #[test]
    fn the_history_window_cuts_whole_turns_and_honours_a_saved_brief() {
        let long = "x".repeat(400);
        let prior = vec![
            row("user", &format!("task one {long}")),
            row("assistant", &format!("did it {long}")),
            row("user", "task two"),
            row("assistant", "done"),
        ];
        // A starved budget drops the older TURN, never half of one: the kept
        // tail begins at a user row, so the request still carries its
        // instruction. This is the bug the old 30-row window shipped.
        let (summary, start, cut) = plan_history(&prior, 10);
        assert_eq!(summary, None);
        assert_eq!((start, cut), (0, 2));
        assert_eq!(prior[start + cut].role, "user");

        // A saved brief is framing, not a turn: it goes to the system prompt
        // and everything it already compressed stays off the wire.
        let with_brief = vec![
            row("user", &format!("ancient {long}")),
            row("summary", "the brief"),
            row("user", &format!("task one {long}")),
            row("assistant", &format!("did it {long}")),
            row("user", "task two"),
            row("assistant", "done"),
        ];
        let (summary, start, cut) = plan_history(&with_brief, 10);
        assert_eq!(summary.as_deref(), Some("the brief"));
        assert_eq!((start, cut), (2, 2), "the pre-brief rows are not counted twice");
        assert_eq!(with_brief[start + cut].role, "user");

        // A single turn cannot be split, however small the budget.
        let one_turn = vec![row("user", "task"), row("assistant", "done")];
        assert_eq!(plan_history(&one_turn, 1).1, 0);
    }

    #[test]
    fn the_summarizer_is_shown_the_instruction_it_must_carry() {
        let long = "x".repeat(400);
        // The in-run shape: the cut keeps the opening row on the wire, so the
        // middle being folded holds no instruction at all. This is the fold
        // that produced a brief reading "user request: not explicit in
        // transcript" — the summarizer was never given the line to copy.
        let wire = vec![
            serde_json::json!({"role": "user", "content": "count the fonts folder"}),
            serde_json::json!({"role": "assistant", "content": format!("listing {long}")}),
            serde_json::json!({"role": "tool", "content": format!("338 entries {long}")}),
            serde_json::json!({"role": "assistant", "content": "now the .NET folder"}),
        ];
        let opening = pinned_opening(&wire, 1, 3).expect("the kept head must be offered");
        assert_eq!(opening, "count the fonts folder");
        let t = brief_transcript(Some(&opening), None, &wire[1..3]);
        assert!(
            t.starts_with("The instruction this conversation is working under"),
            "the request leads the summarizer's input: {t}"
        );
        assert_eq!(
            t.matches("count the fonts folder").count(),
            1,
            "it is offered once, as the instruction, not again as a row"
        );
        assert!(t.contains("[tool] 338 entries"), "the folded rows follow it: {t}");

        // The pre-run shape: the cut is a prefix, so when it swallows the
        // opening row the dropped material already carries it.
        let ledger = history_msgs(&[row("user", "count the fonts folder"), row("assistant", "did it")]);
        assert_eq!(pinned_opening(&ledger, 0, 1), None);
        assert_eq!(
            pinned_opening(&ledger, 1, 2).as_deref(),
            Some("count the fonts folder"),
            "a later fold re-reads the original request, not the last paraphrase"
        );
    }

    #[test]
    fn a_fold_lands_under_the_threshold_with_room_to_grow() {
        // The thrash this prevents: trimming to the threshold leaves the next
        // tool result over the line, so a long run pays for a summariser call
        // on every turn.
        let long = "x".repeat(400);
        let prior: Vec<HistoryItem> = (0..40)
            .map(|i| {
                row(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("turn {i} {long}"),
                )
            })
            .collect();
        let threshold = context::threshold_tokens(12_000, 60);
        let keep = context::target_tokens(threshold).saturating_sub(context::tools_tokens());
        let (start, cut) = {
            let (s, start, cut) = plan_history(&prior, keep);
            assert_eq!(s, None);
            (start, cut)
        };
        let after =
            context::estimate_tokens(&history_msgs(&prior[start + cut..])) + context::tools_tokens();
        assert!(cut > 0, "the history must actually be folded");
        assert!(after <= threshold, "the fold must land under the threshold: {after}");
        assert!(
            threshold - after > threshold / 10,
            "and leave headroom before the next fold: {after} of {threshold}"
        );
    }

    #[test]
    fn the_brief_rules_are_content_not_code() {
        // The summarizer's instructions are prompt text, so they live in
        // prompts/COMPACT.md and the binary carries none. In the dev layout
        // the fold must read the working copy under test — any difference
        // means the rules crept back into Rust.
        assert_eq!(
            brief_rules().unwrap(),
            std::fs::read_to_string("../prompts/COMPACT.md").unwrap()
        );
    }

    #[test]
    fn the_brief_transcript_labels_calls_and_folds_the_previous_brief() {
        let dropped = vec![
            serde_json::json!({"role": "user", "content": "open the drivers page"}),
            serde_json::json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "run_command", "arguments": "{}"}}
            ]}),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "0 entries"}),
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": "a screenshot was captured"},
                {"type": "image_url", "image_url": {"url": "AAAA"}}
            ]}),
        ];
        let t = brief_transcript(None, Some("user wanted drivers"), &dropped);
        assert!(t.starts_with("Earlier brief"), "the old brief is folded in: {t}");
        assert!(t.contains("user wanted drivers"));
        assert!(t.contains("[user] open the drivers page"));
        // The call names itself, so the result that follows is identifiable.
        assert!(t.contains("[assistant run_command]"));
        assert!(t.contains("[tool] 0 entries"));
        // Image parts contribute their text, not their base64.
        assert!(t.contains("a screenshot was captured") && !t.contains("AAAA"));
    }

    #[test]
    fn resumed_skills_carry_their_instructions_into_the_prompt() {
        // The ledger names the skills; the prompt must carry their rules too.
        let loaded = ["winapp".to_string(), "filesystem".to_string(), "winapp".to_string()];
        let out = system_prompt_with_skills("BASE", &loaded);
        assert!(out.starts_with("BASE\n\n## Loaded skill: "));
        let fs = out
            .find("## Loaded skill: filesystem")
            .expect("filesystem rules");
        let wa = out.find("## Loaded skill: winapp").expect("winapp rules");
        assert!(fs < wa, "sorted, so the prefix is stable across runs");
        assert!(
            out[fs..wa].chars().count() > 200,
            "the body, not just the skill's name"
        );
        // Unknown skills inject no empty section; nothing loaded is a no-op.
        let unknown = system_prompt_with_skills("BASE", &["no-such-skill".to_string()]);
        assert_eq!(unknown, "BASE");
        assert_eq!(system_prompt_with_skills("BASE", &[]), "BASE");
    }

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
