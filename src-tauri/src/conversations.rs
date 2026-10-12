//! The conversation record: one sealed file per conversation, and every line is
//! evidence of what a receiver got.
//!
//! `docs/roadmap/REDACTION.md`, section "On-disk truth and the console
//! indicator", is the design. The send path writes this file, because the send
//! path is the only place that holds the text as it went out. The console holds
//! the text after `redact::Redactor::detokenize`, so the console cannot record
//! what it was never given.
//!
//! The redaction scan reads this file. A value that appears in a `sent` line
//! reached a receiver, so the scan needs no guess about which turn sent what.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto;
use crate::log::home_dir;
use crate::redact;
use crate::report::ToolReport;
use crate::scan;

/// The receiver of one send. The record names where each row went, so a later
/// scan can state the provider, the model, and the address without a guess.
#[derive(Clone, Debug)]
pub(crate) struct Send {
    pub conversation: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
}

/// Unix seconds, the unit of `at`. No clock crate lives here.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

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
    Ledger {
        at: u64,
        call_id: String,
        report: ToolReport,
    },
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
    Legacy {
        role: String,
        text: String,
        report: Option<ToolReport>,
    },
}

/// A row the console renders, with the redactions of that row. `load_conversation`
/// detokenizes the stored text, because the console shows real values and the file
/// keeps placeholders. The redactions come from the stored wire copy, so a row
/// names its own values and no other row's.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisplayRow {
    pub(crate) role: String,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) report: Option<ToolReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) folded: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fold_at: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) redactions: Vec<scan::Redaction>,
    pub(crate) at: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) provider: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tool_name: Option<String>,
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

pub(crate) fn path_for(name: &str) -> PathBuf {
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
    *counts
        .entry(row_key(msg, 0))
        .and_modify(|n| *n += 1)
        .or_insert(0)
}

fn parse_records(raw: &str) -> (String, Vec<Record>) {
    let mut lines = raw.lines();
    let name = lines.next().unwrap_or("Untitled").trim().to_string();
    let name = if name.is_empty() {
        "Untitled".to_string()
    } else {
        name
    };
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
            records.push(Record::Legacy {
                role: old.role,
                text: old.text,
                report: old.report,
            });
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
    let plain = if crypto::looks_sealed(&bytes) {
        crypto::open(&bytes)?
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&plain).to_string())
}

/// The name and every line of one conversation. A file that will not open
/// reads as nothing, and the writer then leaves the file as it is.
fn read_conversation(path: &std::path::Path) -> Option<(String, Vec<Record>)> {
    if !path.exists() {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        return Some((name, Vec::new()));
    }
    match read_text(path) {
        Ok(raw) => Some(parse_records(&raw)),
        Err(why) => {
            crate::log::log(&format!(
                "CONV: {} will not open ({why}); leaving it as it is",
                path.display()
            ));
            None
        }
    }
}

fn write_conversation(name: &str, records: &[Record]) -> Result<(), String> {
    std::fs::create_dir_all(conv_dir()).map_err(|e| e.to_string())?;
    let path = path_for(name);
    std::fs::write(
        &path,
        crypto::seal(serialize_records(name, records).as_bytes())?,
    )
    .map_err(|e| e.to_string())
}

/// Append the rows of `wire` the file does not hold yet.
///
/// A row goes on disk the first time it leaves, so the re-send of a whole
/// window on a tool turn adds nothing. `carriers` names the tool behind a tool
/// row, because the wire row carries only the call id. The writer scans nothing:
/// the redaction scan reads these rows when the console or the report asks.
pub(crate) fn record_send(
    to: &Send,
    wire: &[serde_json::Value],
    carriers: &HashMap<String, String>,
) {
    if to.conversation.is_empty() {
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
    let at = now();
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

/// Store the rows one fold dropped, and the brief that replaced them. The leak
/// report reads the dropped rows from this line, because the fold sent them to
/// the summarizer.
pub(crate) fn record_fold(to: &Send, brief: &str, dropped: &[serde_json::Value]) {
    if to.conversation.is_empty() {
        return;
    }
    let path = path_for(&to.conversation);
    let Some((name, mut records)) = read_conversation(&path) else {
        return;
    };
    records.push(Record::Fold {
        at: now(),
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
pub(crate) fn record_ledger(to: &Send, call_id: &str, report: &ToolReport) {
    if to.conversation.is_empty() {
        return;
    }
    let path = path_for(&to.conversation);
    let Some((name, mut records)) = read_conversation(&path) else {
        return;
    };
    records.push(Record::Ledger {
        at: now(),
        call_id: call_id.to_string(),
        report: report.clone(),
    });
    if let Err(why) = write_conversation(&name, &records) {
        crate::log::log(&format!("CONV: lost a ledger row for {name}: {why}"));
    }
}

/// Every conversation file, hidden ones included. The leak report reads every
/// conversation, so a conversation the list hides stays auditable.
pub(crate) fn files() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(conv_dir()) else {
        return Vec::new();
    };
    dir.flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("chat"))
        .collect()
}

/// The conversations the user deleted. A delete hides the transcript instead of
/// removing it, because the leak report reads the rows a hidden transcript holds.
/// The file lives in the conversation directory, whose `.chat` filter keeps it
/// out of `files`.
pub(crate) const HIDDEN_LIST: &str = "hidden.json";

fn hidden_file() -> PathBuf {
    conv_dir().join(HIDDEN_LIST)
}

fn hidden_paths() -> HashSet<String> {
    let Ok(bytes) = std::fs::read(hidden_file()) else {
        return HashSet::new();
    };
    let plain = if crypto::looks_sealed(&bytes) {
        crypto::open(&bytes).unwrap_or_default()
    } else {
        bytes
    };
    serde_json::from_slice(&plain).unwrap_or_default()
}

/// A transcript the conversation list hides: one the user deleted, or the one a
/// temporary conversation wrote.
pub(crate) fn is_hidden(path: &std::path::Path) -> bool {
    let temp = path.file_stem().and_then(|stem| stem.to_str()) == Some(scan::TEMPORARY);
    temp || hidden_paths().contains(&path.to_string_lossy().to_string())
}

fn hide(path: &std::path::Path) -> Result<(), String> {
    let mut hidden = hidden_paths();
    hidden.insert(path.to_string_lossy().to_string());
    let json = serde_json::to_vec(&hidden).map_err(|e| e.to_string())?;
    let file = hidden_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, crypto::seal(&json)?).map_err(|e| e.to_string())
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
    read_conversation(path)
        .map(|(_, records)| records)
        .unwrap_or_default()
}

fn push_row(out: &mut Vec<DisplayRow>, row: DisplayRow) {
    let text = row.text.trim().to_string();
    if text.is_empty() {
        return;
    }
    out.push(DisplayRow { text, ..row });
}

/// The dropped rows of one fold, as `Sent` records, so the fold view and the
/// leak report read them the same way.
pub(crate) fn fold_rows(
    at: u64,
    provider: &str,
    model: &str,
    base_url: &str,
    dropped: &[serde_json::Value],
) -> Vec<Record> {
    dropped
        .iter()
        .map(|msg| Record::Sent {
            at,
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: base_url.to_string(),
            tool: None,
            msg: msg.clone(),
        })
        .collect()
}

/// The rows one record list renders, in the order the record list holds them.
/// `Some(det)` turns the handles back into the values the console shows and
/// reports each row's own redactions. `None` renders the wire copy and reports
/// nothing, so the fold view shows the placeholders the receiver saw.
pub(crate) fn render(
    records: &[Record],
    mut det: Option<&mut redact::Redactor>,
) -> Vec<DisplayRow> {
    let mut calls: HashMap<String, ToolReport> = HashMap::new();
    for record in records {
        if let Record::Ledger {
            call_id, report, ..
        } = record
        {
            calls.insert(call_id.clone(), report.clone());
        }
    }
    let mut out: Vec<DisplayRow> = Vec::new();
    for record in records {
        match record {
            Record::Sent {
                at,
                provider,
                model,
                base_url,
                tool,
                msg,
            } => {
                let Some(role) = msg.get("role").and_then(|r| r.as_str()) else {
                    continue;
                };
                if role == "system" {
                    // The console shows the brief on the fold divider, and the
                    // instruction set lives in `prompts/AGENTS.md`, not here.
                    continue;
                }
                let wire = row_text(msg);
                let mut redactions: Vec<scan::Redaction> = Vec::new();
                let text = match det.as_deref_mut() {
                    Some(det) => {
                        redactions.extend(scan::entries(&wire, det));
                        // A tool call's arguments left on this row, so their
                        // values are this row's redactions too.
                        for call in msg
                            .get("tool_calls")
                            .and_then(|c| c.as_array())
                            .into_iter()
                            .flatten()
                        {
                            if let Some(args) =
                                call.pointer("/function/arguments").and_then(|a| a.as_str())
                            {
                                redactions.extend(scan::entries(args, det));
                            }
                        }
                        det.detokenize(&wire)
                    }
                    None => wire,
                };
                let report = msg
                    .get("tool_call_id")
                    .and_then(|id| id.as_str())
                    .and_then(|id| calls.remove(id));
                push_row(
                    &mut out,
                    DisplayRow {
                        role: role.to_string(),
                        text,
                        report,
                        folded: None,
                        fold_at: None,
                        redactions,
                        at: *at,
                        provider: provider.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                        tool_name: tool.clone(),
                    },
                );
            }
            Record::Fold {
                at,
                provider,
                model,
                base_url,
                brief,
                dropped,
            } => {
                let text = match det.as_deref_mut() {
                    Some(det) => det.detokenize(brief),
                    None => brief.clone(),
                };
                push_row(
                    &mut out,
                    DisplayRow {
                        role: "summary".to_string(),
                        text,
                        report: None,
                        folded: Some(dropped.len()),
                        fold_at: Some(*at),
                        // The brief arrived from the summarizer, so no send of
                        // ours carried its text. The dropped rows are the sends.
                        redactions: Vec::new(),
                        at: *at,
                        provider: provider.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                        tool_name: None,
                    },
                );
            }
            // An earlier build saved display text, so no scan may claim a value
            // in this row left the machine.
            Record::Legacy { role, text, report } => push_row(
                &mut out,
                DisplayRow {
                    role: role.clone(),
                    text: text.clone(),
                    report: report.clone(),
                    folded: None,
                    fold_at: None,
                    redactions: Vec::new(),
                    at: 0,
                    provider: String::new(),
                    model: String::new(),
                    base_url: String::new(),
                    tool_name: None,
                },
            ),
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
        if is_hidden(&path) {
            continue;
        }
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
            name: if name.is_empty() {
                "Untitled".into()
            } else {
                name.to_string()
            },
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
pub(crate) fn load_conversation(
    app: tauri::AppHandle,
    path: String,
) -> Result<Vec<DisplayRow>, String> {
    let full = resolve_conv_path(&path)?;
    let records = records(&full);
    let name = name_of(&full);
    Ok(scan::render_rows(&app, &name, &records))
}

/// The rows one fold dropped, for the console to show read-only.
#[tauri::command]
pub(crate) fn open_fold(conversation: String, at: u64) -> Result<Vec<DisplayRow>, String> {
    let path = path_for(&conversation);
    if !path.is_file() {
        return Err(format!("no conversation named {conversation}"));
    }
    for record in records(&path) {
        if let Record::Fold {
            at: fold_at,
            provider,
            model,
            base_url,
            dropped,
            ..
        } = record
        {
            if fold_at == at {
                return Ok(render(
                    &fold_rows(at, &provider, &model, &base_url, &dropped),
                    None,
                ));
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
            if let Record::Fold {
                at,
                dropped,
                provider,
                model,
                base_url,
                ..
            } = record
            {
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

/// Hide one conversation. The transcript stays, because the leak report reads
/// what a hidden transcript holds, and the conversation list drops the row.
#[tauri::command]
pub(crate) fn delete_conversation(path: String) -> Result<(), String> {
    let full = resolve_conv_path(&path)?;
    crate::log::log(&format!("CONV HIDE {}", full.display()));
    hide(&full)
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

    fn to(name: &str) -> Send {
        Send {
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
        assert!(
            wire_text.contains("⟦CARD_1⟧"),
            "the pass issued no handle: {wire_text}"
        );
        record_send(&to("wire"), &[row("user", &wire_text)], &HashMap::new());
        assert_eq!(
            sent("wire"),
            vec![wire_text.clone()],
            "the file must hold the wire text"
        );
        let rows = render(&records(&path_for("wire")), Some(&mut det));
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].text.contains(CARD),
            "the console lost the value: {}",
            rows[0].text
        );
        assert_eq!(
            rows[0].redactions.len(),
            1,
            "the row must name what it carried"
        );
        assert!(
            !rows[0].redactions[0].is_leaked,
            "a row that carried only the placeholder sent nothing in the clear"
        );
        let wire_rows = render(&records(&path_for("wire")), None);
        assert_eq!(
            wire_rows[0].text,
            wire_text.clone(),
            "the wire view dropped the handle"
        );
    }

    #[test]
    fn a_row_that_carried_the_value_in_the_clear_is_a_leak() {
        ready("clear");
        record_send(
            &to("clear"),
            &[row("user", &format!("pay {CARD} now"))],
            &HashMap::new(),
        );
        let mut det = redact::Redactor::new();
        let rows = render(&records(&path_for("clear")), Some(&mut det));
        assert_eq!(
            rows[0].redactions.len(),
            1,
            "the row must name the value that left"
        );
        assert!(
            rows[0].redactions[0].is_leaked,
            "the wire copy carried the value in the clear"
        );
        assert_eq!(rows[0].redactions[0].class, "CARD");
    }

    #[test]
    fn a_resend_of_the_same_window_adds_no_row() {
        ready("resend");
        let first = vec![row("user", "find the file")];
        record_send(&to("resend"), &first, &HashMap::new());
        let second = vec![row("user", "find the file"), row("tool", "payroll.csv")];
        let mut carriers = HashMap::new();
        carriers.insert("c1".to_string(), "list_dir".to_string());
        record_send(&to("resend"), &second, &carriers);
        record_send(&to("resend"), &second, &carriers);
        assert_eq!(
            sent("resend").len(),
            2,
            "a re-send wrote a second copy of a row"
        );
    }

    #[test]
    fn two_identical_user_rows_both_stay() {
        ready("twice");
        let wire = vec![
            row("user", "yes"),
            row("assistant", "done"),
            row("user", "yes"),
        ];
        record_send(&to("twice"), &wire, &HashMap::new());
        record_send(&to("twice"), &wire, &HashMap::new());
        assert_eq!(sent("twice"), vec!["yes", "done", "yes"]);
    }

    #[test]
    fn a_fold_line_holds_the_rows_a_fold_dropped() {
        ready("fold");
        record_send(&to("fold"), &[row("user", "the task")], &HashMap::new());
        record_fold(
            &to("fold"),
            "the brief",
            &[row("user", "one"), row("tool", "two")],
        );
        let rows = render(&records(&path_for("fold")), None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].role, "summary");
        assert_eq!(
            rows[1].folded,
            Some(2),
            "the divider lost the dropped count"
        );
    }

    #[test]
    fn a_legacy_row_still_renders() {
        ready("legacy");
        let raw = format!("legacy\n{{\"role\":\"user\",\"text\":\"card {CARD}\"}}\n");
        std::fs::write(path_for("legacy"), crypto::seal(raw.as_bytes()).unwrap()).unwrap();
        let rows = render(&records(&path_for("legacy")), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, format!("card {CARD}"));
        assert!(
            matches!(
                records(&path_for("legacy")).first(),
                Some(Record::Legacy { .. })
            ),
            "an old row must not read as a wire row"
        );
    }

    #[test]
    fn a_temporary_transcript_stays_out_of_the_list() {
        ready("temp");
        record_send(
            &to(scan::TEMPORARY),
            &[row("user", "secret")],
            &HashMap::new(),
        );
        assert_eq!(
            files().len(),
            1,
            "a temporary run must reach the disk, so the report can read it"
        );
        assert!(
            is_hidden(&path_for(scan::TEMPORARY)),
            "the list must hide a temporary run"
        );
        assert!(
            list_conversations().is_empty(),
            "the list showed a temporary run"
        );
    }

    #[test]
    fn a_hidden_transcript_still_reads() {
        ready("hidden");
        record_send(&to("hidden"), &[row("user", "secret")], &HashMap::new());
        hide(&path_for("hidden")).unwrap();
        assert!(
            list_conversations().is_empty(),
            "the list showed a deleted conversation"
        );
        assert_eq!(
            sent("hidden"),
            vec!["secret"],
            "the report lost the deleted transcript"
        );
    }

    #[test]
    fn the_agent_cannot_read_a_transcript() {
        ready("refusal");
        record_send(&to("refusal"), &[row("user", "secret")], &HashMap::new());
        let path = path_for("refusal");
        let sealed = std::fs::read(&path).unwrap();
        let kept = crypto::open_store_bytes(&path, sealed.clone()).unwrap();
        assert_eq!(kept, sealed, "read_file would hand the record to the agent");
    }
}
