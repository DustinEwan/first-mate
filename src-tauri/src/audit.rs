//! The console answers a flagged message. The gate holds the send while it waits.

use std::sync::Mutex;

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::log::log;

/// What you chose in the audit card. Each redaction is one exact string the
/// outbound copy must lose.
pub(crate) struct Answer {
    pub(crate) approved: bool,
    pub(crate) redactions: Vec<String>,
}

/// One card at a time, because the card takes the composer's place. Dropping the
/// sender ends the wait with no answer, which is the safe end: the send stays
/// held.
static PENDING: Mutex<Option<oneshot::Sender<Answer>>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<oneshot::Sender<Answer>>> {
    PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Ask the console about one flagged message. `windows` carries each scored
/// window's char range and score, so the card marks where the model saw
/// something. The wait has no timeout: containment must not depend on a clock.
/// `None` means the card closed with no answer, which states no verdict about
/// the message.
pub(crate) async fn ask(
    app: &tauri::AppHandle,
    message: &str,
    score: f64,
    windows: &[(usize, usize, f64)],
) -> Option<Answer> {
    let (tx, rx) = oneshot::channel();
    *lock() = Some(tx);
    let marked: Vec<serde_json::Value> = windows
        .iter()
        .map(|(start, end, p)| serde_json::json!({ "start": start, "end": end, "score": p }))
        .collect();
    let _ = app.emit(
        "audit",
        serde_json::json!({ "message": message, "score": score, "windows": marked }),
    );
    log(&format!("AUDIT: asked about a message at {score:.2}"));
    rx.await.ok()
}

/// Record the console's answer. `false` means no card was open, so a late answer
/// from a closed window cannot release a later message.
pub(crate) fn resolve(approved: bool, redactions: Vec<String>) -> bool {
    match lock().take() {
        Some(tx) => tx
            .send(Answer {
                approved,
                redactions,
            })
            .is_ok(),
        None => false,
    }
}

/// A stop ends the open wait with no answer. The gate holds the send, because no
/// answer states no verdict about the message.
pub(crate) fn cancel() {
    drop(lock().take());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_reaches_the_waiter_once() {
        let (tx, rx) = oneshot::channel();
        *lock() = Some(tx);
        assert!(resolve(true, vec!["0212 555 0134".to_string()]));
        let answer = rx.blocking_recv().unwrap();
        assert!(answer.approved);
        assert_eq!(answer.redactions, vec!["0212 555 0134".to_string()]);
        assert!(!resolve(true, vec![]), "no card is open twice");
    }

    #[test]
    fn a_cancel_ends_the_wait_with_no_answer() {
        let (tx, rx) = oneshot::channel();
        *lock() = Some(tx);
        cancel();
        assert!(
            rx.blocking_recv().is_err(),
            "a dropped sender must end the wait"
        );
    }
}
