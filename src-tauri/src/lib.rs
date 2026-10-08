use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Summon hotkey. Change here; it's registered once in `run`.
const HOTKEY: &str = "Super+Alt+Space";

/// User interrupt for the running agent loop. The tool loop is uncapped
/// (real work takes many turns), so the stop button is the brake: checked
/// between turns and between tool calls.
static CHAT_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn chat_stop_requested() -> bool {
    CHAT_STOP.load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn stop_chat() {
    if !chat_stop_requested() {
        CHAT_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        log("STOP REQUESTED");
    }
}

/// A text-only turn that is a declared intent rather than a final answer:
/// short and action-declaring ("Let me check X", or ending mid-thought).
/// Small models do this when they mean to call a tool but emit prose
/// instead; the loop nudges once per run so the declared action executes.
fn looks_like_unexecuted_intent(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 400 {
        return false;
    }
    let lower = t.to_lowercase();
    let declares = ["let me", "i'll ", "i will ", "now let", "i'm going to", "let's "]
        .iter()
        .any(|p| lower.starts_with(p) || lower.contains(&format!(". {p}")));
    declares || t.ends_with(':') || t.ends_with('…') || t.ends_with("...")
}

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
                "description": "Execute a command on the Windows system and return the output. shell='powershell' is REQUIRED for any PowerShell containing $vars, quotes, or multiple statements: the command text is written VERBATIM to a scratch script and run with 'powershell -File', so $, ' and \" survive exactly as typed — never route PowerShell through the default cmd form (cmd.exe re-quoting corrupts it). shell='cmd' (default) runs argv directly, or via cmd.exe when the text contains pipes/&&/redirection. Foreground commands are force-killed after 5 minutes; interactive or long-running programs MUST use background:true and are driven via get_command_output / kill_command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command to execute (e.g. 'winapp ui inspect -a notepad', or PowerShell statements when shell='powershell')"},
                        "shell": {"type": "string", "enum": ["cmd", "powershell"], "description": "Interpreter. 'powershell' passes command text as script data (zero re-quoting). Default 'cmd'."},
                        "cwd": {"type": "string", "description": "Absolute directory to run the command in (optional)."},
                        "background": {"type": "boolean", "description": "Start detached and return a pid immediately instead of waiting for completion"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_command_output",
                "description": "Get status and accumulated output of a background command started with run_command background:true. Status is 'running' or 'exited: <code>'. Safe to poll repeatedly.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pid": {"type": "integer", "description": "The pid returned by run_command background:true"}
                    },
                    "required": ["pid"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "kill_command",
                "description": "Force-kill a background command and its whole process tree (taskkill /F /T).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pid": {"type": "integer", "description": "The pid to kill"}
                    },
                    "required": ["pid"]
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
        },
        {
            "type": "function",
            "function": {
                "name": "search_files",
                "description": "Recursively find files/directories whose name contains a case-insensitive substring. Returns up to 200 paths. Pick a narrow root; a walk of C:\\ is slow.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Case-insensitive substring of the file name"},
                        "root": {"type": "string", "description": "Directory to search under (default 'C:\\')"}
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_processes",
                "description": "List running processes as CSV: name, PID, session, memory. To filter, use run_command with 'tasklist | findstr /i <name>' instead.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full instructions of one of the advertised skills. Call this first whenever a task matches a skill's domain.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Skill name, exactly as advertised in the system prompt"}
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_skill_resource",
                "description": "Read a reference file that belongs to a skill, e.g. 'my-skill/references/api.md'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "'<skill>/<relative/path>' inside the skill directory"}
                    },
                    "required": ["path"]
                }
            }
        }
    ])
}

/// The only tool schemas injected into context before any skill is loaded:
/// command execution plus the skill machinery itself. Everything else is
/// implemented here in Rust but stays OUT of the model's context until a
/// skill's frontmatter `tools:` list enables it (see `enable_skill_tools`).
const CORE_TOOLS: &[&str] = &[
    "run_command",
    "get_command_output",
    "kill_command",
    "load_skill",
    "read_skill_resource",
];

fn tool_schema(name: &str) -> Option<serde_json::Value> {
    agent_tools()
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some(name))
                .cloned()
        })
}

fn core_tools() -> Vec<serde_json::Value> {
    CORE_TOOLS.iter().filter_map(|n| tool_schema(n)).collect()
}

/// Append the tool schemas a skill's frontmatter `tools:` enables that are
/// not already in `active`. Returns the names added (empty = pure-behavior
/// skill). Unknown names are skipped: the skill cannot invent tools.
fn enable_skill_tools(active: &mut Vec<serde_json::Value>, skill_name: &str) -> Vec<String> {
    let Some(dir) = skill_dir(skill_name) else {
        return Vec::new();
    };
    let Ok(md) = std::fs::read_to_string(dir.join("SKILL.md")) else {
        return Vec::new();
    };
    let (_, _, tools, _) = parse_frontmatter(&md);
    let mut added = Vec::new();
    for t in tools {
        if active.iter().any(|v| v.pointer("/function/name").and_then(|n| n.as_str()) == Some(t.as_str())) {
            continue;
        }
        if let Some(schema) = tool_schema(&t) {
            active.push(schema);
            added.push(t);
        }
    }
    added
}

/// Convert OpenAI-format tool schemas to lmkit types.
fn to_tool_defs(values: &[serde_json::Value]) -> Vec<lmkit::ToolDefinition> {
    use lmkit::{FunctionDefinition, ToolDefinition};
    values
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
        .collect()
}

/// Fallback behavior text, used only when no AGENTS.md exists. The real
/// system prompt is content, and content belongs to the repo/user — see
/// `get_system_prompt`.
const DEFAULT_SYSTEM_PROMPT: &str = "You are First Mate, a Windows control agent. You control this machine and its applications by running commands and loading skills.\n\n## Rules\n- When you decide to take an action, call the tool in the SAME turn. NEVER end your reply with only a declaration of intent (\"Let me check X\", \"I'll verify Y\") — prose alone executes nothing and the run ends there.\n- Capabilities beyond the bare tools live in skills: when a task matches an advertised skill, load_skill(\"<name>\") FIRST, then follow its instructions.\n\n## Convergence\n- If a command fails, do NOT retry it more than once. Report the error to the user and suggest an alternative.\n- When you have enough information to answer, stop calling tools and give your final text response.";

/// AGENTS.md locations: cwd, parent (src-tauri dev layout), exe dir and its
/// parent, then ~/.firstmate. First hit wins.
fn agents_md_path() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> =
        vec![std::path::PathBuf::from("AGENTS.md"), std::path::PathBuf::from("../AGENTS.md")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("AGENTS.md"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("AGENTS.md"));
            }
        }
    }
    candidates.push(home_dir().join(".firstmate").join("AGENTS.md"));
    candidates.into_iter().find(|p| p.is_file())
}

/// The agent's behavior text: AGENTS.md when present, built-in otherwise.
#[tauri::command]
fn get_system_prompt() -> String {
    match agents_md_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_SYSTEM_PROMPT.to_string(),
    }
}

/// Open a URL or filesystem path in the user's default handler — the ONLY
/// sanctioned way anything leaves the webview (browser for http/https,
/// default app for file://). Rejects anything else (powershell schemes,
/// arbitrary executables-by-scheme, command injection).
#[tauri::command]
fn open_path(target: String) -> Result<(), String> {
    let result = open_path_inner(&target);
    log(&format!(
        "OPEN_PATH {target} -> {}",
        result.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str())
    ));
    result
}

fn open_path_inner(target: &str) -> Result<(), String> {
    shell_open(&resolve_open_target(target)?)
}

/// Scheme gate + normalization: file:// -> existing backslash path, http(s) ->
/// verbatim URL. Everything else refused. No cmd/explorer argv is ever built,
/// so URL metacharacters (& | ^) are ordinary data, not injection vectors.
fn resolve_open_target(target: &str) -> Result<String, String> {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("file://") {
        let path = rest.split('#').next().unwrap_or(rest).split('?').next().unwrap_or(rest);
        let decoded = percent_decode(path.trim_start_matches('/'));
        if decoded.contains('"') {
            return Err("path contains a quote character".to_string());
        }
        // file:///C:/x -> C:\x ; file://server/share -> \\server\share (UNC).
        // Handlers get backslashes: forward slashes break naive argv parsers.
        let is_unc = !t.starts_with("file:///");
        let winpath = decoded.replace('/', "\\");
        let winpath = if is_unc {
            format!("\\\\{winpath}")
        } else {
            winpath
        };
        if !std::path::Path::new(&winpath).exists() {
            return Err(format!("path does not exist: {winpath}"));
        }
        return Ok(winpath);
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Ok(t.to_string());
    }
    Err(format!("refusing to open: {t}"))
}

/// ShellExecuteW — the same call Explorer's own UI uses. Unlike explorer.exe's
/// broken argv parser (which opens Documents on input it can't parse) or
/// `cmd /c start` (quote/metacharacter hell), it takes the path as data.
#[cfg(windows)]
fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let rc = unsafe {
        ShellExecuteW(
            None::<HWND>,
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = rc.0 as isize;
    if code > 32 {
        Ok(())
    } else if code == 31 {
        // SE_ERR_NOASSOC: no handler — Windows showed the Open With dialog.
        Ok(())
    } else {
        Err(format!("ShellExecute failed (code {code})"))
    }
}

#[cfg(not(windows))]
fn shell_open(target: &str) -> Result<(), String> {
    Err(format!("no default handler support on this platform: {target}"))
}

/// Minimal percent-decoding for file:// paths (%20 etc.); leaves '+' alone.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Dispatch gate: a tool may execute only if its schema is disclosed in the
/// current run (core or skill-enabled). Resumed conversations and hallucinated
/// calls must not reach undisclosed implementations.
fn tool_is_disclosed(active: &[serde_json::Value], name: &str) -> bool {
    active
        .iter()
        .any(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some(name))
}

/// Skills advertised to the frontend for system-prompt injection (stage 1
/// of progressive disclosure, docs/SKILLS.md).
#[tauri::command]
fn list_skills() -> Vec<SkillInfo> {
    discover_skills()
}

/// One persisted turn. Only user/assistant turns round-trip.
#[derive(serde::Serialize, serde::Deserialize)]
struct ConvMsg {
    role: String,
    text: String,
}

#[derive(serde::Serialize)]
struct ConvInfo {
    name: String,
    path: String,
    modified: u64,
}

fn conv_dir() -> std::path::PathBuf {
    home_dir().join(".firstmate").join("conversations")
}

/// Filesystem-safe conversation filename stem: no path separators or Windows
/// reserved characters, capped at 48 chars.
fn sanitize_conv_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .take(48)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "chat".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Prelude format: the FIRST LINE is the conversation name, so listing a
/// directory needs no JSON parsing; the rest is JSONL of `ConvMsg`.
fn serialize_conversation(name: &str, msgs: &[ConvMsg]) -> String {
    let mut out = format!("{}\n", name);
    for m in msgs {
        if let Ok(line) = serde_json::to_string(m) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn parse_conversation(raw: &str) -> (String, Vec<ConvMsg>) {
    let mut lines = raw.lines();
    let name = lines.next().unwrap_or("Untitled").trim().to_string();
    let name = if name.is_empty() { "Untitled".to_string() } else { name };
    let msgs = lines.filter_map(|l| serde_json::from_str(l).ok()).collect();
    (name, msgs)
}

/// Resolve a stored path, refusing anything outside the conversations dir.
fn resolve_conv_path(path: &str) -> Result<std::path::PathBuf, String> {
    let full = std::path::Path::new(path)
        .canonicalize()
        .map_err(|_| format!("not found: {path}"))?;
    let root = conv_dir().canonicalize().map_err(|e| e.to_string())?;
    if full.starts_with(&root) && full.is_file() {
        Ok(full)
    } else {
        Err("not a conversation file".into())
    }
}

#[tauri::command]
fn save_conversation(name: String, messages: Vec<ConvMsg>) -> Result<String, String> {
    std::fs::create_dir_all(conv_dir()).map_err(|e| e.to_string())?;
    let path = conv_dir().join(format!("{}.chat", sanitize_conv_name(&name)));
    std::fs::write(&path, serialize_conversation(&name, &messages)).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn list_conversations() -> Vec<ConvInfo> {
    let mut out: Vec<ConvInfo> = Vec::new();
    let Ok(dir) = std::fs::read_dir(conv_dir()) else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("chat") {
            continue;
        }
        // Scan the prelude only: first line is the name.
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = raw.lines().next().unwrap_or("Untitled").trim();
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            })
            .unwrap_or(0);
        out.push(ConvInfo {
            name: if name.is_empty() { "Untitled".into() } else { name.to_string() },
            path: path.to_string_lossy().to_string(),
            modified,
        });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

#[tauri::command]
fn load_conversation(path: String) -> Result<Vec<ConvMsg>, String> {
    let full = resolve_conv_path(&path)?;
    let raw = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
    Ok(parse_conversation(&raw).1)
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

/// Cap tool output over `max` chars: spill the FULL text to
/// `~/.firstmate/output/` and return head + pointer + tail. Nothing is
/// unrecoverably lost — `read_file` the pointer path for any omitted middle.
fn truncate_if_over(s: String, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s;
    }
    let head: String = s.chars().take(max * 3 / 4).collect();
    let tail: String = s.chars().skip(count - max / 4).collect();
    let path = fm_out_dir().join(format!(
        "out-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let omitted = count - head.chars().count() - tail.chars().count();
    let pointer = if std::fs::write(&path, &s).is_ok() {
        format!(
            "\n...[{omitted} of {count} chars omitted; FULL output saved: {} — read_file path=\"{}\" offset=<line> for the middle]...\n",
            path.display(),
            path.display()
        )
    } else {
        format!("\n...[{omitted} chars truncated; spill write failed]...\n")
    };
    log(&format!("BUDGET: spilled {count} chars to {}", path.display()));
    format!("{head}{pointer}{tail}")
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

/// A skill advertised to the model: name + description from SKILL.md
/// frontmatter (Agent Skills spec, see docs/SKILLS.md). Tool availability
/// is not advertised — it is revealed when the skill is loaded.
#[derive(serde::Serialize)]
struct SkillInfo {
    name: String,
    description: String,
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn fm_dir() -> std::path::PathBuf {
    home_dir().join(".firstmate")
}

/// Scratch directory for generated command scripts (created on demand).
fn fm_tmp_dir() -> std::path::PathBuf {
    let d = fm_dir().join("tmp");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Spill directory for full tool output that exceeded the context budget.
fn fm_out_dir() -> std::path::PathBuf {
    let d = fm_dir().join("output");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// LRU hygiene: keep only the newest `keep` files in a scratch directory.
/// Run at startup so a previous session's orphans cannot pile up forever.
fn trim_dir(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), m))
        })
        .filter(|(p, _)| p.is_file())
        .collect();
    files.sort_by_key(|(_, m)| *m);
    let drop_count = files.len().saturating_sub(keep);
    for (path, _) in files.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(path);
    }
}

/// Directories scanned for `<skill>/SKILL.md`: repo `skills/` (cwd, exe dir,
/// and the parent of the exe dir for the src-tauri dev layout) plus the
/// user directory `~/.firstmate/skills`.
fn skill_roots() -> Vec<std::path::PathBuf> {
    let mut candidates =
        vec![std::path::PathBuf::from("skills"), std::path::PathBuf::from("../skills")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("skills"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("skills"));
            }
        }
    }
    candidates.push(home_dir().join(".firstmate").join("skills"));
    let mut roots = Vec::new();
    for c in candidates {
        let c = c.canonicalize().unwrap_or(c);
        if c.is_dir() && !roots.contains(&c) {
            roots.push(c);
        }
    }
    roots
}

/// Parse `---` frontmatter. Returns (name, description, tools, body).
/// `tools:` is a comma-separated list of Rust-implemented tools the skill
/// enables when loaded; skills without it are pure behavior.
fn parse_frontmatter(md: &str) -> (Option<String>, Option<String>, Vec<String>, String) {
    let empty_tools = Vec::new();
    let Some(rest) = md
        .strip_prefix("---")
        .and_then(|r| r.strip_prefix('\n'))
        .or_else(|| md.strip_prefix("---\r\n"))
    else {
        return (None, None, empty_tools, md.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None, empty_tools, md.to_string());
    };
    let mut name = None;
    let mut description = None;
    let mut tools = Vec::new();
    for line in rest[..end].lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("tools:") {
            tools = v
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    (name, description, tools, body)
}

/// All skills found under the roots, first root winning on name clashes.
fn discover_skills() -> Vec<SkillInfo> {
    let mut skills: Vec<SkillInfo> = Vec::new();
    for root in skill_roots() {
        let Ok(dir) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in dir.flatten() {
            let Ok(md) = std::fs::read_to_string(entry.path().join("SKILL.md")) else {
                continue;
            };
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let (name, description, _, _) = parse_frontmatter(&md);
            let name = name.unwrap_or(dir_name);
            if !skills.iter().any(|s| s.name == name) {
                skills.push(SkillInfo {
                    name,
                    description: description.unwrap_or_default(),
                });
            }
        }
    }
    skills
}

/// Directory of the named skill (frontmatter name, falling back to dir name).
fn skill_dir(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    for root in skill_roots() {
        let Ok(dir) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let Ok(md) = std::fs::read_to_string(path.join("SKILL.md")) else {
                continue;
            };
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let (fm_name, ..) = parse_frontmatter(&md);
            if fm_name.as_deref() == Some(name) || dir_name == name {
                return Some(path);
            }
        }
    }
    None
}

/// Resolve `"<skill>/<relative/path>"` to a real file, refusing anything
/// that escapes the skill directory (traversal or symlink tricks).
fn resolve_skill_resource(path: &str) -> Result<std::path::PathBuf, String> {
    if path.contains("..") {
        return Err("path traversal not allowed".into());
    }
    let name = path
        .split(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && path.len() > s.len() + 1)
        .ok_or("expected '<skill>/<relative/path>'")?;
    let dir = skill_dir(name).ok_or_else(|| format!("no such skill: {name}"))?;
    let full = dir
        .join(&path[name.len() + 1..])
        .canonicalize()
        .map_err(|_| format!("not found: {path}"))?;
    let root = dir.canonicalize().map_err(|e| e.to_string())?;
    if !full.starts_with(&root) {
        return Err("path escapes the skill directory".into());
    }
    Ok(full)
}

/// Hard cap for one run_command. Detached grandchildren (e.g. `start /b
/// gh auth login`) inherit the output pipes and can hold them open forever;
/// without a cap the agent loop soft-locks waiting for EOF.
/// Override with FIRSTMATE_CMD_TIMEOUT_SECS.
fn cmd_timeout() -> std::time::Duration {
    let secs = std::env::var("FIRSTMATE_CMD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300);
    std::time::Duration::from_secs(secs)
}

/// Spawn with a deadline. On timeout the direct child dies (kill_on_drop),
/// the process tree is swept, and an actionable error is returned - a tool
/// call must always come back so the agent loop can never wedge.
async fn exec_capped(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    // wait_with_output only captures streams piped at spawn time; the old
    // Command::output() set this implicitly, spawn() does not.
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().unwrap_or(0);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("spawn failed: {e}")),
        Err(_) => {
            if pid > 0 {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
            }
            Err(format!(
                "command timed out after {}s; killed pid {pid} and its tree. \
                 Detached grandchildren may still run (taskkill /F /PID them) \
                 and their partial output is lost.",
                timeout.as_secs()
            ))
        }
    }
}

/// A command the agent started with `background: true`. The app owns the
/// pipes and drains them continuously, so the child can never block on a
/// full pipe or hold the agent loop hostage; the agent drives the lifecycle
/// via get_command_output / kill_command.
struct BgJob {
    command: String,
    stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    status: std::sync::Arc<std::sync::Mutex<String>>,
    started: std::time::Instant,
}

const MAX_BG_OUTPUT: usize = 256 * 1024;
const MAX_BG_JOBS: usize = 32;

static BG_REGISTRY: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u32, BgJob>>,
> = std::sync::LazyLock::new(Default::default);

fn bg_registry() -> &'static std::sync::Mutex<std::collections::HashMap<u32, BgJob>> {
    &BG_REGISTRY
}

/// Drain a pipe into the shared buffer, keeping the tail when it overflows.
fn pump_bg_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut b = buf.lock().unwrap();
                    b.extend_from_slice(&chunk[..n]);
                    if b.len() > MAX_BG_OUTPUT {
                        let cut = b.len() - MAX_BG_OUTPUT;
                        b.drain(..cut);
                    }
                }
            }
        }
    });
}

/// Evict oldest finished jobs first so the registry cannot grow unbounded.
fn evict_bg_jobs(reg: &mut std::collections::HashMap<u32, BgJob>) {
    if reg.len() < MAX_BG_JOBS {
        return;
    }
    let mut by_age: Vec<(u32, bool, std::time::Instant)> = reg
        .iter()
        .map(|(pid, j)| {
            let running = j.status.lock().unwrap().starts_with("running");
            (*pid, running, j.started)
        })
        .collect();
    // Finished and old die first; running jobs survive longest.
    by_age.sort_by_key(|(_, running, started)| (*running, *started));
    for (pid, _, _) in by_age.iter().skip(MAX_BG_JOBS - 1) {
        reg.remove(pid);
    }
}

async fn spawn_background(
    mut cmd: tokio::process::Command,
    command: &str,
) -> Result<String, String> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // The job outlives this function; killing is explicit (kill_command).
        .kill_on_drop(false);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().ok_or("child has no pid")?;
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if let Some(out) = child.stdout.take() {
        pump_bg_output(out, stdout.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump_bg_output(err, stderr.clone());
    }
    let status = std::sync::Arc::new(std::sync::Mutex::new("running".to_string()));
    {
        let status = status.clone();
        tokio::spawn(async move {
            let s = match child.wait().await {
                Ok(s) => format!("exited: {}", s.code().unwrap_or(-1)),
                Err(e) => format!("wait error: {e}"),
            };
            *status.lock().unwrap() = s;
        });
    }
    {
        let mut reg = bg_registry().lock().unwrap();
        evict_bg_jobs(&mut reg);
        reg.insert(
            pid,
            BgJob {
                command: command.to_string(),
                stdout,
                stderr,
                status,
                started: std::time::Instant::now(),
            },
        );
    }
    Ok(format!(
        "started background pid {pid}. Poll with get_command_output(pid), stop with kill_command(pid)."
    ))
}

// ---------------------------------------------------------------------------
// Persistent PowerShell session
// ---------------------------------------------------------------------------
//
// One long-lived `powershell.exe` owned by a dedicated worker thread. Each
// command is script TEXT written to the child's stdin (data, never argv)
// followed by a unique run-marker line; a REPL inside the child executes the
// accumulated text as one scriptblock and answers with a sentinel line
// carrying the exit status. `$vars`, functions, and cwd persist across
// tool calls, and there is zero quoting nesting: the command never passes
// through another interpreter's argument parser.
//
// Failure semantics:
// - Broken: the command demonstrably never ran (spawn/write failure) ->
//   caller may safely fall back to one-shot `powershell -File`.
// - TimedOut / died mid-command: the command may have partially run ->
//   the session is killed, partial output is returned, and NOTHING is
//   re-run (no duplicated side effects).

/// The REPL script that owns the session child's stdin loop. It reads
/// lines until it sees a `__FM_RUN_*` marker, dot-sources everything
/// collected (so assignments land in session scope), then answers
/// `__FM_END_<id>:<ok as int>:<$LASTEXITCODE>`. Success is tracked with an
/// explicit flag because `$?` resets to True once a try/catch completes —
/// a caught `throw` must still report failure — and it also folds in the
/// final `$?` so non-terminating cmdlet errors (missing paths etc.) fail.
const PS_REPL: &str = r#"[Console]::OutputEncoding=[Text.Encoding]::UTF8;[Console]::InputEncoding=[Text.Encoding]::UTF8;while($true){$c=[Console]::In.ReadLine();if($null -eq $c){break};$b=New-Object System.Collections.ArrayList;while($c -notlike '__FM_RUN_*'){[void]$b.Add($c);$c=[Console]::In.ReadLine();if($null -eq $c){break}};if($null -eq $c){break};$id=$c.Substring(9);$ok=$true;try{. ([scriptblock]::Create(($b -join [Environment]::NewLine)))}catch{Write-Error $_;$ok=$false};$ok=$ok -and $?;Write-Host "__FM_END_$($id):$([int]$ok):$LASTEXITCODE"}"#;

struct PsCommand {
    text: String,
    cwd: Option<String>,
    timeout: std::time::Duration,
    reply: std::sync::mpsc::Sender<PsReply>,
}

enum PsMsg {
    Cmd(PsCommand),
    /// Test-only: kill the child and stop the worker.
    Shutdown(std::sync::mpsc::Sender<()>),
}

enum PsReply {
    /// Command completed: stdout, exit code, stderr (stderr is buffered
    /// separately, so cross-stream interleaving is not preserved).
    Done { stdout: String, code: i32, stderr: String },
    /// Command never ran; one-shot fallback is safe.
    Broken { note: String },
    /// Command may have partially run; partial output only, never re-run.
    TimedOut { partial: String, note: String },
}

#[derive(Debug)]
enum PsRun {
    Done { stdout: String, code: i32, stderr: String },
    Broken { note: String },
    TimedOut { partial: String, note: String },
}

struct PsState {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    errbuf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    nonce: u64,
}

static PS_QUEUE: std::sync::LazyLock<
    std::sync::Mutex<Option<std::sync::mpsc::Sender<PsMsg>>>,
> = std::sync::LazyLock::new(Default::default);

fn ps_worker_tx() -> std::sync::mpsc::Sender<PsMsg> {
    let mut guard = PS_QUEUE.lock().unwrap();
    if let Some(tx) = guard.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = std::sync::mpsc::channel::<PsMsg>();
    std::thread::spawn(move || ps_worker(rx));
    *guard = Some(tx.clone());
    tx
}

fn ps_spawn() -> Result<PsState, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(1)
        % 1_000_000_000;
    let mut child = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", PS_REPL])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("powershell spawn failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("session stdin missing")?;
    let out = child.stdout.take().ok_or("session stdout missing")?;
    let err = child.stderr.take().ok_or("session stderr missing")?;
    let (ltx, lrx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(out);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                        line.pop();
                    }
                    if ltx
                        .send(String::from_utf8_lossy(&line).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let errbuf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let eb = errbuf.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut err = err;
            let mut chunk = [0u8; 8192];
            loop {
                match err.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut b = eb.lock().unwrap();
                        if b.len() < MAX_BG_OUTPUT {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        });
    }
    Ok(PsState {
        child,
        stdin,
        lines: lrx,
        errbuf,
        nonce,
    })
}

fn ps_worker(rx: std::sync::mpsc::Receiver<PsMsg>) {
    let mut state: Option<PsState> = None;
    let mut seq: u64 = 0;
    for msg in rx {
        match msg {
            PsMsg::Shutdown(ack) => {
                if let Some(mut st) = state.take() {
                    let _ = st.child.kill();
                }
                let _ = ack.send(());
                return;
            }
            PsMsg::Cmd(cmd) => {
                seq += 1;
                ps_handle(cmd, seq, &mut state);
            }
        }
    }
}

fn ps_handle(cmd: PsCommand, seq: u64, state: &mut Option<PsState>) {
    use std::io::Write;
    if state.is_none() {
        match ps_spawn() {
            Ok(st) => *state = Some(st),
            Err(note) => {
                let _ = cmd.reply.send(PsReply::Broken { note });
                return;
            }
        }
    }
    let st = state.as_mut().unwrap();
    let id = format!("{}_{seq}", st.nonce);
    let mut script = String::new();
    if let Some(dir) = &cmd.cwd {
        // Session cwd: persists into later commands (a feature, matching
        // how a human uses one shell for a task).
        script.push_str(&format!("Set-Location -LiteralPath '{}'\n", dir.replace('\'', "''")));
    }
    script.push_str(&cmd.text);
    if !script.ends_with('\n') {
        script.push('\n');
    }
    // No trailing underscores: the REPL derives the id via Substring(9),
    // and the echoed `__FM_END_<id>:` must match Rust's sentinel exactly.
    script.push_str(&format!("__FM_RUN_{id}\n"));
    if st.stdin.write_all(script.as_bytes()).and_then(|_| st.stdin.flush()).is_err() {
        let _ = st.child.kill();
        *state = None;
        let _ = cmd.reply.send(PsReply::Broken {
            note: "session stdin closed; child killed".into(),
        });
        return;
    }
    let sentinel = format!("__FM_END_{id}:");
    let deadline = std::time::Instant::now() + cmd.timeout;
    let mut out = String::new();
    let mut disconnected = false;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match st.lines.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix(&sentinel) {
                    let mut it = rest.split(':');
                    let ok = it
                        .next()
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(1);
                    let lastexit = it.next().and_then(|s| s.trim().parse::<i32>().ok());
                    let code = if ok == 0 { 1 } else { lastexit.unwrap_or(0) };
                    let stderr =
                        std::mem::take(&mut *st.errbuf.lock().unwrap());
                    let _ = cmd.reply.send(PsReply::Done {
                        stdout: out,
                        code,
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                    return;
                }
                out.push_str(&line);
                out.push('\n');
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                disconnected = true; // child died mid-command
                break;
            }
            Err(_) => break, // deadline
        }
    }
    // No sentinel: the command timed out, or the child exited mid-command.
    // If the child EXITED, its process exit code is the command's own
    // result: a command calling `exit N` (or a bare `exit`) tears down the
    // REPL host and Windows reports N as the process code. A real crash
    // honestly surfaces as a large nonzero code with partial output.
    let mut status = st.child.try_wait().ok().flatten();
    if status.is_none() && disconnected {
        // stdout EOF can land microseconds before the process-exit record;
        // give the OS a moment so `exit N` reports N instead of a false
        // timeout (the common case; a hung-but-closed pipe is not a thing).
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            status = st.child.try_wait().ok().flatten();
            if status.is_some() {
                break;
            }
        }
    }
    let _ = st.child.kill();
    let stderr = std::mem::take(&mut *st.errbuf.lock().unwrap());
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    *state = None; // next command respawns the session transparently
    match status.and_then(|s| s.code()) {
        Some(code) => {
            let _ = cmd.reply.send(PsReply::Done {
                stdout: out,
                code,
                stderr,
            });
        }
        None => {
            let _ = cmd.reply.send(PsReply::TimedOut {
                partial: out,
                note: if status.is_some() {
                    "session child exited mid-command without an exit code".into()
                } else {
                    format!(
                        "session command timed out after {}s",
                        cmd.timeout.as_secs()
                    )
                },
            });
        }
    }
}

async fn ps_session_exec(
    text: &str,
    cwd: Option<String>,
    timeout: std::time::Duration,
) -> PsRun {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if ps_worker_tx()
        .send(PsMsg::Cmd(PsCommand {
            text: text.to_string(),
            cwd,
            timeout,
            reply: reply_tx,
        }))
        .is_err()
    {
        return PsRun::Broken { note: "session worker thread gone".into() };
    }
    // The worker enforces each command's own deadline, but a command may
    // additionally queue behind one wedged predecessor (killed at its own
    // cap). The margin covers the worst case: full foreground cap + slack.
    let margin = timeout + cmd_timeout() + std::time::Duration::from_secs(30);
    let joined = tokio::task::spawn_blocking(move || reply_rx.recv_timeout(margin)).await;
    match joined {
        Ok(Ok(PsReply::Done { stdout, code, stderr })) => {
            PsRun::Done { stdout, code, stderr }
        }
        Ok(Ok(PsReply::Broken { note })) => PsRun::Broken { note },
        Ok(Ok(PsReply::TimedOut { partial, note })) => PsRun::TimedOut { partial, note },
        // Reply never arrived: the command may or may not have run, so
        // this is TimedOut (never re-run), NOT Broken (safe fallback).
        _ => PsRun::TimedOut {
            partial: String::new(),
            note: "session reply lost (worker wedged or died)".into(),
        },
    }
}

#[allow(dead_code)] // called from tests; production keeps the session for app lifetime
/// Test-only: tear the session down so a test cannot leak a powershell.exe.
async fn ps_session_shutdown() {
    let taken = PS_QUEUE.lock().unwrap().take();
    if let Some(tx) = taken {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        let _ = tx.send(PsMsg::Shutdown(ack_tx));
        let _ = tokio::task::spawn_blocking(move || {
            ack_rx.recv_timeout(std::time::Duration::from_secs(5))
        })
        .await;
    }
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
            let background = args
                .get("background")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            let shell = args.get("shell").and_then(|s| s.as_str()).unwrap_or("cmd");
            let cwd = args.get("cwd").and_then(|c| c.as_str());
            if let Some(cwd) = cwd {
                if !std::path::Path::new(cwd).is_dir() {
                    return Err(format!("cwd is not a directory: {cwd}"));
                }
            }
            if shell == "powershell" && !background {
                // Preferred path: the persistent session. $vars, functions,
                // and cwd carry across tool calls, and every call skips a
                // ~0.5s powershell startup.
                match ps_session_exec(command, cwd.map(str::to_string), cmd_timeout()).await {
                    PsRun::Done { stdout, code, stderr } => {
                        return if code == 0 {
                            // Non-terminating errors exit 0 with the problem
                            // only on stderr — surface it, never swallow it.
                            if stderr.trim().is_empty() {
                                Ok(stdout)
                            } else {
                                Ok(format!("{}\n--- stderr ---\n{}", stdout, stderr))
                            }
                        } else {
                            Err(format!("exit code {code}: {}\n{}", stdout, stderr))
                        };
                    }
                    PsRun::TimedOut { partial, note } => {
                        return Err(format!(
                            "{note}; session killed; partial output (NOT re-run — side effects may already have happened):\n{partial}"
                        ));
                    }
                    PsRun::Broken { note } => {
                        log(&format!("PS session unusable ({note}); one-shot powershell -File instead"));
                    }
                }
            }
            let mut script_path: Option<std::path::PathBuf> = None;
            let mut cmd = if shell == "powershell" {
                // The command is DATA, never a quoted argv fragment: write
                // it verbatim to a scratch .ps1 (UTF-8 BOM so Windows
                // PowerShell 5.1 decodes non-ASCII) and run it with -File.
                // This kills the cmd -> powershell -> Add-Type quote
                // nesting failure class entirely: zero quoting layers.
                let script = fm_tmp_dir().join(format!(
                    "cmd-{}-{}.ps1",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
                let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
                bytes.extend_from_slice(command.as_bytes());
                std::fs::write(&script, &bytes)
                    .map_err(|e| format!("cannot write scratch script: {e}"))?;
                log(&format!(
                    "RUN powershell -File {} ({} bytes of script)",
                    script.display(),
                    command.len()
                ));
                script_path = Some(script.clone());
                let mut c = tokio::process::Command::new("powershell.exe");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
                c.arg(&script);
                c
            } else if needs_shell(command) {
                // Pipes, &&, redirection, cmd built-ins: run via cmd.exe.
                // /S /C "..." strips only the outermost quotes, so inner
                // quotes survive (plain /C mangles them). raw_arg avoids
                // Rust re-escaping the string.
                log(&format!("RUN via cmd shell: {}", command));
                let mut c = tokio::process::Command::new("cmd.exe");
                c.raw_arg("/S")
                    .raw_arg("/C")
                    .raw_arg(format!("\"{}\"", command));
                c
            } else {
                // Direct launch: quotes are parsed by us, so multi-word
                // quoted arguments reach the program intact.
                let argv = parse_command(command);
                let (program, rest) = match argv.split_first() {
                    Some((p, r)) => (p, r),
                    None => return Err("empty command".into()),
                };
                log(&format!("RUN direct: {} {:?}", program, rest));
                let mut c = tokio::process::Command::new(program);
                c.args(rest);
                c
            };
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            if background {
                // The scratch script must outlive the spawn; tmp/ is
                // LRU-trimmed at startup, so it cannot leak forever.
                return spawn_background(cmd, command).await;
            }
            let output = exec_capped(&mut cmd, cmd_timeout()).await;
            if let Some(p) = &script_path {
                let _ = std::fs::remove_file(p);
            }
            let output = output?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !output.status.success() {
                Err(format!("exit {:?}: {}\n{}", output.status, stdout, stderr))
            } else {
                Ok(stdout)
            }
        }
        "get_command_output" => {
            let pid = args.get("pid").and_then(|p| p.as_u64()).ok_or("pid required")? as u32;
            let reg = bg_registry().lock().unwrap();
            let job = reg
                .get(&pid)
                .ok_or_else(|| format!("no background job with pid {pid}"))?;
            let status = job.status.lock().unwrap().clone();
            let out = String::from_utf8_lossy(&job.stdout.lock().unwrap()).to_string();
            let err = String::from_utf8_lossy(&job.stderr.lock().unwrap()).to_string();
            Ok(format!(
                "pid {pid} [{}] status: {status}, {}s elapsed\n--- stdout ---\n{out}\n--- stderr ---\n{err}",
                job.command,
                job.started.elapsed().as_secs()
            ))
        }
        "kill_command" => {
            let pid = args.get("pid").and_then(|p| p.as_u64()).ok_or("pid required")? as u32;
            let out = tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!(
                    "taskkill failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            log(&format!("KILLED background pid {pid}"));
            Ok(format!("killed pid {pid} and its process tree"))
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
        "search_files" => {
            let pattern = args
                .get("pattern")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_lowercase();
            if pattern.is_empty() {
                return Err("pattern is required".into());
            }
            let root = args.get("root").and_then(|c| c.as_str()).unwrap_or("C:\\");
            let mut out = Vec::new();
            search_files_walk(std::path::Path::new(root), &pattern, 8, &mut out).await;
            if out.is_empty() {
                Ok(format!("no files or directories matching '{pattern}' under {root}"))
            } else {
                Ok(out.join("\n"))
            }
        }
        "list_processes" => {
            let output = tokio::process::Command::new("tasklist")
                .args(["/FO", "CSV", "/NH"])
                .output()
                .await
                .map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        "load_skill" => {
            let name = args.get("name").and_then(|c| c.as_str()).unwrap_or("");
            let dir = skill_dir(name).ok_or_else(|| {
                let avail: Vec<String> = discover_skills().into_iter().map(|s| s.name).collect();
                format!("no such skill: {name} (available: {})", avail.join(", "))
            })?;
            let md = tokio::fs::read_to_string(dir.join("SKILL.md"))
                .await
                .map_err(|e| e.to_string())?;
            let (_, _, tools, body) = parse_frontmatter(&md);
            if tools.is_empty() {
                Ok(body)
            } else {
                Ok(format!(
                    "{body}\n\n[Tools now enabled for the rest of this conversation: {}]",
                    tools.join(", ")
                ))
            }
        }
        "read_skill_resource" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let file = resolve_skill_resource(path)?;
            tokio::fs::read_to_string(&file)
                .await
                .map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

/// Recursive name search: case-insensitive substring, depth- and
/// result-capped, hidden entries skipped.
fn search_files_walk<'a>(
    root: &'a std::path::Path,
    pattern: &'a str,
    depth: u32,
    out: &'a mut Vec<String>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        const CAP: usize = 200;
        if depth == 0 || out.len() >= CAP {
            return;
        }
        let Ok(mut dir) = tokio::fs::read_dir(root).await else {
            return;
        };
        while let Ok(Some(entry)) = dir.next_entry().await {
            if out.len() >= CAP {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if name.to_lowercase().contains(pattern) {
                let p = entry.path().to_string_lossy().to_string();
                out.push(if ft.is_dir() { format!("{p}/") } else { p });
            }
            if ft.is_dir() {
                search_files_walk(&entry.path(), pattern, depth - 1, out).await;
            }
        }
    })
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
    CHAT_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
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
                let _ = app.emit("tool_call", serde_json::json!({ "name": &name, "args": &args }));
                let result = if tool_is_disclosed(&active, &name) {
                    execute_tool(&name, &args, &mut seen)
                        .await
                        .unwrap_or_else(|e| format!("Error: {e}"))
                } else {
                    format!("Error: tool '{name}' is not available in this conversation. Its implementation exists but its schema is not disclosed; load the skill whose 'tools:' list enables it, then call it again.")
                };
                let ok = !result.starts_with("Error");
                let _ = app.emit("tool_result", serde_json::json!({
                    "name": &name,
                    "summary": result.chars().take(160).collect::<String>(),
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
                let _ = app.emit("tool_call", serde_json::json!({ "name": &name, "args": &args }));
                let result = if tool_is_disclosed(&tools, &name) {
                    execute_tool(&name, &args, &mut seen)
                        .await
                        .unwrap_or_else(|e| format!("Error: {e}"))
                } else {
                    format!("Error: tool '{name}' is not available in this conversation. Its implementation exists but its schema is not disclosed; load the skill whose 'tools:' list enables it, then call it again.")
                };
                let _ = app.emit("tool_result", serde_json::json!({
                    "name": &name,
                    "summary": result.chars().take(160).collect::<String>(),
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
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, list_models, test_llm, chat_with_llm, stop_chat, list_skills, list_conversations, load_conversation, save_conversation, get_system_prompt, open_path])
        .setup(|app| {
            // Scratch hygiene (P6): drop old generations of generated
            // scripts and spilled output from previous sessions.
            trim_dir(&fm_tmp_dir(), 50);
            trim_dir(&fm_out_dir(), 50);
            // Tray: left-click toggles the console; menu for explicit actions.
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Show / Hide", true, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle_item, &settings_item, &quit_item])?;

            // The anchor glyph rendered from Segoe UI Emoji (icons/tray.png),
            // not the default Tauri icon.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            TrayIconBuilder::new()
                .icon(tray_icon)
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
        budget_output, conv_dir, core_tools, discover_skills, enable_skill_tools, exec_capped,
        execute_tool_inner, extract_screenshot_path, get_system_prompt,
        looks_like_unexecuted_intent, merge_tool_call_delta, needs_shell, parse_command,
        parse_conversation, parse_frontmatter, resolve_conv_path, resolve_open_target,
        resolve_skill_resource,
        sanitize_conv_name, serialize_conversation, skill_dir, spawn_background, tool_is_disclosed,
        trim_dir, truncate_if_over,
        ps_session_exec, ps_session_shutdown, PsRun,
        CallAcc, ConvMsg, CORE_TOOLS,
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
    fn oversized_output_is_capped_with_spill_pointer() {
        let mut seen = HashMap::new();
        let huge: String = (0..9000usize).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let out = budget_output(&mut seen, "c".into(), huge.clone());
        assert!(out.contains("chars omitted; FULL output saved:"));
        assert!(out.starts_with('a'));
        assert!(out.ends_with(&huge[huge.len() - 50..]));
        assert!(out.chars().count() < 8600);
        let path = out
            .lines()
            .find(|l| l.contains("FULL output saved:"))
            .unwrap()
            .split("saved: ")
            .nth(1)
            .unwrap()
            .split(" —")
            .next()
            .unwrap()
            .to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), huge);
        let _ = std::fs::remove_file(&path);
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

    #[test]
    fn frontmatter_splits_metadata_from_body() {
        let md = "---\nname: demo\ndescription: A demo skill.\ntools: read_file, list_dir\n---\n\n# Body\nDo things.\n";
        let (name, desc, tools, body) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(desc.as_deref(), Some("A demo skill."));
        assert_eq!(tools, vec!["read_file".to_string(), "list_dir".to_string()]);
        assert_eq!(body, "# Body\nDo things.\n");
        // No frontmatter: body passes through untouched.
        let (n2, d2, t2, b2) = parse_frontmatter("plain text");
        assert!(n2.is_none() && d2.is_none());
        assert!(t2.is_empty());
        assert_eq!(b2, "plain text");
    }

    #[test]
    fn repo_skill_is_discoverable_and_loadable() {
        // cargo test runs with cwd = src-tauri, so ../skills is the repo root.
        let skills = discover_skills();
        let git = skills.iter().find(|s| s.name == "git-repo").expect("git-repo skill");
        assert!(git.description.contains("git repositories"));
        let dir = skill_dir("git-repo").expect("skill dir");
        let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        let (.., body) = parse_frontmatter(&md);
        assert!(body.starts_with("# Git Repos"));
        assert!(!body.contains("description:"));
    }

    #[test]
    fn winapp_skill_is_discoverable_with_cost_guidance() {
        // winapp is a skill, not prompt-hardcoded: discovery must find it and
        // the body must carry the measured latency guidance. It is pure
        // behavior: it enables no tools (it drives run_command).
        let skills = discover_skills();
        let win = skills
            .iter()
            .find(|s| s.name == "winapp")
            .expect("winapp skill");
        assert!(win.description.contains("winapp CLI"), "{}", win.description);
        let dir = skill_dir("winapp").expect("winapp skill dir");
        let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        let (_, _, tools, body) = parse_frontmatter(&md);
        assert!(body.contains("# WinApp UI Automation"));
        assert!(body.contains("measured on this machine"));
        assert!(tools.is_empty(), "winapp must not enable tools");
    }

    #[test]
    fn fs_tools_hidden_until_filesystem_skill_enables_them() {
        // Tool availability is skill-driven: the core context carries only
        // command+skill primitives; the filesystem skill's frontmatter
        // enables the fs tools; unknown names can't invent tools.
        let core = core_tools();
        let core_names: Vec<&str> = core
            .iter()
            .map(|t| t.pointer("/function/name").and_then(|n| n.as_str()).unwrap())
            .collect();
        assert_eq!(core_names.len(), CORE_TOOLS.len());
        for hidden in ["read_file", "write_file", "list_dir", "search_files", "list_processes"] {
            assert!(!core_names.contains(&hidden), "{hidden} must not be in core");
        }
        let fs_skill = skill_dir("filesystem").expect("filesystem skill dir");
        let md = std::fs::read_to_string(fs_skill.join("SKILL.md")).unwrap();
        let (_, _, declared, _) = parse_frontmatter(&md);
        assert_eq!(declared.len(), 5, "filesystem skill declares 5 tools");
        let mut active = core.clone();
        let added = enable_skill_tools(&mut active, "filesystem");
        assert_eq!(added.len(), 5);
        assert_eq!(active.len(), core.len() + 5);
        // Idempotent: loading twice adds nothing.
        assert!(enable_skill_tools(&mut active, "filesystem").is_empty());
        // Pure-behavior skill enables nothing; unknown skill enables nothing.
        assert!(enable_skill_tools(&mut active, "winapp").is_empty());
        assert!(enable_skill_tools(&mut active, "no-such-skill").is_empty());
    }

    #[test]
    fn undisclosed_tools_are_refused_at_dispatch() {
        // The gate mirrors disclosure: core run cannot execute fs tools;
        // after the skill enables them, the same check passes.
        let core = core_tools();
        assert!(tool_is_disclosed(&core, "run_command"));
        assert!(tool_is_disclosed(&core, "load_skill"));
        for hidden in ["read_file", "write_file", "list_dir", "search_files", "list_processes"] {
            assert!(!tool_is_disclosed(&core, hidden), "{hidden} undisclosed");
        }
        let mut active = core.clone();
        enable_skill_tools(&mut active, "filesystem");
        assert!(tool_is_disclosed(&active, "read_file"));
    }

    #[test]
    fn open_path_refuses_everything_but_http_https_and_existing_files() {
        assert!(resolve_open_target("javascript:alert(1)").is_err());
        assert!(resolve_open_target("powershell.exe -c calc").is_err());
        assert!(resolve_open_target("file:///C:/nope/nothing-here-xyzzy.txt").is_err());
        // URL metacharacters are data for ShellExecuteW, not injection vectors;
        // query strings must survive untouched.
        assert_eq!(
            resolve_open_target("https://example.com/&calc|x").unwrap(),
            "https://example.com/&calc|x"
        );
        // file:// paths are normalized to backslashes for handler argv parsers.
        assert_eq!(
            resolve_open_target("file:///C:/Windows/System32/drivers/etc/hosts").unwrap(),
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        );
    }

    #[test]
    fn tool_ledger_round_trips_through_conversation_file() {
        let msgs = vec![
            ConvMsg { role: "user".into(), text: "do it".into() },
            ConvMsg { role: "tool".into(), text: "🔧 list_dir path=C:\\x -> ok: a, b".into() },
            ConvMsg { role: "assistant".into(), text: "done".into() },
        ];
        let raw = serialize_conversation("demo", &msgs);
        let (name, back) = parse_conversation(&raw);
        assert_eq!(name, "demo");
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].role, "tool");
        assert!(back[1].text.contains("-> ok:"));
    }

    #[test]
    fn system_prompt_comes_from_agents_md_not_the_binary() {
        // cargo test runs with cwd = src-tauri: ../AGENTS.md is the repo file.
        // It carries the file:// link rule; the built-in fallback does not.
        let p = get_system_prompt();
        assert!(p.contains("First Mate"));
        assert!(p.contains("file://"), "repo AGENTS.md must win over fallback");
    }

    #[test]
    fn skill_resource_resolution_is_locked_down() {
        // Traversal and malformed shapes fail before any filesystem access.
        assert!(resolve_skill_resource("git-repo/../secrets.txt").is_err());
        assert!(resolve_skill_resource("git-repo").is_err());
        assert!(resolve_skill_resource("no-such-skill/references/x.md").is_err());
        // A real reference file resolves inside the skill dir.
        let f = resolve_skill_resource("git-repo/references/recipes.md").unwrap();
        assert!(f.is_file());
        assert!(f.starts_with(skill_dir("git-repo").unwrap().canonicalize().unwrap()));
    }

    #[test]
    fn conversation_prelude_is_first_line() {
        let msgs = vec![
            ConvMsg { role: "user".into(), text: "hi\nthere".into() },
            ConvMsg { role: "assistant".into(), text: "hello".into() },
        ];
        let ser = serialize_conversation("My chat", &msgs);
        // Scanning needs only the first line.
        assert_eq!(ser.lines().next(), Some("My chat"));
        let (name, back) = parse_conversation(&ser);
        assert_eq!(name, "My chat");
        assert_eq!(back.len(), 2);
        // Multi-line text survives the JSONL round-trip.
        assert_eq!(back[0].text, "hi\nthere");
    }

    #[test]
    fn conv_names_are_filesystem_safe() {
        assert_eq!(sanitize_conv_name("a/b:c*d?e\"f<g>h|i"), "a-b-c-d-e-f-g-h-i");
        assert_eq!(sanitize_conv_name("..."), "chat");
        assert_eq!(sanitize_conv_name(&"x".repeat(80)).len(), 48);
    }

    #[test]
    fn conv_path_stays_inside_store() {
        std::fs::create_dir_all(conv_dir()).unwrap();
        let f = conv_dir().join("_firstmate_test.chat");
        std::fs::write(&f, "test\n").unwrap();
        assert!(resolve_conv_path(f.to_str().unwrap()).is_ok());
        assert!(resolve_conv_path("C:\\Windows\\win.ini").is_err());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn hung_command_is_killed_at_the_cap() {
        // The soft-lock shape: the process keeps stdout open and never
        // exits (here ping -n 3600; in the wild, `start /b gh auth login`).
        // The call must return with an error, not wait for EOF forever.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let t0 = std::time::Instant::now();
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "3600", "127.0.0.1"]);
            let err = exec_capped(&mut c, std::time::Duration::from_secs(2))
                .await
                .expect_err("hanging command must not return Ok");
            assert!(err.contains("timed out"), "{err}");
            assert!(t0.elapsed() < std::time::Duration::from_secs(30));
            // The child must actually be dead, not orphaned holding pipes.
            // PID-scoped so parallel ping tests can't interfere.
            let pid: u32 = err
                .split("killed pid ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .expect("timeout error must name the killed pid");
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
                "pid {pid} survived the timeout kill"
            );
        });
    }

    fn bg_pid(start_msg: &str) -> u32 {
        start_msg
            .split_whitespace()
            .nth(3)
            .unwrap()
            .trim_end_matches('.')
            .parse()
            .unwrap()
    }

    #[test]
    fn background_command_completes_and_output_is_captured() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "3", "127.0.0.1"]);
            let msg = spawn_background(c, "ping -n 3 127.0.0.1").await.unwrap();
            let pid = bg_pid(&msg);
            let mut poll = String::new();
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                poll = execute_tool_inner("get_command_output", &serde_json::json!({"pid": pid}))
                    .await
                    .unwrap();
                if !poll.contains("status: running") {
                    break;
                }
            }
            assert!(poll.contains("status: exited: 0"), "{poll}");
            let stdout = poll.split("--- stdout ---").nth(1).unwrap();
            assert!(!stdout.trim().is_empty(), "background stdout must be captured");
        });
    }

    #[test]
    fn background_command_can_be_killed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "600", "127.0.0.1"]);
            let msg = spawn_background(c, "ping -n 600 127.0.0.1").await.unwrap();
            let pid = bg_pid(&msg);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let res = execute_tool_inner("kill_command", &serde_json::json!({"pid": pid}))
                .await
                .unwrap();
            assert!(res.contains("killed"), "{res}");
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
                "pid {pid} survived kill_command"
            );
        });
    }

    #[test]
    fn foreground_command_captures_stdout_both_paths() {
        // Regression: exec_capped once spawned without piping stdio, so
        // wait_with_output returned empty output for every success.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // cmd-shell branch (built-in).
            let out = execute_tool_inner(
                "run_command",
                &serde_json::json!({"command": "echo CAPTURED_FOREGROUND"}),
            )
            .await
            .unwrap();
            assert!(out.contains("CAPTURED_FOREGROUND"), "shell path lost stdout: {out:?}");
            // Direct branch (real binary with a quoted argument).
            let out = execute_tool_inner(
                "run_command",
                &serde_json::json!({"command": "tasklist /FI \"IMAGENAME eq svchost.exe\" /NH"}),
            )
            .await
            .unwrap();
            assert!(out.contains("svchost"), "direct path lost stdout: {out:?}");
        });
    }

    #[test]
    fn intent_detection_separates_declared_action_from_final_answer() {
        // Both real transcript cases that ended a run with no tool call.
        assert!(looks_like_unexecuted_intent("Let me verify the sign-in state."));
        assert!(looks_like_unexecuted_intent(
            "Right — browser session ≠ CLI auth. Those are separate, so `gh` is still unauthenticated. Let me set up the device flow; since your browser session is now signed in, I should be able to complete the authorization myself."
        ));
        assert!(looks_like_unexecuted_intent("First, a quick check:"));
        // Real final answers must end the run without a nudge.
        assert!(!looks_like_unexecuted_intent(
            "Done. Git 2.52 and gh 2.91 are installed and on PATH."
        ));
        assert!(!looks_like_unexecuted_intent(&"Summary line. ".repeat(40)));
        assert!(!looks_like_unexecuted_intent(""));
    }

    #[test]
    fn truncate_spills_full_output_behind_a_pointer() {
        let big: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let capped = truncate_if_over(big.clone(), 800);
        assert!(capped.starts_with("line 0\n"));
        assert!(capped.ends_with("line 499\n"));
        let line = capped
            .lines()
            .find(|l| l.contains("FULL output saved:"))
            .expect("pointer line");
        let path = line
            .split("saved: ")
            .nth(1)
            .expect("path")
            .split(" —")
            .next()
            .expect("terminator")
            .to_string();
        let full = std::fs::read_to_string(&path).expect("spill file must exist and be complete");
        assert_eq!(full, big);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trim_dir_keeps_only_the_newest() {
        let dir = std::env::temp_dir().join(format!("fm-trim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        for i in 0..5u32 {
            let p = dir.join(format!("f{i}.txt"));
            std::fs::write(&p, "x").unwrap();
            if i < 3 {
                let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
                f.set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
        }
        trim_dir(&dir, 2);
        assert!(!dir.join("f0.txt").exists() && !dir.join("f2.txt").exists());
        assert!(dir.join("f3.txt").exists() && dir.join("f4.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn powershell_shell_mode_survives_quote_hell() {
        // Exactly the payload class that died in the cmd -> powershell ->
        // Add-Type chain: single-quoted string with an escaped quote and
        // embedded double quotes, plus $ interpolation. As script DATA
        // there is no quoting layer left to corrupt it.
        let cmd = "$name = 'world''s \"best\"'\nWrite-Output \"hello $name\"\n";
        let out = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": cmd, "shell": "powershell" }),
        )
        .await
        .expect("powershell -File run");
        assert!(
            out.contains("hello world's \"best\""),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn cwd_is_validated_and_applied() {
        let bad = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "cd", "cwd": "C:\\no-such-dir-fm" }),
        )
        .await;
        assert!(bad.unwrap_err().contains("cwd is not a directory"));
        let ok = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "cd", "cwd": "C:\\Windows" }),
        )
        .await
        .unwrap();
        assert_eq!(ok.trim().to_lowercase(), "c:\\windows");
    }

    /// One test fn on a current-thread runtime: the session is global, so
    /// its lifecycle checks must not interleave with each other.
    #[tokio::test]
    async fn ps_session_persists_state_and_survives_failures() {
        use std::time::Duration;
        let t = Duration::from_secs(60);
        // 1. State persists across separate tool-level calls.
        let a = ps_session_exec("$fmTestVar = 6 * 7", None, t).await;
        assert!(matches!(a, PsRun::Done { code: 0, .. }), "spawn+assign: {a:?}");
        let b = ps_session_exec("Write-Output \"val=$fmTestVar\"", None, t).await;
        match b {
            PsRun::Done { stdout, code: 0, .. } => {
                assert!(stdout.contains("val=42"), "state must persist: {stdout}");
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // 2. Multiline script with a real multi-line construct (here-string).
        let c = ps_session_exec(
            "$here = @'\nline one\nline two\n'@\nWrite-Output $here",
            None,
            t,
        )
        .await;
        match c {
            PsRun::Done { stdout, code: 0, .. } => {
                assert!(stdout.contains("line one") && stdout.contains("line two"));
            }
            other => panic!("here-string: {other:?}"),
        }
        // 3. A failing command reports failure but does NOT kill the session.
        let d = ps_session_exec("throw 'boom'", None, t).await;
        match d {
            PsRun::Done { code, stderr, .. } => {
                assert_eq!(code, 1, "throw must surface as failure");
                assert!(stderr.contains("boom"), "error text on stderr: {stderr}");
            }
            other => panic!("throw: {other:?}"),
        }
        let e = ps_session_exec("Write-Output alive", None, t).await;
        assert!(matches!(e, PsRun::Done { code: 0, .. }), "session survives throw: {e:?}");
        // 4. cwd param applies and persists in the session.
        let f = ps_session_exec("", Some("C:\\Windows".into()), t).await;
        assert!(matches!(f, PsRun::Done { code: 0, .. }), "set-location: {f:?}");
        let g = ps_session_exec("(Get-Location).Path", None, t).await;
        match g {
            PsRun::Done { stdout, .. } => {
                assert_eq!(stdout.trim().to_lowercase(), "c:\\windows");
            }
            other => panic!("cwd persist: {other:?}"),
        }
        // 5. Timeout kills the session (no re-run), and the next call
        //    transparently respawns.
        let h = ps_session_exec("Start-Sleep -Seconds 30", None, Duration::from_secs(2)).await;
        match h {
            PsRun::TimedOut { note, .. } => assert!(note.contains("timed out"), "{note}"),
            other => panic!("timeout expected, got {other:?}"),
        }
        let i = ps_session_exec("Write-Output respawned", None, t).await;
        assert!(matches!(i, PsRun::Done { code: 0, .. }), "respawn: {i:?}");
        // 6. Through the tool surface: session failure surfaces as tool error.
        let bad = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "exit 3", "shell": "powershell" }),
        )
        .await;
        assert!(bad.unwrap_err().contains("exit code 3"));
        ps_session_shutdown().await;
    }
}
