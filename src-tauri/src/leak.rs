//! The leak report: one row per value that left the machine in plaintext.
//! `docs/roadmap/REDACTION.md`, section "Leak report", is the design.
//!
//! The report holds real values, so the file carries the `FMENC1` envelope and
//! is never written in plaintext, not even for the length of one write. The
//! agent cannot read it back: `crypto::open_store_bytes` refuses this file the
//! way it refuses the token map, because a report the agent can open is a report
//! the agent can send to a provider.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::conversations::Record;
use crate::redact;

/// The report file, beside `settings.json`.
pub(crate) const LEAKS: &str = "leaks.jsonl";

/// The name of a conversation the user never saved. The console has that state,
/// and a report row with no name names no conversation, so the send records this
/// name instead of a blank.
pub(crate) const UNSAVED: &str = "(unsaved)";

/// One leak: one value, one send. A value that leaves again on a later turn is a
/// second row, because the second send is a second fact an auditor counts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeakRow {
    /// Unix seconds at the send.
    pub at: u64,
    /// The conversation name, exactly as `conversations.rs` saves it.
    pub conversation: String,
    /// The leaked value, verbatim. The report is an exact record, not a redacted
    /// one, and the user owns its security once First Mate writes it.
    pub value: String,
    /// `redact::Class::label()`.
    pub class: String,
    /// The char index of the value in the row text as it stood at the send.
    pub start: usize,
    /// One past the value, in the same count.
    pub end: usize,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// The tool whose result or call arguments carried the value, when a tool
    /// carried it. The range counts inside that tool's text, not the message.
    pub tool: Option<String>,
}

/// The marker the console draws over a leaked range. The marker is render-time
/// text: literal marker text in a row would tell the receiver the value leaked.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LeakMark {
    pub value: String,
    pub class: String,
    pub start: usize,
    pub end: usize,
    pub at: u64,
    pub provider: String,
    pub model: String,
}

/// The receiver of one send. Every row that send records carries these fields,
/// because the report names where each value went. The fields are owned: the
/// provider config takes the model by value on the way out, and the report keeps
/// its own copy of what the send carried.
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

/// Read-decrypt-append-re-seal is one step, so two turns that leak in the same
/// second cannot drop one another's row.
static REPORT: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    REPORT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Append one row. The file opens, gains its line, and goes back sealed inside
/// one lock, so no plaintext copy of the report reaches the disk.
pub(crate) fn record(row: &LeakRow) {
    let _guard = lock();
    let path = store();
    let mut plain = match std::fs::read(&path) {
        // A report that will not open stays as it is. Rewriting it with one new
        // row would destroy the record the report exists to keep.
        Ok(bytes) if crate::crypto::looks_sealed(&bytes) => match crate::crypto::open(&bytes) {
            Ok(plain) => plain,
            Err(why) => {
                crate::log::log(&format!("LEAK REPORT: {} will not open ({why}); leaving it as it is", path.display()));
                return;
            }
        },
        // A plaintext report is a worse state than an unwritten row, so a stray
        // plaintext file is sealed on the way past.
        Ok(plain) => plain,
        Err(_) => Vec::new(),
    };
    let Ok(line) = serde_json::to_string(row) else {
        return;
    };
    if !plain.is_empty() && plain.last() != Some(&b'\n') {
        plain.push(b'\n');
    }
    plain.extend_from_slice(line.as_bytes());
    plain.push(b'\n');
    let sealed = crate::crypto::seal(&plain)
        .and_then(|bytes| {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&path, bytes).map_err(|e| e.to_string())
        })
        .map_err(|e| {
            // The envelope is the only way this file may exist, so a machine
            // with no key loses the row and says so in the debug log.
            crate::log::log(&format!("LEAK REPORT: lost a row ({e})"));
            e
        });
    let _ = sealed;
}

/// Every row of the report, oldest first.
pub(crate) fn all() -> Vec<LeakRow> {
    let _guard = lock();
    let path = store();
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let plain = if crate::crypto::looks_sealed(&bytes) {
        match crate::crypto::open(&bytes) {
            Ok(plain) => plain,
            Err(why) => {
                crate::log::log(&format!("LEAK REPORT: {} will not open ({why})", path.display()));
                return Vec::new();
            }
        }
    } else {
        bytes
    };
    String::from_utf8_lossy(&plain)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The leaked ranges of one conversation, for the console to color red.
pub(crate) fn marks(conversation: &str) -> Vec<LeakMark> {
    all()
        .into_iter()
        .filter(|row| row.conversation == conversation)
        .map(|row| LeakMark {
            value: row.value,
            class: row.class,
            start: row.start,
            end: row.end,
            at: row.at,
            provider: row.provider,
            model: row.model,
        })
        .collect()
}

/// Record every value one text of one send carried in the clear.
///
/// `text` is the text that goes on the wire, so a token the redaction pass
/// issued names no value here, and only a value the pass left in place is
/// recorded. A provider whose `Hide sensitive information` switch is off sends
/// the real text, so the detector finds every value in it.
pub(crate) fn record_send(to: &Send, det: &redact::Redactor, text: &str, tool: Option<&str>) {
    for row in rows(to, det, text, tool) {
        det.mark_left(&row.value);
        record(&row);
    }
}

/// The values one wire row carries in the clear: its own text, plus the
/// arguments of each tool call it makes. The console links a range back to the
/// text that held it, so each text is scanned as itself.
fn rows(
    to: &Send,
    det: &redact::Redactor,
    text: &str,
    tool: Option<&str>,
) -> Vec<LeakRow> {
    let at = now();
    det.scan(text)
        .into_iter()
        .map(|found| LeakRow {
            at,
            conversation: to.conversation.clone(),
            value: found.value,
            class: found.class.to_string(),
            start: found.start,
            end: found.end,
            provider: to.provider.clone(),
            model: to.model.clone(),
            base_url: to.base_url.clone(),
            tool: tool.map(str::to_string),
        })
        .collect()
}

/// Every value one wire row carried in the clear. `carrier` names the tool that
/// produced a `tool` row, because the row itself holds only the call id.
fn msg_rows(
    to: &Send,
    det: &redact::Redactor,
    msg: &serde_json::Value,
    carrier: Option<&str>,
) -> Vec<LeakRow> {
    let mut out: Vec<LeakRow> = Vec::new();
    let text = crate::conversations::row_text(msg);
    if !text.is_empty() {
        out.extend(rows(to, det, &text, carrier));
    }
    let calls = msg.get("tool_calls").and_then(|c| c.as_array());
    for call in calls.into_iter().flatten() {
        let name = call.pointer("/function/name").and_then(|n| n.as_str());
        if let Some(args) = call.pointer("/function/arguments").and_then(|a| a.as_str()) {
            out.extend(rows(to, det, args, name));
        }
    }
    out
}

/// Record every value one wire row carried in the clear.
pub(crate) fn record_msg(
    to: &Send,
    det: &redact::Redactor,
    msg: &serde_json::Value,
    carrier: Option<&str>,
) {
    for row in msg_rows(to, det, msg, carrier) {
        det.mark_left(&row.value);
        record(&row);
    }
}

/// Record every value the rows of one send carried in the clear. The fold sends
/// the rows it drops to the summarizer, so the scan runs over them before that
/// call. `carriers` names the tool behind a tool-result row.
pub(crate) fn record_rows(
    to: &Send,
    det: &redact::Redactor,
    rows: &[serde_json::Value],
    carriers: &HashMap<String, String>,
) {
    for row in rows {
        let carrier = row
            .get("tool_call_id")
            .and_then(|id| id.as_str())
            .and_then(|id: &str| carriers.get(id))
            .map(String::as_str);
        record_msg(to, det, row, carrier);
    }
}

/// The row's identity in the report, so a rescan adds no second copy of a fact
/// the report already holds.
fn key(row: &LeakRow) -> (String, String, String, usize, usize, Option<String>) {
    (
        row.conversation.clone(),
        row.value.clone(),
        row.class.clone(),
        row.start,
        row.end,
        row.tool.clone(),
    )
}

/// The values the report already names. `redact::Redactor::seed_left` loads
/// them, so a later run's chip can name what an earlier send cannot undo.
pub(crate) fn left_values() -> Vec<String> {
    let mut values: Vec<String> = all().into_iter().map(|row| row.value).collect();
    values.sort();
    values.dedup();
    values
}

/// The declared set the last rescan covered, so one change costs one rescan.
pub(crate) const SCAN: &str = "scan.json";

fn fingerprint(values: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for value in values {
        value.hash(&mut hasher);
    }
    hasher.finish()
}

fn read_fingerprint(path: &std::path::Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let plain = if crate::crypto::looks_sealed(&bytes) {
        crate::crypto::open(&bytes).ok()?
    } else {
        bytes
    };
    serde_json::from_str::<u64>(&String::from_utf8_lossy(&plain)).ok()
}

/// Scan every stored row of every conversation with the declared values, and
/// return the rows this scan added.
///
/// A new declared value is the only event that makes an already-read row worth
/// reading again, so an ordinary turn scans its new rows alone in
/// `conversations::record_send` and never re-reads the file.
pub(crate) fn rescan(values: &[String], scan_file: &std::path::Path) -> usize {
    if values.is_empty() {
        return 0;
    }
    let want = fingerprint(values);
    if read_fingerprint(scan_file) == Some(want) {
        return 0;
    }
    let mut det = redact::Redactor::new();
    for value in values {
        det.seed(value);
    }
    let mut seen: HashSet<_> = all().iter().map(key).collect();
    let mut added = 0usize;
    for path in crate::conversations::files() {
        let conversation = crate::conversations::name_of(&path);
        let stored = crate::conversations::records(&path);
        // A fold line holds a tool row, and that row names only its call id.
        // The sent line of the same call id names the tool that ran.
        let mut carriers: HashMap<String, String> = HashMap::new();
        for line in &stored {
            if let Record::Sent { tool: Some(name), msg, .. } = line {
                if let Some(id) = msg.get("tool_call_id").and_then(|i| i.as_str()) {
                    carriers.insert(id.to_string(), name.clone());
                }
            }
        }
        for line in &stored {
            let own = match line {
                Record::Sent { tool, .. } => tool.as_deref(),
                _ => None,
            };
            let (to, msgs): (Send, &[serde_json::Value]) = match line {
                Record::Sent { provider, model, base_url, msg, .. } => (
                    Send {
                        conversation: conversation.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                    },
                    std::slice::from_ref(msg),
                ),
                Record::Fold { provider, model, base_url, dropped, .. } => (
                    Send {
                        conversation: conversation.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                    },
                    dropped.as_slice(),
                ),
                _ => continue,
            };
            for msg in msgs {
                let carrier = msg
                    .get("tool_call_id")
                    .and_then(|id| id.as_str())
                    .and_then(|id| carriers.get(id))
                    .map(String::as_str)
                    .or(own);
                for row in msg_rows(&to, &det, msg, carrier) {
                    if seen.insert(key(&row)) {
                        record(&row);
                        added += 1;
                    }
                }
            }
        }
    }
    if let Ok(sealed) = crate::crypto::seal(format!("{want}").as_bytes()) {
        if let Some(dir) = scan_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(scan_file, sealed);
    }
    added
}

/// Rescan the stored rows when the declared values changed. `lib.rs` runs this
/// on startup and before a send, because the user edits `entity_registry.json`
/// outside First Mate.
pub(crate) fn rescan_now(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_config_dir() else { return };
    let values = crate::settings::registry(app);
    let added = rescan(&values, &dir.join(SCAN));
    if added > 0 {
        crate::log::log(&format!(
            "LEAK RESCAN: {added} stored rows carried a value the registry declares"
        ));
    }
}

/// The report file. `crypto::is_store` covers this directory, which is why
/// `crypto::open_store_bytes` needs its refusal by name.
fn store() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_STORE.with(|slot| slot.borrow().clone()) {
        return path;
    }
    crate::log::fm_dir().join(LEAKS)
}

#[cfg(test)]
thread_local! {
    /// A test writes its own report, because the real one holds real values.
    static TEST_STORE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Point this thread's report at a fresh file of its own.
#[cfg(test)]
pub(crate) fn use_store(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fm-leak-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(LEAKS);
    let _ = std::fs::remove_file(&path);
    TEST_STORE.with(|slot| *slot.borrow_mut() = Some(path.clone()));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "4111 1111 1111 1111";
    const EMAIL: &str = "dusti@example.com";

    fn send(conversation: &str) -> Send {
        Send {
            conversation: conversation.to_string(),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            base_url: "http://127.0.0.1:1234/v1".to_string(),
        }
    }

    fn row(conversation: &str, value: &str, class: &str) -> LeakRow {
        LeakRow {
            at: 1_760_000_000,
            conversation: conversation.to_string(),
            value: value.to_string(),
            class: class.to_string(),
            start: 4,
            end: 4 + value.chars().count(),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            tool: Some("read_file".to_string()),
        }
    }

    #[test]
    fn a_recorded_row_round_trips_through_the_sealed_file() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        let path = use_store("round-trip");
        record(&row("payroll", EMAIL, "EMAIL"));
        let on_disk = std::fs::read(&path).unwrap();
        assert!(crate::crypto::looks_sealed(&on_disk), "the report must be sealed");
        let text = String::from_utf8_lossy(&on_disk);
        assert!(!text.contains(EMAIL), "the file holds the value in plaintext");
        assert!(!text.contains("payroll"), "the file names the conversation in plaintext");
        let rows = all();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], row("payroll", EMAIL, "EMAIL"));
    }

    #[test]
    fn two_appends_keep_both_rows() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("append");
        record(&row("payroll", EMAIL, "EMAIL"));
        record(&row("payroll", CARD, "CARD"));
        let rows = all();
        assert_eq!(rows.len(), 2, "an append must not drop the earlier row");
        assert_eq!(rows[0].value, EMAIL);
        assert_eq!(rows[1].value, CARD);
    }

    #[test]
    fn marks_name_only_the_requested_conversation() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("marks");
        record(&row("payroll", EMAIL, "EMAIL"));
        record(&row("lease", CARD, "CARD"));
        let paid = marks("payroll");
        assert_eq!(paid.len(), 1, "another conversation's leak is not mine");
        assert_eq!(paid[0].value, EMAIL);
        assert_eq!(paid[0].class, "EMAIL");
        assert_eq!(paid[0].start, 4);
        assert_eq!(paid[0].end, 4 + EMAIL.chars().count());
        assert_eq!(paid[0].at, 1_760_000_000);
        assert_eq!(paid[0].provider, "openai");
        assert_eq!(paid[0].model, "gpt-4o");
        assert!(marks("nothing here").is_empty());
    }

    #[test]
    fn a_send_with_the_switch_off_records_the_receiver() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("switch-off");
        let det = redact::Redactor::new();
        // `Zahlungskarte ✓ ` is 16 characters and 18 bytes, so a byte count
        // would show a different start.
        record_send(&send("payroll"), &det, &format!("Zahlungskarte ✓ {CARD}"), None);
        let rows = all();
        assert_eq!(rows.len(), 1, "a value that left is one row");
        assert_eq!(rows[0].value, CARD);
        assert_eq!(rows[0].class, "CARD");
        assert_eq!(rows[0].start, 16, "the range counts chars");
        assert_eq!(rows[0].end, 35);
        assert_eq!(rows[0].provider, "openai");
        assert_eq!(rows[0].model, "gpt-4o");
        assert_eq!(rows[0].base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(rows[0].conversation, "payroll");
        assert!(rows[0].tool.is_none());
        assert!(rows[0].at > 1_700_000_000, "the row carries the send time");
    }

    #[test]
    fn the_same_value_on_a_later_turn_gains_a_second_row() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("re-send");
        let det = redact::Redactor::new();
        let to = send("payroll");
        record_send(&to, &det, &format!("pay {CARD} now"), None);
        record_send(&to, &det, &format!("pay {CARD} now"), None);
        assert_eq!(all().len(), 2, "every send that carries the value is a row");
    }

    #[test]
    fn a_tokenized_send_gains_no_row_and_a_clear_send_gains_one() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("tokenized");
        let mut det = redact::Redactor::new();
        let to = send("payroll");
        // The switch is on, so the wire carries the token and the value stays
        // on this machine.
        let wire = det.tokenize(&format!("pay {CARD} now"));
        record_send(&to, &det, &wire, None);
        assert!(all().is_empty(), "a replaced value never left");
        // A provider whose switch is off gets the real text, so that send is
        // the leak, and the later chip must name it.
        record_send(&to, &det, &format!("pay {CARD} now"), None);
        let rows = all();
        assert_eq!(rows.len(), 1, "the clear send is the leak");
        assert_eq!(rows[0].value, CARD);
        det.tokenize(&format!("pay {CARD} now"));
        assert_eq!(det.leaks(), vec!["CARD"], "the chip lost the earlier leak");
    }

    #[test]
    fn the_report_opens_for_no_one() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        let sealed = crate::crypto::seal(format!("the value is {CARD}").as_bytes()).unwrap();
        let bytes = crate::crypto::open_store_bytes(&crate::log::fm_dir().join(LEAKS), sealed.clone())
            .expect("a sealed store file is not an error");
        assert_eq!(bytes, sealed, "the agent read the report back in plaintext");
        // The refusal names the file, not the directory: a spill beside it stays
        // readable, so `read_file` still reaches a tool's output.
        let neighbour = crate::crypto::open_store_bytes(
            &crate::log::fm_dir().join("req-0-t0.json"),
            sealed.clone(),
        )
        .unwrap();
        assert_eq!(neighbour, format!("the value is {CARD}").as_bytes());
    }

    /// The point of the stored transcript: a value the user declares later is
    /// still in the row that went out weeks earlier.
    #[test]
    fn a_value_declared_after_a_send_is_found_in_the_stored_row() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        crate::crypto::init(std::env::temp_dir().join("fm-envelope"));
        use_store("declared");
        crate::conversations::use_dir("declared");
        let declared = "AC-9931-X";
        let det = redact::Redactor::new();
        crate::conversations::record_send(
            &send("acct"),
            &det,
            &[serde_json::json!({ "role": "user", "content": format!("the invoice is {declared}") })],
            &std::collections::HashMap::new(),
        );
        assert!(all().is_empty(), "a value nobody declared was recorded");
        let scan = std::env::temp_dir().join("fm-envelope").join(SCAN);
        let _ = std::fs::remove_file(&scan);
        assert_eq!(rescan(&[declared.to_string()], &scan), 1, "the rescan missed the stored row");
        assert_eq!(all()[0].value, declared);
        assert_eq!(all()[0].conversation, "acct");
        assert_eq!(rescan(&[declared.to_string()], &scan), 0, "one declared set rescanned twice");
        assert_eq!(all().len(), 1, "the second rescan duplicated a row");
    }
}
