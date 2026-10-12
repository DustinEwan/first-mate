//! The conversation record: one sealed file per conversation, and every line is
//! evidence of what a receiver got.
//!
//! `docs/roadmap/REDACTION.md`, section "On-disk truth and the console
//! indicator", is the design. The send path writes this file, because the send
//! path is the only place that holds the text as it went out. The console holds
//! the text after `redact::Redactor::detokenize`, so the console cannot record
//! what it was never given.
//!
//! A leak scan reads this file. A value that appears in a `sent` line reached a
//! receiver, so the scan needs no guess about which turn carried what.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::crypto;
use crate::leak;
use crate::log::home_dir;
use crate::redact;
use crate::report::ToolReport;

/// One line of a conversation file. Line 1 of every file is the conversation
/// name, so `list_conversations` needs no JSON parse to label a file.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Record {
    /// The message object exactly as one request carried it, tokens and all.
    #[serde(rename_all = "camelCase")]
    Sent {
        at: u64,
        provider: String,
        model: String,
        base_url: String,
        /// The tool that produced a `tool` row. The row carries only its call
        /// id, and the report names the tool that ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        msg: serde_json::Value,
    },
    /// The console's ledger row for one tool call. Duration, counts and the
    /// working directory exist in no wire row, so the ledger lives beside them.
    #[serde(rename_all = "camelCase")]
    Ledger { at: u64, call_id: String, report: ToolReport },
    /// The rows one fold removed, and the brief that replaced them. The fold
    /// sends the dropped rows to the summarizer in plaintext, so a later scan
    /// needs the rows after the fold, when a new declared value arrives.
    #[serde(rename_all = "camelCase")]
    Fold {
        at: u64,
        provider: String,
        model: String,
        base_url: String,
        brief: String,
        dropped: Vec<serde_json::Value>,
    },
    /// A row an earlier build wrote. That build saved the console's display
    /// text, so no scan may claim the row left the machine.
    Legacy { role: String, text: String, report: Option<ToolReport> },
}

/// A row the console renders. `load_conversation` detokenizes the stored text,
/// because the console shows real values and the file keeps handles.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisplayRow {
    role: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<ToolReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folded: Option<usize>,
}

/// One fold a conversation recorded, for the report panel to offer.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FoldInfo {
    conversation: String,
    at: u64,
    dropped: usize,
    provider: String,
    model: String,
    base_url: String,
}

pub(crate) fn conv_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = TEST_DIR.with(|slot| slot.borrow().clone()) {
        return dir;
    }
    home_dir().join(".firstmate").join("conversations")
}

#[cfg(test)]
thread_local! {
    /// A test writes its own conversation, because the real ones hold real values.
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Point this thread's conversation directory at a fresh directory of its own.
#[cfg(test)]
pub(crate) fn use_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fm-conv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TEST_DIR.with(|slot| *slot.borrow_mut() = Some(dir.clone()));
    dir
}

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

fn path_for(name: &str) -> PathBuf {
    conv_dir().join(format!("{}.chat", sanitize_conv_name(name)))
}

/// The text one message carries. A parts array holds the text parts and the
/// base64 of a screenshot; only the text parts are text.
pub(crate) fn row_text(m: &serde_json::Value) -> String {
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

/// The key that says a row is already on disk.
///
/// A tool row is keyed by its call id, because `context::prune_old_tool_bodies`
/// rewrites an old tool row and the elided copy is the same row sent again. A
/// row with tool calls is keyed by those call ids for the same reason. Every
/// other row is keyed by its own bytes, plus which copy of that byte string
/// this is, so a conversation that says `yes` twice keeps both rows.
fn row_key(msg: &serde_json::Value, repeat: usize) -> String {
    if let Some(id) = msg.get("tool_call_id").and_then(|i| i.as_str()) {
        return format!("call:{id}");
    }
    let calls: Vec<&str> = msg
        .get("tool_calls")
        .and_then(|c| c.as_array())
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| c.get("id").and_then(|i| i.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if !calls.is_empty() {
        return format!("calls:{}", calls.join(","));
    }
    format!("json:{msg}#{repeat}")
}

/// Which copy of an identical row this is, counted in arrival order.
fn next_copy(counts: &mut HashMap<String, usize>, msg: &serde_json::Value) -> usize {
    *counts.entry(row_key(msg, 0)).and_modify(|n| *n += 1).or_insert(0)
}

fn parse_records(raw: &str) -> (String, Vec<Record>) {
    let mut lines = raw.lines();
    let name = lines.next().unwrap_or("Untitled").trim().to_string();
    let name = if name.is_empty() { "Untitled".to_string() } else { name };
    let mut records: Vec<Record> = Vec::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("kind").is_some() {
            if let Ok(record) = serde_json::from_value::<Record>(value) {
                records.push(record);
            }
            continue;
        }
        // A build before the wire record saved `{role, text, report}`.
        #[derive(serde::Deserialize)]
        struct Legacy {
            role: String,
            text: String,
            #[serde(default)]
            report: Option<ToolReport>,
        }
        if let Ok(old) = serde_json::from_value::<Legacy>(value) {
            records.push(Record::Legacy { role: old.role, text: old.text, report: old.report });
        }
    }
    (name, records)
}

fn serialize_records(name: &str, records: &[Record]) -> String {
    let mut out = format!("{}\n", name);
    for record in records {
        if let Ok(line) = serde_json::to_string(record) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn read_text(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // First Mate owns this record, so the reader opens the envelope.
    // `crypto::open_store_bytes` refuses the same file to the agent.
    let plain = if crypto::looks_sealed(&bytes) { crypto::open(&bytes)? } else { bytes };
    Ok(String::from_utf8_lossy(&plain).to_string())
}

/// The name and every line of one conversation. A file that will not open
/// reads as nothing, and the writer then leaves the file as it is.
fn read_conversation(path: &std::path::Path) -> Option<(String, Vec<Record>)> {
    if !path.exists() {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled").to_string();
        return Some((name, Vec::new()));
    }
    match read_text(path) {
        Ok(raw) => Some(parse_records(&raw)),
        Err(why) => {
            crate::log::log(&format!("CONV: {} will not open ({why}); leaving it as it is", path.display()));
            None
        }
    }
}

fn write_conversation(name: &str, records: &[Record]) -> Result<(), String> {
    std::fs::create_dir_all(conv_dir()).map_err(|e| e.to_string())?;
    let path = path_for(name);
    std::fs::write(&path, crypto::seal(serialize_records(name, records).as_bytes())?)
        .map_err(|e| e.to_string())
}

/// Append the rows of `wire` the file does not hold yet, and scan each appended
/// row for a value that left in the clear.
///
/// A row goes on disk the first time it leaves, so the re-send of a whole
/// window on a tool turn adds nothing. `carriers` names the tool behind a tool
/// row, because the wire row carries only the call id.
pub(crate) fn record_send(
    to: &leak::Send,
    det: &redact::Redactor,
    wire: &[serde_json::Value],
    carriers: &HashMap<String, String>,
) {
    if to.conversation == leak::UNSAVED || to.conversation.is_empty() {
        return;
    }
    let path = path_for(&to.conversation);
    let Some((name, mut records)) = read_conversation(&path) else {
        return;
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut stored: HashMap<String, usize> = HashMap::new();
    for record in &records {
        if let Record::Sent { msg, .. } = record {
            let nth = next_copy(&mut stored, msg);
            seen.insert(row_key(msg, nth));
        }
    }
    let at = leak::now();
    let mut repeats: HashMap<String, usize> = HashMap::new();
    for msg in wire {
        let nth = next_copy(&mut repeats, msg);
        let key = row_key(msg, nth);
        if !seen.insert(key) {
            continue;
        }
        let tool = msg
            .get("tool_call_id")
            .and_then(|id| id.as_str())
            .and_then(|id: &str| carriers.get(id))
            .cloned();
        leak::record_msg(to, det, msg, tool.as_deref());
        records.push(Record::Sent {
            at,
            provider: to.provider.clone(),
            model: to.model.clone(),
            base_url: to.base_url.clone(),
            tool,
            msg: msg.clone(),
        });
    }
    if let Err(why) = write_conversation(&name, &records) {
        crate::log::log(&format!("CONV: lost rows for {name}: {why}"));
    }
}

/// Store the rows one fold dropped, and the brief that replaced them. The scan
/// of those rows happens at the summarizer call, because the fold sends them in
/// plaintext whether or not the summarizer accepts the brief.
pub(crate) fn record_fold(to: &leak::Send, brief: &str, dropped: &[serde_json::Value]) {
    if to.conversation == leak::UNSAVED || to.conversation.is_empty() {
        return;
    }
    let path = path_for(&to.conversation);
    let Some((name, mut records)) = read_conversation(&path) else {
        return;
    };
    records.push(Record::Fold {
        at: leak::now(),
        provider: to.provider.clone(),
        model: to.model.clone(),
        base_url: to.base_url.clone(),
        brief: brief.to_string(),
        dropped: dropped.to_vec(),
    });
    if let Err(why) = write_conversation(&name, &records) {
        crate::log::log(&format!("CONV: lost a fold for {name}: {why}"));
    }
}

/// Store the console's ledger row for one tool call, so a reopened conversation
/// shows work already done.
pub(crate) fn record_ledger(to: &leak::Send, call_id: &str, report: &ToolReport) {
    if to.conversation == leak::UNSAVED || to.conversation.is_empty() {
        return;
    }
    let path = path_for(&to.conversation);
    let Some((name, mut records)) = read_conversation(&path) else {
        return;
    };
    records.push(Record::Ledger { at: leak::now(), call_id: call_id.to_string(), report: report.clone() });
    if let Err(why) = write_conversation(&name, &records) {
        crate::log::log(&format!("CONV: lost a ledger row for {name}: {why}"));
    }
}

/// Every conversation file, for the leak rescan.
pub(crate) fn files() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(conv_dir()) else {
        return Vec::new();
    };
    dir.flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("chat"))
        .collect()
}

/// The name of one conversation file.
pub(crate) fn name_of(path: &std::path::Path) -> String {
    read_text(path)
        .ok()
        .and_then(|raw| raw.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "Untitled".to_string())
}

/// Every line of one conversation file.
pub(crate) fn records(path: &std::path::Path) -> Vec<Record> {
    read_conversation(path).map(|(_, records)| records).unwrap_or_default()
}

fn display(role: String, text: String, report: Option<ToolReport>, folded: Option<usize>) -> Option<DisplayRow> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(DisplayRow { role, text, report, folded })
}

/// The rows one record list renders, in the order the record list holds them.
/// `Some(det)` turns the handles back into the values the console shows. `None`
/// renders the wire copy, so the fold view shows the markers the receiver saw.
pub(crate) fn render(records: &[Record], det: Option<&redact::Redactor>) -> Vec<DisplayRow> {
    let mut calls: HashMap<String, ToolReport> = HashMap::new();
    for record in records {
        if let Record::Ledger { call_id, report, .. } = record {
            calls.insert(call_id.clone(), report.clone());
        }
    }
    let mut out: Vec<DisplayRow> = Vec::new();
    for record in records {
        match record {
            Record::Sent { msg, .. } => {
                let Some(role) = msg.get("role").and_then(|r| r.as_str()) else { continue };
                if role == "system" {
                    // The console shows the brief on the fold divider, and the
                    // instruction set lives in `prompts/AGENTS.md`, not here.
                    continue;
                }
                let text = match det {
                    Some(det) => det.detokenize(&row_text(msg)),
                    None => row_text(msg),
                };
                let report = msg
                    .get("tool_call_id")
                    .and_then(|id| id.as_str())
                    .and_then(|id| calls.remove(id));
                out.extend(display(role.to_string(), text, report, None));
            }
            Record::Fold { brief, dropped, .. } => {
                out.extend(display(
                    "summary".to_string(),
                    match det {
                        Some(det) => det.detokenize(brief),
                        None => brief.clone(),
                    },
                    None,
                    Some(dropped.len()),
                ));
            }
            Record::Legacy { role, text, report } => {
                out.extend(display(role.clone(), text.clone(), report.clone(), None));
            }
            Record::Ledger { .. } => {}
        }
    }
    out
}

fn resolve_conv_path(path: &str) -> Result<PathBuf, String> {
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
pub(crate) fn list_conversations() -> Vec<crate::conversations::ConvInfo> {
    let mut out: Vec<ConvInfo> = Vec::new();
    for path in files() {
        let Ok(raw) = read_text(&path) else { continue };
        let name = raw.lines().next().unwrap_or("Untitled").trim();
        let modified = std::fs::metadata(&path)
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

#[derive(serde::Serialize)]
pub(crate) struct ConvInfo {
    name: String,
    path: String,
    modified: u64,
}

#[tauri::command]
pub(crate) fn load_conversation(path: String) -> Result<Vec<DisplayRow>, String> {
    let full = resolve_conv_path(&path)?;
    let records = records(&full);
    Ok(render(&records, Some(&redact::Redactor::new())))
}

/// The rows one fold dropped, for the console to show read-only.
#[tauri::command]
pub(crate) fn open_fold(conversation: String, at: u64) -> Result<Vec<DisplayRow>, String> {
    let path = path_for(&conversation);
    if !path.is_file() {
        return Err(format!("no conversation named {conversation}"));
    }
    for record in records(&path) {
        if let Record::Fold { at: fold_at, dropped, .. } = record {
            if fold_at == at {
                let rows: Vec<Record> =
                    dropped.iter().map(|msg| Record::Sent {
                        at,
                        provider: String::new(),
                        model: String::new(),
                        base_url: String::new(),
                        tool: None,
                        msg: msg.clone(),
                    }).collect();
                return Ok(render(&rows, None));
            }
        }
    }
    Err(format!("no fold at {at} in {conversation}"))
}

/// Every fold First Mate recorded, newest first, for the report panel.
#[tauri::command]
pub(crate) fn list_folds() -> Vec<FoldInfo> {
    let mut out: Vec<FoldInfo> = Vec::new();
    for path in files() {
        let name = name_of(&path);
        for record in records(&path) {
            if let Record::Fold { at, dropped, provider, model, base_url, .. } = record {
                out.push(FoldInfo {
                    conversation: name.clone(),
                    at,
                    dropped: dropped.len(),
                    provider,
                    model,
                    base_url,
                });
            }
        }
    }
    out.sort_by_key(|fold| std::cmp::Reverse(fold.at));
    out
}

#[tauri::command]
pub(crate) fn delete_conversation(path: String) -> Result<(), String> {
    let full = resolve_conv_path(&path)?;
    crate::log::log(&format!("CONV DELETE {}", full.display()));
    std::fs::remove_file(&full).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "4111 1111 1111 1111";

    /// A conversation file needs the envelope, and the envelope needs the key.
    fn ready(name: &str) {
        if !crypto::tpm_ready() {
            return;
        }
        crypto::init(std::env::temp_dir().join("fm-envelope"));
        use_dir(name);
    }

    fn to(name: &str) -> leak::Send {
        leak::Send {
            conversation: name.to_string(),
            provider: "openai".into(),
            model: "gpt-test".into(),
            base_url: "https://api.test".into(),
        }
    }

    fn row(role: &str, text: &str) -> serde_json::Value {
        serde_json::json!({ "role": role, "content": text })
    }

    fn sent(name: &str) -> Vec<String> {
        records(&path_for(name))
            .iter()
            .map(|record| match record {
                Record::Sent { msg, .. } => row_text(msg),
                Record::Legacy { text, .. } => text.clone(),
                Record::Fold { brief, .. } => brief.clone(),
                Record::Ledger { .. } => String::new(),
            })
            .collect()
    }

    #[test]
    fn the_file_keeps_the_handles_the_wire_carried() {
        ready("wire");
        let mut det = redact::Redactor::new();
        let wire_text = det.tokenize(&format!("pay {CARD} now"));
        assert!(wire_text.contains("⟦CARD_1⟧"), "the pass issued no handle: {wire_text}");
        record_send(&to("wire"), &det, &[row("user", &wire_text)], &HashMap::new());
        assert_eq!(sent("wire"), vec![wire_text.clone()], "the file must hold the wire text");
        let rows = render(&records(&path_for("wire")), Some(&det));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].text.contains(CARD), "the console lost the value: {}", rows[0].text);
        let wire_rows = render(&records(&path_for("wire")), None);
        assert_eq!(wire_rows[0].text, wire_text.clone(), "the wire view dropped the handle");
    }

    #[test]
    fn a_resend_of_the_same_window_adds_no_row() {
        ready("resend");
        let det = redact::Redactor::new();
        let first = vec![row("user", "find the file")];
        record_send(&to("resend"), &det, &first, &HashMap::new());
        let second = vec![
            row("user", "find the file"),
            row("tool", "payroll.csv"),
        ];
        let mut carriers = HashMap::new();
        carriers.insert("c1".to_string(), "list_dir".to_string());
        record_send(&to("resend"), &det, &second, &carriers);
        record_send(&to("resend"), &det, &second, &carriers);
        assert_eq!(sent("resend").len(), 2, "a re-send wrote a second copy of a row");
    }

    #[test]
    fn two_identical_user_rows_both_stay() {
        ready("twice");
        let det = redact::Redactor::new();
        let wire = vec![row("user", "yes"), row("assistant", "done"), row("user", "yes")];
        record_send(&to("twice"), &det, &wire, &HashMap::new());
        record_send(&to("twice"), &det, &wire, &HashMap::new());
        assert_eq!(sent("twice"), vec!["yes", "done", "yes"]);
    }

    #[test]
    fn a_fold_line_holds_the_rows_a_fold_dropped() {
        ready("fold");
        let det = redact::Redactor::new();
        record_send(&to("fold"), &det, &[row("user", "the task")], &HashMap::new());
        record_fold(&to("fold"), "the brief", &[row("user", "one"), row("tool", "two")]);
        let rows = render(&records(&path_for("fold")), Some(&det));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].role, "summary");
        assert_eq!(rows[1].folded, Some(2), "the divider lost the dropped count");
    }

    #[test]
    fn a_legacy_row_still_renders() {
        ready("legacy");
        let raw = format!("legacy\n{{\"role\":\"user\",\"text\":\"card {CARD}\"}}\n");
        std::fs::write(path_for("legacy"), crypto::seal(raw.as_bytes()).unwrap()).unwrap();
        let rows = render(&records(&path_for("legacy")), Some(&redact::Redactor::new()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, format!("card {CARD}"));
        assert!(
            matches!(records(&path_for("legacy")).first(), Some(Record::Legacy { .. })),
            "an old row must not read as a wire row"
        );
    }

    #[test]
    fn an_unsaved_run_writes_no_file() {
        ready("unsaved");
        let det = redact::Redactor::new();
        record_send(&to(leak::UNSAVED), &det, &[row("user", "secret")], &HashMap::new());
        assert!(files().is_empty(), "a temporary conversation reached the disk");
    }

    #[test]
    fn the_agent_cannot_read_a_transcript() {
        ready("refusal");
        record_send(&to("refusal"), &redact::Redactor::new(), &[row("user", "secret")], &HashMap::new());
        let path = path_for("refusal");
        let sealed = std::fs::read(&path).unwrap();
        let kept = crypto::open_store_bytes(&path, sealed.clone()).unwrap();
        assert_eq!(kept, sealed, "read_file would hand the record to the agent");
    }
}
