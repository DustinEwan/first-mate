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

/// Execute a tool by name with JSON arguments. Returns the result as a string.
async fn execute_tool(name: &str, args: &serde_json::Value) -> Result<String, String> {
    match name {
        "run_command" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let output = tokio::process::Command::new("cmd.exe")
                .arg("/C")
                .arg(command)
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
/// Send a message to the LLM and return the response.
#[tauri::command]
async fn chat_with_llm(
    app: tauri::AppHandle,
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    message: String,
    system_prompt: String,
) -> Result<String, String> {
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("base URL is empty".into());
    }
    if model.is_empty() {
        return Err("no model selected".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    match provider.as_str() {
        "ollama" => {
            let body = serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": message}
                ],
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
                .unwrap_or("")
                .to_string();
            Ok(reply)
        }
        "openai" | "custom" => {
            let completions = if base.ends_with("/v1") {
                format!("{base}/chat/completions")
            } else {
                format!("{base}/v1/chat/completions")
            };
            // Build the conversation history.
            let mut messages: Vec<serde_json::Value> = vec![
                serde_json::json!({"role": "system", "content": system_prompt}),
                serde_json::json!({"role": "user", "content": message}),
            ];
            let tools = agent_tools();
            // Tool-calling loop: send to LLM, execute any tool calls, feed results back.
            for _ in 0..20 {
                let body = serde_json::json!({
                    "model": model,
                    "messages": messages,
                    "tools": tools,
                });
                let mut req = client.post(&completions).json(&body);
                if !api_key.is_empty() {
                    req = req.bearer_auth(api_key.clone());
                }
                let resp = req.send().await.map_err(|e| e.to_string())?;
                let status = resp.status();
                let text = resp.text().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!("HTTP {status}: {text}"));
                }
                let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                let choice = v.get("choices").and_then(|c| c.get(0));
                let choice = match choice {
                    Some(c) => c,
                    None => return Err("no choices in response".into()),
                };
                // Check for tool calls.
                let tool_calls = choice
                    .get("message")
                    .and_then(|m| m.get("tool_calls"))
                    .and_then(|t| t.as_array());
                if let Some(calls) = tool_calls {
                    // Add the assistant's tool-call message to history.
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or(""),
                        "tool_calls": calls,
                    }));
                    // Execute each tool call and add the results.
                    for call in calls {
                        let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        let func = call.get("function").unwrap_or(&serde_json::Value::Null);
                        let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let args_str = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                        let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or(serde_json::json!({}));
                        // Emit a tool_call event so the frontend can show what the agent is doing.
                        let _ = app.emit(
                            "tool_call",
                            serde_json::json!({
                                "name": name,
                                "args": args,
                            }),
                        );
                        let result = execute_tool(name, &args).await.unwrap_or_else(|e| format!("Error: {e}"));
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": result,
                        }));
                    }
                    // Loop back to send the updated history to the LLM.
                    continue;
                }
                // No tool calls — return the text response.
                let reply = choice
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok(reply);
            }
            Err("max tool iterations reached".into())
        }
        "anthropic" => {
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 1024,
                "system": system_prompt,
                "messages": [{"role": "user", "content": message}],
            });
            let mut req = client
                .post(format!("{base}/v1/messages"))
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body);
            let _ = &mut req;
            let resp = req.send().await.map_err(|e| e.to_string())?;
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
                .unwrap_or("")
                .to_string();
            Ok(reply)
        }
        _ => Err("no chat endpoint for this provider".into()),
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
