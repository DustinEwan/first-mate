//! Client for the decision server the user runs.
//! `docs/roadmap/REDACTION.md` holds the API contract and the measured
//! operating points.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};

/// A choice question over categories invites a topic answer. The measured model
/// scored the topic, not the privacy of the topic.
const QUESTION: &str = "Does this text contain potentially sensitive information?";

/// The server states a token budget, and the client counts characters. A refused
/// answer corrects this estimate for the served engine.
const CHARS_PER_TOKEN: usize = 4;

/// A window below this size cannot shrink further, so the message goes unjudged
/// and the server answer becomes the reason.
const MIN_WINDOW_CHARS: usize = 200;

/// A server that answers at or above this on a benign message reports a floor, not
/// a probability. The H2O shim ships a 0.801 yes/no floor.
pub(crate) const FLOOR: f64 = 0.8;

/// The measured p95 is 74 ms. Two seconds is 27 times that measurement.
const GATE_TIMEOUT: Duration = Duration::from_secs(2);

/// The first answer can pay a server-side compile, so setup waits longer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// A budget read must not delay the first message of a run.
const LIMIT_TIMEOUT: Duration = Duration::from_secs(5);

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// Window size in characters per server. `usize::MAX` means the server states no
/// budget, so the client sends the whole message and learns the size from a
/// refused answer.
static WINDOWS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The settings file may or may not carry a trailing slash, and both shapes name
/// one server.
fn key(base: &str) -> String {
    base.trim_end_matches('/').to_string()
}

/// Decis names the measured token count and the limit in a refused answer. Other
/// servers state nothing, so the client falls back to halving.
static OVER_LIMIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"about (\d+) tokens, over[^0-9]*(\d+)").unwrap());

/// Neither probe text holds a real value.
const BENIGN: &str = "The build finished in 41 seconds and 3 tests failed on Windows.";
const CREDENTIAL: &str = "Paste this into the console: sk-live-1234567890abcdef1234567890abcd";

pub(crate) struct Probe {
    pub benign: f64,
    pub credential: f64,
}

impl Probe {
    /// A server that clears both probe texts flags every message, so the gate
    /// would audit all traffic.
    pub(crate) fn usable(&self) -> bool {
        self.benign < FLOOR || self.credential < FLOOR
    }
}

/// One message's verdict. `windows` holds each window's range and score, so the
/// audit card marks the window the model saw something in. Ranges count chars
/// of the message, not bytes.
pub(crate) struct Score {
    pub best: f64,
    pub windows: Vec<(usize, usize, f64)>,
}

/// The probability that `message` holds sensitive information. A message over the
/// server budget gets overlapping windows, and the highest window score decides.
pub(crate) async fn score(base: &str, message: &str) -> Result<Score, String> {
    let chars: Vec<char> = message.chars().collect();
    // An empty state earns a refusal from some servers, and a refusal reports the
    // whole message unjudged.
    if chars.is_empty() {
        return Ok(Score {
            best: 0.0,
            windows: vec![],
        });
    }
    loop {
        let size = window_chars(base).await;
        let mut shrunk = false;
        let mut best = 0.0_f64;
        let mut scored: Vec<(usize, usize, f64)> = Vec::new();
        for (start, end) in windows(chars.len(), size) {
            let window: String = chars[start..end].iter().collect();
            match ask(base, &window, QUESTION, GATE_TIMEOUT).await {
                Answer::Score(p) => {
                    best = best.max(p);
                    scored.push((start, end, p));
                }
                Answer::TooLarge(answer) if end - start > MIN_WINDOW_CHARS => {
                    shrink(base, end - start, &answer);
                    shrunk = true;
                    break;
                }
                Answer::TooLarge(answer) => return Err(answer),
                Answer::Failed(e) => return Err(e),
            }
        }
        if !shrunk {
            return Ok(Score {
                best,
                windows: scored,
            });
        }
    }
}

/// Window ranges over `len` characters. A tenth of a window overlaps, so a value
/// at a boundary still appears whole inside one window.
fn windows(len: usize, size: usize) -> Vec<(usize, usize)> {
    if size >= len {
        return vec![(0, len)];
    }
    let step = size - size / 10;
    let mut out = Vec::new();
    let mut start = 0;
    while start < len {
        let end = start.saturating_add(size).min(len);
        out.push((start, end));
        if end == len {
            break;
        }
        start += step;
    }
    out
}

/// Window size in characters for this server.
async fn window_chars(base: &str) -> usize {
    let key = key(base);
    if let Some(size) = WINDOWS.lock().get(&key).copied() {
        return size;
    }
    let size = read_budget(&key).await;
    WINDOWS.lock().insert(key, size);
    size
}

/// Settings stores the address with its `/v1` suffix, the form `llm.rs` accepts
/// too. Appending an endpoint to that address doubles the segment.
fn root(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
}

/// `GET {base}/v1/models` reports the token budget the served engine declares.
async fn read_budget(base: &str) -> usize {
    let url = format!("{}/v1/models", root(base));
    let Ok(sent) = CLIENT.get(&url).timeout(LIMIT_TIMEOUT).send().await else {
        return usize::MAX;
    };
    let Ok(body) = sent.json::<Value>().await else {
        return usize::MAX;
    };
    let Some(model) = body["models"].as_array().and_then(|m| m.first()) else {
        return usize::MAX;
    };
    let limits = &model["decis"];
    let state = limits["max_state_tokens"].as_u64().unwrap_or(0) as usize;
    if state > 0 {
        return state.saturating_mul(CHARS_PER_TOKEN);
    }
    let sequence = limits["max_sequence_tokens"].as_u64().unwrap_or(0) as usize;
    let question = limits["max_question_tokens"].as_u64().unwrap_or(0) as usize;
    if sequence > question {
        return (sequence - question).saturating_mul(CHARS_PER_TOKEN);
    }
    usize::MAX
}

/// A refused answer states the measured count and the limit for some servers, so
/// the new window keeps that ratio. Halving is the fallback, because a smaller
/// window still covers the whole message.
fn shrink(base: &str, used: usize, answer: &str) {
    let key = key(base);
    let ratio = OVER_LIMIT.captures(answer).and_then(|c| {
        let measured: usize = c.get(1)?.as_str().parse().ok()?;
        let limit: usize = c.get(2)?.as_str().parse().ok()?;
        if measured == 0 || limit == 0 {
            return None;
        }
        Some((used * limit) / measured)
    });
    let size = ratio
        .filter(|n| *n < used && *n >= MIN_WINDOW_CHARS)
        .unwrap_or(used / 2)
        .max(MIN_WINDOW_CHARS);
    WINDOWS.lock().insert(key, size);
}

/// A refused size says something about the window. A failure says something about
/// the server. Only the second one ends the gate for that message.
enum Answer {
    Score(f64),
    TooLarge(String),
    Failed(String),
}

async fn ask(base: &str, state: &str, question: &str, budget: Duration) -> Answer {
    let url = format!("{}/v1/systemone", root(base));
    let body = json!({
        "state": state,
        "questions": {
            "sensitive": {
                "type": "noul",
                "instructions": question,
                "criteria": { "true": "yes", "false": "no" },
            }
        }
    });
    let sent = match CLIENT.post(&url).timeout(budget).json(&body).send().await {
        Ok(sent) => sent,
        Err(e) => return Answer::Failed(format!("{url}: send failed: {e}")),
    };
    let status = sent.status();
    if status.as_u16() == 413 || status.as_u16() == 422 {
        let detail = sent.text().await.unwrap_or_default();
        return Answer::TooLarge(format!("{url}: HTTP {status} {detail}"));
    }
    if !status.is_success() {
        return Answer::Failed(format!("{url}: HTTP {status}"));
    }
    let Ok(answer) = sent.json::<Value>().await else {
        return Answer::Failed(format!("{url}: unreadable answer"));
    };
    match answer["answers"]["sensitive"]["noul"].as_f64() {
        Some(p) => Answer::Score(p),
        None => Answer::Failed(format!("{url}: no probability in the answer")),
    }
}

/// The slider maps to the measured server, not to another model's behaviour.
pub(crate) async fn probe(base: &str) -> Result<Probe, String> {
    Ok(Probe {
        benign: ask_one(base, BENIGN).await?,
        credential: ask_one(base, CREDENTIAL).await?,
    })
}

async fn ask_one(base: &str, text: &str) -> Result<f64, String> {
    match ask(base, text, QUESTION, PROBE_TIMEOUT).await {
        Answer::Score(p) => Ok(p),
        Answer::TooLarge(answer) | Answer::Failed(answer) => Err(answer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

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
    fn a_short_message_is_one_window() {
        assert_eq!(windows(12, 4000), vec![(0, 12)]);
        assert_eq!(windows(12, usize::MAX), vec![(0, 12)]);
    }

    #[test]
    fn the_windows_cover_every_character_with_no_gap() {
        let mut at = 0;
        let mut scored = 0;
        for (start, end) in windows(100_000, 4000) {
            assert!(start <= at, "a gap left characters unscored");
            assert!(end <= 100_000);
            at = end;
            scored += end - start;
        }
        assert_eq!(at, 100_000, "the tail was never scored");
        assert!(scored > 100_000, "the windows hold no overlap");
    }

    #[test]
    fn a_window_overlaps_its_neighbour_by_a_tenth() {
        let w = windows(20_000, 4000);
        assert_eq!(w[1].0, w[0].1 - 400);
    }

    #[test]
    fn a_refused_answer_that_names_the_limit_sets_the_ratio() {
        let answer = "Question 'placement' is about 207 tokens, over this model's \
                      limit of 192 per question.";
        shrink("http://127.0.0.1:91/", 4000, answer);
        assert_eq!(window_size("http://127.0.0.1:91/"), (4000 * 192) / 207);
    }

    #[test]
    fn a_refused_answer_that_names_nothing_halves_the_window() {
        shrink("http://127.0.0.1:92/", 4000, "HTTP 413 request_too_large");
        assert_eq!(window_size("http://127.0.0.1:92/"), 2000);
    }

    fn window_size(base: &str) -> usize {
        *WINDOWS.lock().get(&key(base)).unwrap()
    }

    /// One thread answers every request on a loopback port, so a test sees the
    /// request the client really sent.
    fn serve(reply: impl Fn(&str) -> (String, String) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let request = read_request(&mut stream);
                if request.is_empty() {
                    continue;
                }
                let (status, body) = reply(&request);
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        base
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut head_end = 0usize;
        let mut body_len = usize::MAX;
        loop {
            if body_len == usize::MAX {
                if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    head_end = at + 4;
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
                    body_len = head
                        .lines()
                        .find_map(|l| l.trim().strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                }
            }
            if body_len != usize::MAX && buf.len() >= head_end + body_len {
                break;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn noul(flagged: bool) -> String {
        let p = if flagged { 0.9 } else { 0.05 };
        serde_json::json!({ "answers": { "sensitive": { "noul": p } } }).to_string()
    }

    #[tokio::test]
    async fn a_flag_in_the_tail_of_a_long_message_flags_the_message() {
        let base = serve(|request| {
            if request.starts_with("GET") {
                return (
                    "200 OK".to_string(),
                    r#"{"models":[{"decis":{"max_state_tokens":0,"max_sequence_tokens":1000,"max_question_tokens":100}}]}"#.to_string(),
                );
            }
            ("200 OK".to_string(), noul(request.contains("SECRET-IN-THE-TAIL")))
        });
        let mut text = "x".repeat(40_000);
        text.push_str("SECRET-IN-THE-TAIL");
        let verdict = score(&base, &text).await.unwrap();
        assert_eq!(verdict.best, 0.9);
        assert!(
            verdict
                .windows
                .iter()
                .any(|(_, end, p)| *p >= 0.9 && *end == text.chars().count()),
            "the audit card must mark the window that holds the flag"
        );
    }

    #[tokio::test]
    async fn a_base_url_that_ends_in_v1_sends_one_v1() {
        let base = serve(|request| {
            if request.contains("/v1/v1/") {
                return ("404 Not Found".to_string(), r#"{"detail":"Not Found"}"#.to_string());
            }
            if request.starts_with("GET") {
                return (
                    "200 OK".to_string(),
                    r#"{"models":[{"decis":{"max_state_tokens":0,"max_sequence_tokens":1000,"max_question_tokens":100}}]}"#.to_string(),
                );
            }
            ("200 OK".to_string(), noul(request.contains("SECRET-IN-THE-TAIL")))
        });
        let verdict = score(&format!("{base}/v1"), "SECRET-IN-THE-TAIL")
            .await
            .unwrap();
        assert_eq!(
            verdict.best, 0.9,
            "the doubled /v1 left the scorer unreachable"
        );
    }

    #[tokio::test]
    async fn a_refused_window_is_rescored_smaller() {
        let base = serve(|request| {
            if request.starts_with("GET") {
                return (
                    "404 Not Found".to_string(),
                    r#"{"detail":"Not Found"}"#.to_string(),
                );
            }
            if request.len() > 5000 {
                return (
                    "422 Unprocessable Entity".to_string(),
                    r#"{"detail":[{"msg":"state is about 1200 tokens, over this model's limit of 1000"}]}"#
                        .to_string(),
                );
            }
            ("200 OK".to_string(), noul(request.contains("SECRET-IN-THE-TAIL")))
        });
        let mut text = "x".repeat(30_000);
        text.push_str("SECRET-IN-THE-TAIL");
        let verdict = score(&base, &text).await.unwrap();
        assert_eq!(verdict.best, 0.9);
        assert!(
            verdict.windows.iter().all(|(start, end, _)| end - start <= 5_000),
            "a refused window must come back smaller, not stay whole"
        );
    }
}
