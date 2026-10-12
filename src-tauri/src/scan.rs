//! The redaction scan, and the leak report it generates on request.
//!
//! `docs/roadmap/REDACTION.md`, sections "Redaction scan" and "Leak report", is
//! the design. The scan covers the active conversation and keeps its result in
//! memory. The report covers every conversation, including one the list hides,
//! and reuses that cached result where one exists.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::conversations;
use crate::redact;

/// The conversation name for a run the user marked temporary. The transcript
/// exists, and `conversations::list_conversations` hides it, so a leak audit
/// still reads those rows.
pub(crate) const TEMPORARY: &str = "(temporary)";

/// One value one conversation row carried. `is_leaked` is true when the wire
/// copy held the value in the clear, so the receiver still has it. `placeholder`
/// is the marker the pass used, or the marker the scan issued for a leaked value.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Redaction {
    pub redacted_value: String,
    pub placeholder: String,
    pub is_leaked: bool,
    pub class: &'static str,
}

/// One leak report row: one value, one message that sent it in the clear.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportRow {
    pub at: u64,
    pub conversation: String,
    /// The transcript file, so a report row opens even when the conversation
    /// list hides that transcript.
    pub path: String,
    /// Index into the rows the console shows for that conversation, so a click
    /// scrolls to the message without matching any text.
    pub index: usize,
    /// The `at` of the fold that dropped the row, when a fold sent it.
    pub fold: Option<u64>,
    pub value: String,
    pub placeholder: String,
    pub class: String,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub tool: Option<String>,
}

/// The work the report has finished, for the progress bar.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Progress {
    pub done: usize,
    pub total: usize,
}

/// The redactions of one wire row: the placeholders the pass applied, and the
/// values it left in the clear. A left value with no placeholder in the map gains
/// one here, so every entry names a placeholder.
pub(crate) fn entries(wire: &str, det: &mut redact::Redactor) -> Vec<Redaction> {
    let mut out: Vec<Redaction> = Vec::new();
    for applied in det.applied(wire) {
        out.push(Redaction {
            redacted_value: applied.value,
            placeholder: applied.token,
            is_leaked: false,
            class: applied.class,
        });
    }
    for found in det.scan(wire) {
        let placeholder = match det.placeholder(&found.value) {
            Some(token) => token,
            None => det.mint(&found.value),
        };
        out.push(Redaction {
            redacted_value: found.value,
            placeholder,
            is_leaked: true,
            class: found.class,
        });
    }
    out
}

/// The detector the scan and the report read. The registry seeds it, because a
/// declared value has no shape the rules would match.
fn detector(app: &tauri::AppHandle) -> redact::Redactor {
    let mut det = redact::Redactor::new();
    for value in crate::settings::registry(app) {
        det.seed(&value);
    }
    det
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The rendered rows of one conversation, with the redactions of every row.
struct Cached {
    conversation: String,
    rows: usize,
    rendered: Vec<conversations::DisplayRow>,
}

fn cached(name: &str, rows: usize) -> Option<Vec<conversations::DisplayRow>> {
    let held = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    held.as_ref()
        .filter(|cached| cached.conversation == name && cached.rows == rows)
        .map(|cached| cached.rendered.clone())
}

/// The rows one conversation renders. The cached result serves the conversation
/// the scan holds at the same length, so a reopen costs no new pass over the
/// rules.
fn rows(
    app: &tauri::AppHandle,
    name: &str,
    records: &[conversations::Record],
) -> Vec<conversations::DisplayRow> {
    if let Some(rows) = cached(name, records.len()) {
        return rows;
    }
    conversations::render(records, Some(&mut detector(app)))
}

/// The rows of the active conversation, cached for the next open. One
/// conversation is cached at a time, so a move to another replaces the entry.
pub(crate) fn render_rows(
    app: &tauri::AppHandle,
    name: &str,
    records: &[conversations::Record],
) -> Vec<conversations::DisplayRow> {
    let rendered = rows(app, name, records);
    let mut held = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *held = Some(Cached {
        conversation: name.to_string(),
        rows: records.len(),
        rendered: rendered.clone(),
    });
    rendered
}

/// The leak rows of one rendered row.
fn leaks_of(
    name: &str,
    path: &str,
    index: usize,
    fold: Option<u64>,
    row: &conversations::DisplayRow,
) -> Vec<ReportRow> {
    row.redactions
        .iter()
        .filter(|redaction| redaction.is_leaked)
        .map(|redaction| ReportRow {
            at: row.at,
            conversation: name.to_string(),
            path: path.to_string(),
            index,
            fold,
            value: redaction.redacted_value.clone(),
            placeholder: redaction.placeholder.clone(),
            class: redaction.class.to_string(),
            provider: row.provider.clone(),
            model: row.model.clone(),
            base_url: row.base_url.clone(),
            tool: row.tool_name.clone(),
        })
        .collect()
}

/// The leak rows of one conversation: the stored rows, then the rows each fold
/// dropped and sent to the summarizer.
fn leaks_of_conversation(
    app: &tauri::AppHandle,
    name: &str,
    path: &str,
    records: &[conversations::Record],
    cached_rows: Vec<conversations::DisplayRow>,
) -> Vec<ReportRow> {
    let mut out: Vec<ReportRow> = Vec::new();
    for (index, row) in cached_rows.iter().enumerate() {
        out.extend(leaks_of(name, path, index, None, row));
    }
    let mut det = detector(app);
    for record in records {
        let conversations::Record::Fold {
            at,
            provider,
            model,
            base_url,
            dropped,
            ..
        } = record
        else {
            continue;
        };
        let rows = conversations::render(
            &conversations::fold_rows(*at, provider, model, base_url, dropped),
            Some(&mut det),
        );
        for (index, row) in rows.iter().enumerate() {
            out.extend(leaks_of(name, path, index, Some(*at), row));
        }
    }
    out
}

/// Every leak, from every conversation on disk, newest first. The report writes
/// no file: the transcripts and `token_map.json` are the record.
pub(crate) fn generate(app: &tauri::AppHandle) -> Vec<ReportRow> {
    let paths = conversations::files();
    let total = paths.len();
    let mut out: Vec<ReportRow> = Vec::new();
    for (done, path) in paths.iter().enumerate() {
        let _ = app.emit("report-progress", Progress { done, total });
        let name = conversations::name_of(path);
        let records = conversations::records(path);
        let rendered = rows(app, &name, &records);
        out.extend(leaks_of_conversation(
            app,
            &name,
            &path.display().to_string(),
            &records,
            rendered,
        ));
    }
    let _ = app.emit("report-progress", Progress { done: total, total });
    out.sort_by_key(|row| std::cmp::Reverse(row.at));
    out
}

/// The values this conversation sent in the clear. The chip names their classes
/// on every later turn, because no later send can recall them.
pub(crate) fn left_values(app: &tauri::AppHandle, name: &str) -> Vec<String> {
    let path = conversations::path_for(name);
    if !path.is_file() {
        return Vec::new();
    }
    let rows = rows(app, name, &conversations::records(&path));
    let mut values: Vec<String> = rows
        .iter()
        .flat_map(|row| row.redactions.iter())
        .filter(|redaction| redaction.is_leaked)
        .map(|redaction| redaction.redacted_value.clone())
        .collect();
    values.sort();
    values.dedup();
    values
}
