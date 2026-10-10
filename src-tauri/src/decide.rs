//! Client for the decision server the user runs.
//! `docs/roadmap/REDACTION.md` holds the API contract and the measured
//! operating points.

use std::sync::LazyLock;
use std::time::Duration;

use serde_json::{json, Value};

/// One `noul` question per span. A choice question over categories invites a
/// topic answer, and the measured model scores the topic, not the privacy of
/// the topic.
const QUESTION: &str = "Does this text contain potentially sensitive information?";

/// The server holds one window per span. Longer prose is scored on its head,
/// because the head carries the identity of a file.
const MAX_SPAN_CHARS: usize = 6000;

/// A server that answers at or above this on a benign span reports a floor,
/// not a probability. H2O's shim ships a 0.801 yes/no floor as shipped.
pub(crate) const FLOOR: f64 = 0.8;

/// The gate must not stall a request. The measured p95 is 74 ms, so two
/// seconds is 27 times that measurement, and a slower answer is an outage.
const GATE_TIMEOUT: Duration = Duration::from_secs(2);

/// The first answer can pay a server-side compile, so setup waits longer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// The two spans the setup probe sends. Neither holds a real value.
const BENIGN: &str = "The build finished in 41 seconds and 3 tests failed on Windows.";
const CREDENTIAL: &str = "Paste this into the console: sk-live-1234567890abcdef1234567890abcd";

/// The two answers that decide whether a server can drive the gate.
pub(crate) struct Probe {
    pub benign: f64,
    pub credential: f64,
}

impl Probe {
    /// A server that clears both spans flags every span, so the gate would
    /// audit all traffic.
    pub(crate) fn usable(&self) -> bool {
        self.benign < FLOOR || self.credential < FLOOR
    }
}

/// The probability that `span` holds sensitive information.
pub(crate) async fn score(base: &str, span: &str) -> Result<f64, String> {
    ask(base, span, QUESTION, GATE_TIMEOUT).await
}

/// The setup measurement for one server, so the slider maps to this server's
/// behaviour and not to another model's.
pub(crate) async fn probe(base: &str) -> Result<Probe, String> {
    let benign = ask(base, BENIGN, QUESTION, PROBE_TIMEOUT).await?;
    let credential = ask(base, CREDENTIAL, QUESTION, PROBE_TIMEOUT).await?;
    Ok(Probe { benign, credential })
}

async fn ask(
    base: &str,
    state: &str,
    question: &str,
    budget: Duration,
) -> Result<f64, String> {
    let url = format!("{}/v1/systemone", base.trim_end_matches('/'));
    let body = json!({
        "state": clip(state),
        "questions": {
            "sensitive": {
                "type": "noul",
                "instructions": question,
                "criteria": { "true": "yes", "false": "no" },
            }
        }
    });
    let sent = CLIENT
        .post(&url)
        .timeout(budget)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    let status = sent.status();
    if !status.is_success() {
        return Err(format!("{url}: HTTP {status}"));
    }
    let answer: Value = sent
        .json()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    answer["answers"]["sensitive"]["noul"]
        .as_f64()
        .ok_or_else(|| format!("{url}: no probability in the answer"))
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_SPAN_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floored_server_is_refused() {
        assert!(!Probe {
            benign: 0.9,
            credential: 0.95
        }
        .usable());
        assert!(Probe {
            benign: 0.12,
            credential: 0.91
        }
        .usable());
    }

    #[test]
    fn a_long_span_keeps_its_head() {
        let long = "a".repeat(MAX_SPAN_CHARS + 500);
        assert_eq!(clip(&long).chars().count(), MAX_SPAN_CHARS);
    }
}
