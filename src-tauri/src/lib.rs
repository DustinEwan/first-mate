use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
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

/// Execute a tool by name with JSON arguments. Returns the result as a string.
async fn execute_tool(name: &str, args: &serde_json::Value) -> Result<String, String> {
    log(&format!("TOOL CALL: {} args={}", name, args));
    let result = execute_tool_inner(name, args).await;
    let summary = match &result {
        Ok(s) => format!("OK len={}", s.len()),
        Err(e) => format!("ERR {}", e.chars().take(200).collect::<String>()),
    };
    log(&format!("TOOL RESULT: {} -> {}", name, summary));
    result
}

async fn execute_tool_inner(name: &str, args: &serde_json::Value) -> Result<String, String> {
    match name {
        "run_command" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let argv = parse_command(command);
            let (program, rest) = match argv.split_first() {
                Some((p, r)) => (p, r),
                None => return Err("empty command".into()),
            };
            let output = tokio::process::Command::new(program)
                .args(rest)
                .output()
                .await
                .map_err(|e| e.to_string())?;
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
                let result = execute_tool(&name, &args)
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

/// Execute a command on the Windows system and return the output.
#[tauri::command]
async fn run_command(command: String) -> Result<String, String> {
    let output = tokio::process::Command::new("cmd.exe")
        .arg("/C")
        .arg(&command)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!("exit {:?}: {}\n{}", output.status, stdout, stderr));
    }
    Ok(stdout)
}

/// Read a file from the Windows filesystem.
#[tauri::command]
async fn read_file(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| e.to_string())
}

/// Write a file to the Windows filesystem.
#[tauri::command]
async fn write_file(path: String, content: String) -> Result<(), String> {
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| e.to_string())
}

/// List directory contents.
#[tauri::command]
async fn list_dir(path: String) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(&path)
        .await
        .map_err(|e| e.to_string())?;
    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let is_dir = tokio::fs::metadata(&path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        entries.push(if is_dir { format!("{name}/") } else { name });
    }
    Ok(entries)
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
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, list_models, test_llm, chat_with_llm, run_command, read_file, write_file, list_dir])
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
