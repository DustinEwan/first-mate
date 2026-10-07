use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Summon hotkey. Change here; it's registered once in `run`.
const HOTKEY: &str = "Super+Alt+Space";

/// Expose the summon hotkey to the frontend so the UI always shows the real binding.
#[tauri::command]
fn get_hotkey() -> &'static str {
    HOTKEY
}

/// LLM configuration, persisted as part of the app settings.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
struct LlmSettings {
    provider: String,
    model: String,
    api_key: String,
    base_url: String,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
struct Settings {
    llm: LlmSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self { llm: LlmSettings::default() }
    }
}

fn settings_file(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    let dir = app.path().app_config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> tauri::Result<Settings> {
    let path = settings_file(&app)?;
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents)?)
    } else {
        Ok(Settings::default())
    }
}

#[tauri::command]
fn save_settings(app: tauri::AppHandle, settings: Settings) -> tauri::Result<()> {
    let path = settings_file(&app)?;
    let contents = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Discover the models a provider exposes, using its model-listing endpoint.
/// Ollama: GET {base}/api/tags; OpenAI-compatible: GET {base}/v1/models.
#[tauri::command]
async fn list_models(provider: String, base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::new();
    let models: Vec<String> = match provider.as_str() {
        "ollama" => {
            let resp = client
                .get(format!("{base}/api/tags"))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let body: serde_json::Value = resp
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
        "openai" | "custom" => {
            let models_url = if base.ends_with("/v1") {
                format!("{base}/models")
            } else {
                format!("{base}/v1/models")
            };
            let mut req = client.get(&models_url);
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let body: serde_json::Value = resp
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
        // Anthropic and others: no public model-listing endpoint.
        _ => Vec::new(),
    };
    Ok(models)
}

/// Make a minimal chat completion to verify the LLM config actually works.
#[tauri::command]
async fn test_llm(provider: String, base_url: String, api_key: String, model: String) -> Result<String, String> {
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
    match provider.as_str() {
        "ollama" => {
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
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("HTTP {status}: {text}"));
            }
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let reply = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            Ok(format!("OK — {model} replied: {reply}"))
        }
        "openai" | "custom" => {
            let body = serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
            });
            let completions = if base.ends_with("/v1") {
                format!("{base}/chat/completions")
            } else {
                format!("{base}/v1/chat/completions")
            };
            let mut req = client.post(&completions).json(&body);
            if !api_key.is_empty() {
                req = req.bearer_auth(api_key);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("HTTP {status}: {text}"));
            }
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let reply = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            Ok(format!("OK — {model} replied: {reply}"))
        }
        "anthropic" => {
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "ping"}],
            });
            let resp = client
                .post(format!("{base}/v1/messages"))
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("HTTP {status}: {text}"));
            }
            let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let reply = v
                .get("content")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            Ok(format!("OK — {model} replied: {reply}"))
        }
        _ => Err("no test endpoint for this provider".into()),
    }
}


/// The tools the agent can call, in OpenAI function-calling format.
fn agent_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Execute a command on the Windows system and return the output. Use this to run winapp commands and other shell commands.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command to execute (e.g. 'winapp ui inspect -a notepad')"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file from the Windows filesystem and return its contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The file path to read"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write content to a file on the Windows filesystem.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The file path to write"},
                        "content": {"type": "string", "description": "The content to write"}
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List the contents of a directory. Directory entries end with '/'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The directory path to list"}
                    },
                    "required": ["path"]
                }
            }
        }
    ])
}

/// Append a timestamped line to the local log file.
fn log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("firstmate.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(f, "[{}.{:03}] {}", ts.as_secs(), ts.subsec_millis(), msg);
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

/// Split a command line into arguments, respecting double quotes.
/// `winapp ui search "Qwen 3.8 Flash Next" -a zen`
///   -> ["winapp", "ui", "search", "Qwen 3.8 Flash Next", "-a", "zen"]
fn parse_command(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            ' ' | '\t' if !in_quotes => {
                if started {
                    args.push(current.clone());
                    current.clear();
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// True when the command uses shell features (pipes, `&&`, redirection,
/// cmd built-ins) that require running through `cmd.exe` instead of
/// launching the program directly.
fn needs_shell(cmd: &str) -> bool {
    if cmd.contains(['|', '&', '>', '<', '^', '%', '(', ')']) {
        return true;
    }
    matches!(
        cmd.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str(),
        "if" | "for" | "echo" | "set" | "dir" | "copy" | "move" | "del" | "erase"
            | "type" | "cd" | "chdir" | "mkdir" | "md" | "rmdir" | "rd" | "ren"
            | "rename" | "start" | "call" | "pause" | "timeout" | "exit" | "shift"
            | "setlocal" | "endlocal" | "pushd" | "popd" | "path" | "rem" | "goto"
    )
}

/// LCS-based line diff: lines dropped from `prev` are `-`, added lines `+`.
fn line_diff(prev: &str, now: &str) -> String {
    let a: Vec<&str> = prev.lines().collect();
    let b: Vec<&str> = now.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push_str("- ");
            out.push_str(a[i]);
            out.push('\n');
            i += 1;
        } else {
            out.push_str("+ ");
            out.push_str(b[j]);
            out.push('\n');
            j += 1;
        }
    }
    while i < n {
        out.push_str("- ");
        out.push_str(a[i]);
        out.push('\n');
        i += 1;
    }
    while j < m {
        out.push_str("+ ");
        out.push_str(b[j]);
        out.push('\n');
        j += 1;
    }
    out
}

/// Truncate output over `max` chars with a hint to narrow the command.
fn truncate_if_over(s: String, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        let rest = s.chars().count() - max;
        log(&format!("BUDGET: truncated {} chars", rest));
        format!(
            "{}\n...[truncated {} chars — narrow the command: --depth N, a selector, or pipe to findstr]",
            head, rest
        )
    } else {
        s
    }
}

/// Keep tool output from exploding the context. An identical repeat result
/// collapses to a pointer; a changed repeat returns only the line diff
/// against the previous output; oversized output is truncated. `seen` maps
/// tool call -> full result, per chat run.
fn budget_output(seen: &mut HashMap<String, String>, key: String, result: String) -> String {
    const MAX: usize = 8000;
    if let Some(prev) = seen.get(&key) {
        if *prev == result {
            log(&format!("BUDGET: deduped identical result ({} bytes)", result.len()));
            seen.insert(key, result.clone());
            return format!(
                "[identical to your earlier call: {} bytes, unchanged — the state has not changed]",
                result.len()
            );
        }
        let diff = line_diff(prev, &result);
        let out = if diff.trim().is_empty() {
            "[no visible line changes vs your earlier call]".to_string()
        } else if diff.chars().count() >= result.chars().count() {
            result.clone() // wholesale change: a diff would save nothing
        } else {
            log(&format!(
                "BUDGET: returned line diff instead of full {}-byte result",
                result.len()
            ));
            format!("[changes vs your earlier call]\n{}", diff)
        };
        seen.insert(key, result);
        return truncate_if_over(out, MAX);
    }
    seen.insert(key, result.clone());
    truncate_if_over(result, MAX)
}

/// Execute a tool by name with JSON arguments. Returns the result as a string.
async fn execute_tool(
    name: &str,
    args: &serde_json::Value,
    seen: &mut HashMap<String, String>,
) -> Result<String, String> {
    log(&format!("TOOL CALL: {} args={}", name, args));
    let result = execute_tool_inner(name, args).await;
    let summary = match &result {
        Ok(s) => format!("OK len={}", s.len()),
        Err(e) => format!("ERR {}", e.chars().take(200).collect::<String>()),
    };
    log(&format!("TOOL RESULT: {} -> {}", name, summary));
    result.map(|r| budget_output(seen, format!("{}{}", name, args), r))
}

async fn execute_tool_inner(name: &str, args: &serde_json::Value) -> Result<String, String> {
    match name {
        "run_command" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let output = if needs_shell(command) {
                // Pipes, &&, redirection, cmd built-ins: run via cmd.exe.
                // /S /C "..." strips only the outermost quotes, so inner
                // quotes survive (plain /C mangles them). raw_arg avoids
                // Rust re-escaping the string.
                log(&format!("RUN via cmd shell: {}", command));
                tokio::process::Command::new("cmd.exe")
                    .raw_arg("/S")
                    .raw_arg("/C")
                    .raw_arg(format!("\"{}\"", command))
                    .output()
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                // Direct launch: quotes are parsed by us, so multi-word
                // quoted arguments reach the program intact.
                let argv = parse_command(command);
                let (program, rest) = match argv.split_first() {
                    Some((p, r)) => (p, r),
                    None => return Err("empty command".into()),
                };
                log(&format!("RUN direct: {} {:?}", program, rest));
                tokio::process::Command::new(program)
                    .args(rest)
                    .output()
                    .await
                    .map_err(|e| e.to_string())?
            };
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !output.status.success() {
                Err(format!("exit {:?}: {}\n{}", output.status, stdout, stderr))
            } else {
                Ok(stdout)
            }
        }
        "read_file" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            tokio::fs::read_to_string(path).await.map_err(|e| e.to_string())
        }
        "write_file" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            tokio::fs::write(path, content).await.map_err(|e| e.to_string())?;
            Ok("OK".to_string())
        }
        "list_dir" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let mut entries = Vec::new();
            let mut dir = tokio::fs::read_dir(path).await.map_err(|e| e.to_string())?;
            while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
                let name = entry.file_name().to_string_lossy().to_string();
                let p = entry.path();
                let is_dir = tokio::fs::metadata(&p).await.map(|m| m.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{name}/") } else { name });
            }
            Ok(entries.join("\n"))
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

/// A prior conversation turn sent by the frontend so the agent has memory
/// across chat_with_llm invocations.
#[derive(serde::Deserialize)]
struct HistoryItem {
    role: String,
    content: String,
}

/// Send a message to the LLM and return the response.
/// OpenAI-compatible endpoints (custom/openai/ollama) use the native client,
/// which supports image parts so screenshots are actually visible to the
/// model. lmkit handles Anthropic only (text-only chat).
#[tauri::command]
async fn chat_with_llm(
    app: tauri::AppHandle,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
) -> Result<String, String> {
    if provider == "anthropic" {
        chat_via_lmkit(app, provider, base_url, api_key, model, message, prior_history, system_prompt).await
    } else {
        chat_via_openai(app, provider, base_url, api_key, model, message, prior_history, system_prompt).await
    }
}

/// lmkit-backed chat (text-only; used for Anthropic).
async fn chat_via_lmkit(
    app: tauri::AppHandle,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
) -> Result<String, String> {
    use futures_util::StreamExt;
    use lmkit::{
        create_chat_provider, merge_tool_call_deltas, ChatEvent, ChatMessage,
        ChatRequest, FunctionDefinition, Provider, ProviderConfig, Role, ToolCallDelta,
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

    // Convert the tools JSON to lmkit types.
    let tools: Vec<ToolDefinition> = agent_tools()
        .as_array()
        .expect("agent_tools returns an array")
        .iter()
        .map(|t| {
            let func = t.get("function").expect("tool has a function");
            let name = func
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = func
                .get("parameters")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let function = match func.get("description").and_then(|d| d.as_str()) {
                Some(desc) => FunctionDefinition::with_description(name, desc, parameters),
                None => FunctionDefinition::new(name, parameters),
            };
            ToolDefinition { function }
        })
        .collect();

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
    loop {
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
                let name = call.function.name.clone();
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                let _ = app.emit("tool_call", serde_json::json!({ "name": &name, "args": &args }));
                let result = execute_tool(&name, &args, &mut seen)
                    .await
                    .unwrap_or_else(|e| format!("Error: {e}"));
                history.push(ChatMessage::tool(call.id.clone(), result));
            }
            log(&format!(
                "TURN {} TOOL TURN: {} calls, history_msgs={}",
                turn, calls.len(), history.len()
            ));
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
async fn chat_via_openai(
    app: tauri::AppHandle,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    prior_history: Vec<HistoryItem>,
    system_prompt: String,
) -> Result<String, String> {
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

    let tools = agent_tools();
    let mut seen: HashMap<String, String> = HashMap::new();
    let client = reqwest::Client::new();
    let mut turn = 0;
    loop {
        let body = serde_json::json!({
            "model": model,
            "messages": msgs,
            "tools": tools,
            "stream": true,
        });
        let body_json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        log(&format!(
            "TURN {} REQUEST: len={} head={:?}",
            turn,
            body_json.len(),
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
                let name = call.name.clone();
                let args: serde_json::Value =
                    serde_json::from_str(&call.args).unwrap_or(serde_json::json!({}));
                let _ = app.emit("tool_call", serde_json::json!({ "name": &name, "args": &args }));
                let result = execute_tool(&name, &args, &mut seen)
                    .await
                    .unwrap_or_else(|e| format!("Error: {e}"));
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

        let _ = app.emit("stream_done", serde_json::json!({}));
        return Ok(text);
    }
}

fn toggle_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    match window.is_visible() {
        Ok(true) => {
            let _ = window.hide();
        }
        _ => {
            // Dock to the top edge of the monitor, horizontally centered.
            if let Ok(Some(monitor)) = window.current_monitor() {
                let win_size = window.outer_size().unwrap_or_else(|_| monitor.size().clone());
                let x = monitor.position().x + ((monitor.size().width as i64 - win_size.width as i64) / 2) as i32;
                let _ = window.set_position(PhysicalPosition::new(x, monitor.position().y));
            }
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

/// Open (or focus) the settings window. Created on first use.
fn open_settings(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    match tauri::WebviewWindow::builder(
        app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("First Mate — Settings")
    .inner_size(480.0, 600.0)
    .decorations(false)
    .shadow(true)
    .build()
    {
        Ok(window) => {
            let _ = window.set_focus();
        }
        Err(e) => eprintln!("failed to open settings window: {e}"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, list_models, test_llm, chat_with_llm])
        .setup(|app| {
            // Tray: left-click toggles the console; menu for explicit actions.
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Show / Hide", true, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle_item, &settings_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("First Mate")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => toggle_window(app),
                    "settings" => open_settings(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event({
                    // A single tray click can emit more than one event; debounce so a
                    // double event doesn't show-then-hide (flash) the window.
                    let last_toggle = Arc::new(AtomicU64::new(0));
                    move |tray, event| {
                        if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            let last = last_toggle.load(Ordering::Relaxed);
                            if now.saturating_sub(last) < 300 {
                                return;
                            }
                            last_toggle.store(now, Ordering::Relaxed);
                            toggle_window(tray.app_handle());
                        }
                    }
                })
                .build(app)?;

            // Global hotkey toggles the console from anywhere.
            app.global_shortcut()
                .on_shortcut(HOTKEY, |app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_window(app);
                    }
                })?;

            // Closing the window hides it; the app lives in the tray.
            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running First Mate");
}

#[cfg(test)]
mod tests {
    use super::{
        budget_output, extract_screenshot_path, merge_tool_call_delta, needs_shell,
        parse_command, CallAcc,
    };
    use std::collections::HashMap;

    #[test]
    fn parse_keeps_quoted_phrase_together() {
        assert_eq!(
            parse_command(r#"winapp ui search "Qwen 3.8 Flash Next" -a zen"#),
            vec!["winapp", "ui", "search", "Qwen 3.8 Flash Next", "-a", "zen"]
        );
    }

    #[test]
    fn parse_plain_command() {
        assert_eq!(
            parse_command("winapp ui inspect -a zen"),
            vec!["winapp", "ui", "inspect", "-a", "zen"]
        );
    }

    #[test]
    fn shell_features_route_to_cmd() {
        assert!(needs_shell(r#"tasklist | findstr /i "zen""#));
        assert!(needs_shell("winapp ui click a && winapp ui send-keys ctrl+t"));
        assert!(needs_shell("winapp ui inspect -a zen 2>&1"));
        assert!(needs_shell(r#"if exist "C:\x" (dir /b "C:\x")"#));
        assert!(needs_shell("dir /b C:\\"));
    }

    #[test]
    fn plain_commands_run_direct() {
        assert!(!needs_shell("winapp ui inspect -a zen"));
        assert!(!needs_shell(r#"winapp ui search "Qwen 3.8 Flash Next" -a zen"#));
        assert!(!needs_shell("tasklist /FI \"IMAGENAME eq zen.exe\""));
    }

    #[test]
    fn cmd_shell_path_preserves_quoted_argument() {
        use std::os::windows::process::CommandExt;
        // Same construction as the run_command shell path.
        let cmd = r#"winapp ui search "Qwen 3.8 Flash Next" -a nosuchapp"#;
        let out = std::process::Command::new("cmd.exe")
            .raw_arg("/S")
            .raw_arg("/C")
            .raw_arg(format!("\"{}\"", cmd))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        // If quotes survive, clap parses one selector and winapp fails on the
        // unknown app name; if cmd mangles them, clap rejects the split
        // tokens ("'3.8' was not matched") before the app lookup happens.
        assert!(
            !text.contains("was not matched"),
            "quotes were mangled by cmd: {text}"
        );
    }

    #[test]
    fn identical_repeat_collapses_to_pointer() {
        let mut seen = HashMap::new();
        let out = "x".repeat(100);
        let first = budget_output(&mut seen, "cmd".into(), out.clone());
        assert_eq!(first, out);
        let second = budget_output(&mut seen, "cmd".into(), out.clone());
        assert!(second.contains("identical to your earlier call"));
        // Changed output after an action still comes through fully.
        let changed = "y".repeat(50);
        let third = budget_output(&mut seen, "cmd".into(), changed.clone());
        assert_eq!(third, changed);
    }

    #[test]
    fn oversized_output_is_truncated_with_hint() {
        let mut seen = HashMap::new();
        let huge = "z".repeat(9000);
        let out = budget_output(&mut seen, "c".into(), huge);
        assert!(out.contains("truncated 1000 chars"));
        assert!(out.contains("narrow the command"));
        assert!(out.chars().count() < 8200);
    }

    #[test]
    fn changed_repeat_returns_line_diff() {
        let mut seen = HashMap::new();
        budget_output(&mut seen, "inspect".into(), "tab A\ntab B\ntab C".into());
        let out = budget_output(&mut seen, "inspect".into(), "tab A\ntab B2\ntab C".into());
        assert!(out.starts_with("[changes vs your earlier call]"));
        assert!(out.contains("- tab B"));
        assert!(out.contains("+ tab B2"));
        // Unchanged lines are omitted entirely.
        assert!(!out.contains("tab A"));
        assert!(!out.contains("tab C"));
    }

    #[test]
    fn wholesale_change_falls_back_to_full_output() {
        let mut seen = HashMap::new();
        budget_output(&mut seen, "k".into(), "completely\ndifferent\ncontent".into());
        let now = "a\nb".to_string();
        let out = budget_output(&mut seen, "k".into(), now.clone());
        // Diff (3 removals + 2 additions) is bigger than the output itself.
        assert_eq!(out, now);
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
