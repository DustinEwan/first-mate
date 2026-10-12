//! The leak report: one row per value that left the machine in plaintext.
//! `docs/roadmap/REDACTION.md`, section "Leak report", is the design.
//!
//! The report holds real values, so the file carries the `FMENC1` envelope and
//! is never written in plaintext, not even for the length of one write. The
//! agent cannot read it back: `crypto::open_store_bytes` refuses this file the
//! way it refuses the token map, because a report the agent can open is a report
//! the agent can send to a provider.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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

/// Record every value one row of one send carries out of the machine.
///
/// `hide` is that provider's `Hide sensitive information` switch. With the
/// switch on, the send replaces the value, so only a value the Agent Model
/// already holds has actually left, and `redact::Redactor::scan` reports exactly
/// those. With the switch off, every value the detector finds leaves in
/// plaintext, and the report records every one of them.
pub(crate) fn record_send(to: &Send, det: &redact::Redactor, text: &str, tool: Option<&str>, hide: bool) {
    for found in det.scan(text) {
        if hide && !found.left {
            continue;
        }
        record(&LeakRow {
            at: now(),
            conversation: to.conversation.clone(),
            value: found.value,
            class: found.class.to_string(),
            start: found.start,
            end: found.end,
            provider: to.provider.clone(),
            model: to.model.clone(),
            base_url: to.base_url.clone(),
            tool: tool.map(str::to_string),
        });
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
        record_send(
            &send("payroll"),
            &det,
            &format!("Zahlungskarte ✓ {CARD}"),
            None,
            false,
        );
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
        record_send(&to, &det, &format!("pay {CARD} now"), None, false);
        record_send(&to, &det, &format!("pay {CARD} now"), None, false);
        assert_eq!(all().len(), 2, "every send that carries the value is a row");
    }

    #[test]
    fn a_redacted_value_gains_a_row_only_after_it_left() {
        if !crate::crypto::tpm_ready() {
            return;
        }
        use_store("switch-on");
        let mut det = redact::Redactor::new();
        let to = send("payroll");
        // The switch is on, and this send replaces the value, so nothing leaves.
        record_send(&to, &det, &format!("pay {CARD} now"), None, true);
        assert!(all().is_empty(), "a redacted value never left");
        // An earlier send carried the value in plaintext, so this send repeats a
        // leak that already happened.
        det.note_sent(std::iter::once(format!("I paid {CARD} last week")));
        record_send(&to, &det, &format!("pay {CARD} now"), None, true);
        record_send(&to, &det, &format!("pay {CARD} now"), None, true);
        assert_eq!(all().len(), 2, "each later send gains its own row");
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
}
