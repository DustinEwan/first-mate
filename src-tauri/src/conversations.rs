use crate::log::{home_dir, log};
use crate::report::ToolReport;

/// One persisted turn. Only user/assistant turns round-trip.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ConvMsg {
    role: String,
    text: String,
    /// Structured facts for tool turns: the UI renders these on resume;
    /// `text` is the model-facing transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<ToolReport>,
}

#[derive(serde::Serialize)]
pub(crate) struct ConvInfo {
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
pub(crate) fn save_conversation(name: String, messages: Vec<ConvMsg>) -> Result<String, String> {
    std::fs::create_dir_all(conv_dir()).map_err(|e| e.to_string())?;
    let path = conv_dir().join(format!("{}.chat", sanitize_conv_name(&name)));
    std::fs::write(&path, serialize_conversation(&name, &messages)).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
pub(crate) fn list_conversations() -> Vec<ConvInfo> {
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
    out.sort_by_key(|c| std::cmp::Reverse(c.modified));
    out
}

#[tauri::command]
pub(crate) fn load_conversation(path: String) -> Result<Vec<ConvMsg>, String> {
    let full = resolve_conv_path(&path)?;
    let raw = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
    Ok(parse_conversation(&raw).1)
}

/// Delete a conversation file. The path goes through the same containment
/// check as load: only files inside the conversations dir can be removed.
#[tauri::command]
pub(crate) fn delete_conversation(path: String) -> Result<(), String> {
    let full = resolve_conv_path(&path)?;
    std::fs::remove_file(&full).map_err(|e| e.to_string())?;
    log(&format!("CONV DELETE {}", full.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_ledger_round_trips_through_conversation_file() {
        let msgs = vec![
            ConvMsg { role: "user".into(), text: "do it".into(), report: None },
            ConvMsg {
                role: "tool".into(),
                text: "→ x\n✅ 0.0s · 2 entries".into(),
                report: Some(ToolReport {
                    tool: "list_dir".into(),
                    subject: "C:\\x".into(),
                    detail: None,
                    shell: None,
                    cwd: None,
                    background: None,
                    from_tag: None,
                    ok: true,
                    error: None,
                    status: None,
                    pid: None,
                    wall_ms: 0,
                    tag: None,
                    count: Some(2),
                    unit: Some("entries".into()),
                    body: None,
                }),
            },
            ConvMsg { role: "assistant".into(), text: "done".into(), report: None },
        ];
        let raw = serialize_conversation("demo", &msgs);
        let (name, back) = parse_conversation(&raw);
        assert_eq!(name, "demo");
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].role, "tool");
        assert!(back[1].text.starts_with("→ x"));
        // Facts survive the round trip: the UI re-renders from them, no parsing.
        let r = back[1].report.as_ref().unwrap();
        assert_eq!(r.subject, "C:\\x");
        assert_eq!(r.count, Some(2));
    }

    #[test]
    fn conversation_prelude_is_first_line() {
        let msgs = vec![
            ConvMsg { role: "user".into(), text: "hi\nthere".into(), report: None },
            ConvMsg { role: "assistant".into(), text: "hello".into(), report: None },
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
    fn delete_cannot_escape_conversations_dir() {
        // Anything outside the conversations dir is refused before any
        // remove_file runs: existing file elsewhere, traversal, missing file.
        assert!(resolve_conv_path("C:\\Windows\\win.ini").is_err());
        assert!(resolve_conv_path("..\\..\\settings.json").is_err());
        assert!(resolve_conv_path("no-such-file-here").is_err());
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
}
