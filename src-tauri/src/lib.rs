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

/// Hard stop for the tool loop: a model that keeps calling tools forever is
/// a broken run, not a working one.
const MAX_TURNS: u32 = 8;

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
                "description": "Execute a command on the Windows system and return the output. Foreground commands are force-killed after 5 minutes, so interactive or long-running programs (login prompts, servers, watchers) MUST use background:true and are driven via get_command_output / kill_command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command to execute (e.g. 'winapp ui inspect -a notepad')"},
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

/// A skill advertised to the model: name + description from SKILL.md
/// frontmatter (Agent Skills spec, see docs/SKILLS.md).
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

/// Parse `---` frontmatter. Returns (name, description, body-without-frontmatter).
fn parse_frontmatter(md: &str) -> (Option<String>, Option<String>, String) {
    let Some(rest) = md
        .strip_prefix("---")
        .and_then(|r| r.strip_prefix('\n'))
        .or_else(|| md.strip_prefix("---\r\n"))
    else {
        return (None, None, md.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None, md.to_string());
    };
    let mut name = None;
    let mut description = None;
    for line in rest[..end].lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    (name, description, body)
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
            let (name, description, _) = parse_frontmatter(&md);
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
            let (fm_name, _, _) = parse_frontmatter(&md);
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
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
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
            let mut cmd = if needs_shell(command) {
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
            if background {
                return spawn_background(cmd, command).await;
            }
            let output = exec_capped(&mut cmd, cmd_timeout()).await?;
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
            let (_, _, body) = parse_frontmatter(&md);
            Ok(body)
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
            if turn >= MAX_TURNS {
                log(&format!("TURN CAP: stopping at {MAX_TURNS} turns — model not converging"));
                let _ = app.emit("stream_done", serde_json::json!({}));
                return Ok(format!(
                    "{text}\n(stopped after {MAX_TURNS} tool turns without converging — try rephrasing the task)"
                ));
            }
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
            if turn >= MAX_TURNS {
                log(&format!("TURN CAP: stopping at {MAX_TURNS} turns — model not converging"));
                let _ = app.emit("stream_done", serde_json::json!({}));
                return Ok(format!(
                    "{text}\n(stopped after {MAX_TURNS} tool turns without converging — try rephrasing the task)"
                ));
            }
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
        .invoke_handler(tauri::generate_handler![get_hotkey, get_settings, save_settings, list_models, test_llm, chat_with_llm, list_skills, list_conversations, load_conversation, save_conversation])
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
        budget_output, conv_dir, discover_skills, exec_capped, execute_tool_inner,
        extract_screenshot_path, merge_tool_call_delta, needs_shell, parse_command,
        parse_conversation, parse_frontmatter, resolve_conv_path, resolve_skill_resource,
        sanitize_conv_name, serialize_conversation, skill_dir, spawn_background, CallAcc, ConvMsg,
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

    #[test]
    fn frontmatter_splits_metadata_from_body() {
        let md = "---\nname: demo\ndescription: A demo skill.\n---\n\n# Body\nDo things.\n";
        let (name, desc, body) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(desc.as_deref(), Some("A demo skill."));
        assert_eq!(body, "# Body\nDo things.\n");
        // No frontmatter: body passes through untouched.
        let (n2, d2, b2) = parse_frontmatter("plain text");
        assert!(n2.is_none() && d2.is_none());
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
        let (_, _, body) = parse_frontmatter(&md);
        assert!(body.starts_with("# Git Repos"));
        assert!(!body.contains("description:"));
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
}
