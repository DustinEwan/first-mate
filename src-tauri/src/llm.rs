use std::collections::HashMap;
use tauri::Emitter;
use crate::context;
use crate::decide;
use crate::execute::execute_tool;
use crate::log::{dump_request, log};
use crate::provider_catalog::{self, Api};
use crate::redact;
use crate::report::tool_report;
use crate::route::looks_like_unexecuted_intent;
use crate::settings;
use crate::tooldefs::{agent_tools, to_tool_defs};

/// The tool loop has no turn cap, so a stop request is the only bound on
/// it. The loop checks this flag between turns and between each tool call.
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

/// `lmkit::ChatRequest` does not implement `Serialize`.
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

/// `chat_with_llm` holds no conversation state between calls. The frontend
/// therefore resends the transcript on every call, one row per turn.
#[derive(serde::Deserialize, Clone)]
pub(crate) struct HistoryItem {
    role: String,
    content: String,
}

/// One chat turn's inputs, moved whole into whichever provider path runs.
struct ChatParams {
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
    /// Loaded skills, sent by the frontend from the tool ledger. The list
    /// seeds the dispatch gate, so tools unlocked on an earlier turn stay
    /// unlocked.
    loaded_skills: Vec<String>,
    /// `docs/COMPACTION.md` defines the `context_window` 0 sentinel.
    context_window: u32,
    compact_at_percent: u8,
}

// Each parameter is one JSON field in the frontend IPC contract, so the
// flat signature is required. ChatParams groups the fields for the
// provider paths.
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
    // lmkit owns the Anthropic and Gemini wire formats, so those providers
    // use it. Every other provider speaks the OpenAI-compatible HTTP API.
    let api = provider_catalog::lookup(&params.provider).map(|s| s.api);
    if matches!(api, Some(Api::Anthropic) | Some(Api::Gemini)) {
        chat_via_lmkit(app, params).await
    } else {
        chat_via_openai(app, params).await
    }
}

/// The redaction pass for this run, or `None` when the provider's `Hide
/// sensitive information` box is unchecked. The trigger is read here from the
/// stored setting, so no caller can put a request on the wire without it.
fn redactor(app: &tauri::AppHandle, provider: &str) -> Option<redact::Redactor> {
    let (hide, values) = settings::redaction_for(app, provider);
    if !hide {
        return None;
    }
    let mut red = redact::Redactor::new();
    for value in values {
        red.seed(&value);
    }
    Some(red)
}

/// The decision server for this run, or `None` when the user left it off.
struct Scorer {
    base: String,
    threshold: f64,
}

fn scorer(app: &tauri::AppHandle) -> Option<Scorer> {
    let (base, threshold) = settings::decision_server(app)?;
    Some(Scorer { base, threshold })
}

/// Ask the server about each span this request carries for the first time, and
/// replace a flagged span with one token. A server that fails leaves the exact
/// rules in force, because the gate only advances detection and one unanswered
/// question must not end a turn. A span the gate never asked about stays
/// unscored, so the next turn asks again.
async fn gate(red: &mut redact::Redactor, scorer: &Scorer, spans: Vec<String>) {
    for span in spans {
        match decide::score(&scorer.base, &span).await {
            Ok(p) => {
                if red.record_score(&span, p, scorer.threshold) {
                    log(&format!("GATE: flagged a span at {p:.2}"));
                }
            }
            Err(e) => {
                log(&format!("GATE: {e}; sending on the exact rules"));
                return;
            }
        }
    }
}

/// The wire copy of the history. The stored transcript keeps real values, so
/// the pass runs at this boundary and nowhere else.
fn tokenize_chat(red: &mut redact::Redactor, msgs: &mut [lmkit::ChatMessage]) {
    for m in msgs.iter_mut() {
        if let Some(content) = m.content.as_mut() {
            *content = red.tokenize(content);
        }
        for call in m.tool_calls.iter_mut().flatten() {
            call.function.arguments = red.tokenize(&call.function.arguments);
        }
    }
}

/// What the console shows for one streamed delta. A token that straddles two
/// chunks waits in `carry` for its close.
fn shown(red: &Option<redact::Redactor>, carry: &mut String, delta: &str) -> String {
    match red {
        Some(r) => r.detokenize_stream(carry, delta),
        None => delta.to_string(),
    }
}

/// The chip event. A run that replaced nothing emits nothing.
fn emit_redacted(app: &tauri::AppHandle, red: &Option<redact::Redactor>) {
    let Some(r) = red else { return };
    let count = r.redacted_count();
    if count > 0 {
        let _ = app.emit(
            "redact",
            serde_json::json!({ "count": count, "classes": r.breakdown() }),
        );
    }
}

/// The tool ledger records which skills loaded, not what their `SKILL.md`
/// said. A resumed conversation regains the gated tools without the rules
/// that govern them.
///
/// A mutated system prompt costs a fresh prefill on every turn. The
/// prompt must be built once per run, before the first request.
///
/// A skill loaded mid-run needs no injection here: its body arrives in
/// that turn's tool result.
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

/// The brief rules are content, not code. They live in `prompts/COMPACT.md`,
/// layered like `AGENTS.md`, so the memory policy is editable without a
/// rebuild. With no copy at any layer, nothing instructs the summarizer.
/// Folding is skipped. The oversized request goes out instead. The log
/// records that choice.
fn brief_rules() -> Option<String> {
    crate::skills::prompt_content("COMPACT.md")
}

/// The window this run must fit, and who said so. The user's setting beats
/// the endpoint's report. The endpoint's report beats the fallback.
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
/// Rows the summary compressed never return to the wire. `plan_history` cuts
/// the rest by tokens, and only at a turn boundary.
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
/// The brief rules require the request as the first line. A summarizer cannot
/// copy a line it was never shown. The in-run cut keeps the opening row on the
/// wire, so the dropped middle lacks that line. The pre-run cut on a later fold
/// sees only the previous brief. Without the pin, the request would be
/// re-paraphrased on every fold until it drifts. Every fold is therefore handed
/// the original text, not the last summary.
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
    // The brief is bounded by its own request. Keep the opening (the task)
    // and the tail (what is pending). Drop the middle.
    let max = 60_000;
    let count = out.chars().count();
    if count <= max {
        return out;
    }
    let head: String = out.chars().take(max * 3 / 4).collect();
    let tail: String = out.chars().skip(count - max / 4).collect();
    format!("{head}\n[...middle of the dropped transcript omitted...]\n{tail}")
}

/// The next turn carries the brief instead of the rows it replaced.
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

/// The lmkit-backed path: text only, no image parts. Anthropic and Gemini
/// route here.
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
    // Rules for already-loaded skills join the prompt before the prompt is
    // logged. The logged sys_len is therefore the value that goes on the wire.
    let system_prompt = system_prompt_with_skills(&system_prompt, &loaded_skills);

    // Resolved before the endpoint handle takes ownership of the
    // credentials it needs to ask about the model.
    let (window, window_src) =
        resolve_window(&provider, base, &api_key, &model, context_window).await;

    log(&format!(
        "CHAT START: provider={} base={} model={} msg_len={} sys_len={} prior={}",
        provider, base, model, message.len(), system_prompt.len(), prior_history.len()
    ));

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
    let mut red = redactor(&app, &provider);
    let scorer = scorer(&app);
    let mut carry = String::new();

    // Every request carries the full registry, because the tool array is
    // static. Availability is not schema-gated: the dispatch gate in
    // `execute.rs` enforces the skill contract. The identical array also keeps
    // provider prefix caching warm (docs/SKILLS.md).
    let active: Vec<serde_json::Value> = agent_tools()
        .as_array()
        .expect("tool array")
        .clone();

    // Every provider windows the persisted transcript the same way. The brief
    // stands in for rows too old to send. The tail is cut only at a turn
    // boundary (docs/COMPACTION.md).
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

    let mut history: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt)];
    for h in &prior_history[kept_from..] {
        match h.role.as_str() {
            "user" => history.push(ChatMessage::user(&h.content)),
            "assistant" => history.push(ChatMessage::assistant(&h.content)),
            _ => {}
        }
    }
    history.push(ChatMessage::user(message));

    // Tool results are remembered for this run only, so a repeat can be
    // deduped or truncated.
    let mut seen: HashMap<String, String> = HashMap::new();
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
        let tools: Vec<ToolDefinition> = to_tool_defs(&active);
        // The token map lives for one run, so the live history keeps real
        // values and only the outbound copy carries handles. A handle stored in
        // the live history reaches the transcript, and the next run's fresh map
        // cannot restore it.
        let mut wire = history.clone();
        if let Some(r) = red.as_mut() {
            if let Some(s) = scorer.as_ref() {
                let mut found: Vec<&str> = Vec::new();
                for m in wire.iter().rev() {
                    if let Some(c) = m.content.as_deref() {
                        found.push(c);
                    }
                    for call in m.tool_calls.iter().flatten() {
                        found.push(&call.function.arguments);
                    }
                }
                let spans = r.pending_from(found.into_iter());
                gate(r, s, spans).await;
            }
            tokenize_chat(r, &mut wire);
        }
        log(&format!("TURN {}: history_msgs={} tools={}", turn, wire.len(), tools.len()));
        let req_json = match serde_json::to_string(&request_to_json(&wire, &tools)) {
            Ok(j) => j,
            Err(e) => {
                log(&format!("TURN {} REQUEST SERIALIZE ERROR: {}", turn, e));
                return Err(e.to_string());
            }
        };
        let request = ChatRequest {
            messages: wire,
            tools: Some(tools),
            ..Default::default()
        };
        log(&format!(
            "TURN {} REQUEST: len={} head={:?}",
            turn,
            req_json.len(),
            req_json.chars().take(300).collect::<String>()
        ));

        let mut text = String::new();
        let mut deltas: Vec<ToolCallDelta> = Vec::new();
        let mut stream = match llm.complete_stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                log(&format!("TURN {} ERROR: complete_stream failed: {}", turn, e));
                let dump = dump_request(turn, &req_json);
                log(&format!("TURN {} REQUEST DUMPED to {}", turn, dump.display()));
                return Err(e.to_string());
            }
        };
        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| e.to_string())?;
            match event {
                ChatEvent::Delta(t) => {
                    let shown = shown(&red, &mut carry, &t);
                    text.push_str(&t);
                    let _ = app.emit("stream_chunk", serde_json::json!({ "text": shown }));
                }
                ChatEvent::ToolCallDelta(d) => deltas.extend(d),
                ChatEvent::Finish(_) => {}
            }
        }
        if let Some(r) = red.as_ref() {
            let tail = r.flush(&mut carry);
            if !tail.is_empty() {
                let _ = app.emit("stream_chunk", serde_json::json!({ "text": tail }));
            }
        }
        log(&format!(
            "TURN {} STREAM DONE: text_len={} tool_deltas={} history_msgs={}",
            turn, text.len(), deltas.len(), history.len()
        ));

        let calls = merge_tool_call_deltas(&deltas);
        if !calls.is_empty() {
            let _ = app.emit("stream_reset", serde_json::json!({}));
            let mut stored = calls.clone();
            for call in stored.iter_mut() {
                if let Some(r) = red.as_ref() {
                    call.function.arguments = r.detokenize(&call.function.arguments);
                }
            }
            history.push(ChatMessage {
                role: Role::Assistant,
                content: Some(text.clone()),
                tool_calls: Some(stored),
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
                let mut args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                if let Some(r) = red.as_ref() {
                    r.detokenize_json(&mut args);
                }
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

        // A reply that declares an action but calls no tool gets one nudge.
        // The nudge makes "Let me verify X" produce the verification.
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

        emit_redacted(&app, &red);
        let _ = app.emit("stream_done", serde_json::json!({}));
        return Ok(match red.as_ref() {
            Some(r) => r.detokenize(&text),
            None => text,
        });
    }
}

/// One tool call accumulated across streamed deltas, in the OpenAI wire shape.
#[derive(Default)]
struct CallAcc {
    id: String,
    name: String,
    args: String,
}

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

/// If a tool result reports a saved screenshot, return its path. winapp uses
/// three phrasings: `Saved composite: <path>`, `Saved: <path>`, and
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

/// The native OpenAI-compatible path. It streams, handles tool calls, and sends
/// image parts, so the agent sees the screenshots it captures.
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
    let system_prompt = system_prompt_with_skills(&system_prompt, &loaded_skills);
    let client = reqwest::Client::new();
    let (window, window_src) =
        resolve_window(&provider, base, &api_key, &model, context_window).await;
    let mut red = redactor(&app, &provider);
    let scorer = scorer(&app);
    let mut carry = String::new();

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
            // No brief means no cut. Dropping rows with nothing standing in
            // for them would lose the task silently. An oversized request that
            // the endpoint refuses is at least visible.
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

    let tools: Vec<serde_json::Value> = agent_tools().as_array().expect("tool array").clone();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut loaded_skills: std::collections::HashSet<String> =
        loaded_skills.into_iter().collect();
    let mut turn = 0;
    let mut nudged = false;
    // The endpoint's own token count for the last request, and the length of
    // `msgs` when it was taken. Most endpoints report nothing, and the char
    // estimate then stands alone.
    let mut measured: Option<usize> = None;
    let mut measured_at = 0usize;
    loop {
        if chat_stop_requested() {
            log(&format!("CHAT STOPPED after {turn} turns"));
            let _ = app.emit("stream_done", serde_json::json!({}));
            return Ok("(stopped)".to_string());
        }
        // The run itself can outgrow the window, because 8 KB tool results and
        // screenshots accumulate turn by turn. Prune first: pruning costs no
        // model call, and the spill pointers keep the elided text recoverable.
        // Then fold the oldest whole turns into the brief.
        // The measured count floors the estimate. Only the rows appended since
        // that request are still guesses.
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
                        // The budget would drop mostly short rows from the
                        // newest turn. A summarizer call would cost a call and
                        // free almost nothing, so send the oversized turn.
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
        // The token map lives for one run, so the live buffer keeps real values
        // and only the outbound copy carries handles.
        let mut wire = msgs.clone();
        if let Some(r) = red.as_mut() {
            if let Some(s) = scorer.as_ref() {
                let spans = r.pending(&mut wire);
                gate(r, s, spans).await;
            }
            r.tokenize_msgs(&mut wire);
        }
        measured_at = msgs.len();
        let body = serde_json::json!({
            "model": model,
            "messages": wire,
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
                let dump = dump_request(turn, &body_json);
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
            let dump = dump_request(turn, &body_json);
            log(&format!("TURN {} REQUEST DUMPED to {}", turn, dump.display()));
            return Err(format!(
                "API error ({status}): {}",
                text.chars().take(600).collect::<String>()
            ));
        }

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
                // A usage block rides the final chunk on endpoints that report
                // one. Most OpenAI-compatible servers stay silent unless asked,
                // so absence is normal rather than an error.
                if let Some(p) = v.pointer("/usage/prompt_tokens").and_then(|p| p.as_u64()) {
                    measured = Some(p as usize);
                }
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { continue };
                let delta = choice.get("delta");
                if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                    if !t.is_empty() {
                        let shown = shown(&red, &mut carry, t);
                        text.push_str(t);
                        let _ = app.emit("stream_chunk", serde_json::json!({ "text": shown }));
                    }
                }
                if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
                    for tc in tcs {
                        merge_tool_call_delta(&mut calls, tc);
                    }
                }
            }
        }
        if let Some(r) = red.as_ref() {
            let tail = r.flush(&mut carry);
            if !tail.is_empty() {
                let _ = app.emit("stream_chunk", serde_json::json!({ "text": tail }));
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
                    // The live buffer keeps real values, so a remembered tool
                    // call carries no handle the next run must restore.
                    let arguments = match red.as_ref() {
                        Some(r) => r.detokenize(&c.args),
                        None => c.args.clone(),
                    };
                    serde_json::json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": arguments },
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
                let mut args: serde_json::Value =
                    serde_json::from_str(&call.args).unwrap_or(serde_json::json!({}));
                if let Some(r) = red.as_ref() {
                    r.detokenize_json(&mut args);
                }
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
                // If the command saved a screenshot, attach the image so the
                // model sees it on the next turn.
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

        emit_redacted(&app, &red);
        let _ = app.emit("stream_done", serde_json::json!({}));
        return Ok(match red.as_ref() {
            Some(r) => r.detokenize(&text),
            None => text,
        });
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
        let (summary, start, cut) = plan_history(&prior, 10);
        assert_eq!(summary, None);
        assert_eq!((start, cut), (0, 2));
        assert_eq!(prior[start + cut].role, "user");

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

        let one_turn = vec![row("user", "task"), row("assistant", "done")];
        assert_eq!(plan_history(&one_turn, 1).1, 0);
    }

    #[test]
    fn the_summarizer_is_shown_the_instruction_it_must_carry() {
        let long = "x".repeat(400);
        // The in-run shape: the cut keeps the opening row on the wire.
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

        // The pre-run shape: the cut is a prefix. When it swallows the opening
        // row, the dropped material already carries the instruction.
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
        // A difference between `brief_rules()` and `prompts/COMPACT.md` means
        // Rust carries the rules again.
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
        assert!(t.contains("a screenshot was captured") && !t.contains("AAAA"));
    }

    #[test]
    fn resumed_skills_carry_their_instructions_into_the_prompt() {
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
        // Unknown skills inject no empty section, and nothing loaded is a no-op.
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
