//! Context-window management: which parts of the conversation are sent
//! to the provider. Design and evidence: `docs/COMPACTION.md`.
//!
//! The unit is tokens, never message rows: a row budget silently drops the
//! user's task when the rows are tool-heavy. A request begins at a user
//! turn, never mid-turn. Compaction spends the provider's prefix cache.
//! Compaction fires only when the window demands it. Pruning works in
//! chunks large enough to be worth that churn.

use serde_json::Value;

/// Used when neither the user nor the endpoint gives a window. The value
/// stays inside small local models and avoids summarizing a hosted
/// model's history every other turn.
pub(crate) const DEFAULT_WINDOW_TOKENS: u32 = 32_768;

/// Never prune inside this many newest tokens: the evidence for what the
/// agent is doing right now.
pub(crate) const KEEP_RECENT_TOKENS: usize = 20_000;

/// Pruning rewrites old rows, which costs a prefix-cache miss. Only do it
/// when it frees at least this much.
const MIN_PRUNE_SAVINGS_TOKENS: usize = 2_000;

/// A fold costs a summariser call and the same prefix-cache miss that
/// pruning costs. Dropping a handful of rows does not repay that cost.
/// Measured on a 6k window: one fold traded a call for 259 tokens.
pub(crate) const MIN_FOLD_SAVINGS_TOKENS: usize = 1_000;

/// Charge a screenshot as a fixed allowance, not by base64 length. The
/// encoded bytes are not text tokens. Every vision backend bills one
/// image as a few hundred to a couple thousand tokens.
const IMAGE_TOKENS: usize = 1_200;

/// The tool registry is sent whole on every request (docs/SKILLS.md). The
/// registry occupies the window whether or not a tool is ever called.
pub(crate) fn tools_tokens() -> usize {
    estimate_text(crate::tooldefs::agent_tools().to_string().len())
}

/// Text averages ~4 chars/token. The error runs both ways and is small
/// against the budget it is compared with.
fn estimate_text(chars: usize) -> usize {
    chars / 4
}

pub(crate) fn estimate_str(s: &str) -> usize {
    estimate_text(s.chars().count())
}

/// Image parts count as a fixed allowance, not as their encoded length.
pub(crate) fn estimate_tokens(msgs: &[Value]) -> usize {
    msgs.iter()
        .map(|m| match m.get("content") {
            Some(Value::String(s)) => estimate_text(s.chars().count()),
            Some(Value::Array(parts)) => parts
                .iter()
                .map(|p| match p.get("type").and_then(|t| t.as_str()) {
                    Some("image_url") => IMAGE_TOKENS,
                    _ => estimate_text(
                        p.get("text")
                            .and_then(|t| t.as_str())
                            .map_or(0, |s| s.chars().count()),
                    ),
                })
                .sum(),
            // Assistant rows carrying tool calls have no string content; the
            // serialized row is the arguments the model already paid for.
            _ => estimate_text(m.to_string().chars().count()),
        })
        .sum()
}

/// Clamp `percent` so a bad setting can neither disable compaction nor
/// fire it on an empty window.
pub(crate) fn threshold_tokens(window: u32, percent: u8) -> usize {
    let percent = u64::from(percent.clamp(10, 100));
    ((u64::from(window) * percent) / 100) as usize
}

/// The fold target is NOT the threshold. Trimming a request to exactly the
/// threshold leaves it over the line again on the next tool result. Every
/// fold costs a summariser call. The headroom below the threshold keeps a
/// long run from folding on every turn.
pub(crate) fn target_tokens(threshold: usize) -> usize {
    threshold / 4 * 3
}

/// The line a compaction marks in the system prompt. A new brief replaces
/// the section, so a second compaction never stacks on the first.
const SUMMARY_HEADER: &str = "## Earlier conversation summary";

/// Closes the folded brief so `get_summary` can recover it. A second
/// compaction folds the first brief forward instead of replacing it.
const SUMMARY_TRAILER: &str = "Use this brief as established fact: the transcript below begins where it ends.";

/// The brief belongs in the system prompt, not in a message. The brief is
/// framing, not a turn. The history stays a clean sequence of
/// user/assistant/tool rows. `Some` replaces any earlier brief. `None`
/// leaves the prompt exactly as it was written.
pub(crate) fn summary_into(prompt: &str, summary: Option<&str>) -> String {
    let head = head_of(prompt);
    match summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("{head}\n\n{SUMMARY_HEADER}\n\n{s}\n\n{SUMMARY_TRAILER}"),
        None => head.to_string(),
    }
}

pub(crate) fn set_summary(system: &mut Value, summary: Option<&str>) {
    let Some(prompt) = prompt_of(system) else {
        return;
    };
    let folded = summary_into(&prompt, summary);
    match system {
        Value::String(s) => *s = folded,
        Value::Object(fields) => {
            fields.insert("content".to_string(), Value::String(folded));
        }
        _ => {}
    }
}

pub(crate) fn get_summary(system: &Value) -> Option<String> {
    let prompt = prompt_of(system)?;
    let (_, rest) = prompt.split_once(SUMMARY_HEADER)?;
    rest.trim()
        .strip_suffix(SUMMARY_TRAILER)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn head_of(prompt: &str) -> &str {
    prompt
        .split_once(format!("\n\n{SUMMARY_HEADER}").as_str())
        .map_or(prompt, |(head, _)| head)
}

/// Callers hold the prompt two ways: the request builders hold message
/// objects, the chat paths hold bare strings.
fn prompt_of(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => system.get("content").and_then(|c| c.as_str()).map(String::from),
        _ => None,
    }
}

/// Screenshot attachments also arrive as `user` rows mid-turn. A cut at
/// such a row is still legal and still leaves the tool round intact.
fn is_turn_start(m: &Value) -> bool {
    m.get("role").and_then(|r| r.as_str()) == Some("user")
}

/// Index of the first row to keep when the history must fit `budget`.
///
/// The oldest kept row is always a user row. A cut that lands mid-turn
/// would drop the instruction the rest of the request obeys. The
/// overshoot past `budget` is one turn, which the threshold's reserve
/// absorbs.
pub(crate) fn turn_anchored_cut(msgs: &[Value], budget: usize) -> usize {
    if msgs.is_empty() {
        return 0;
    }
    let mut used = 0usize;
    let mut first = msgs.len();
    for i in (0..msgs.len()).rev() {
        let t = estimate_tokens(&msgs[i..i + 1]);
        if first < msgs.len() && used + t > budget {
            break;
        }
        used += t;
        first = i;
    }
    if first == msgs.len() {
        // Even the last row exceeds the budget; send it rather than nothing.
        first = msgs.len() - 1;
    }
    while first > 0 && !is_turn_start(&msgs[first]) {
        first -= 1;
    }
    first
}

/// The rows a run keeps when the run has outgrown the window: `(head, tail)`.
/// Rows `..head` and `tail..` are kept. The middle is folded into the brief.
///
/// A run usually holds ONE turn: a long tool loop answering one request.
/// `turn_anchored_cut` would anchor back to that turn's first row, cut
/// nothing, and let the request go out oversized. Dropping whole tool rounds
/// out of the middle is the only cut that is both legal and effective. Legal
/// means no `tool` row is orphaned from the call that produced it. The opening
/// row is kept unconditionally, because it is the instruction the rest obeys.
pub(crate) fn middle_out_cut(msgs: &[Value], budget: usize) -> (usize, usize) {
    if msgs.is_empty() {
        return (0, 0);
    }
    let head = usize::from(is_turn_start(&msgs[0]));
    let mut used = 0usize;
    let mut tail = msgs.len();
    for i in (head..msgs.len()).rev() {
        let t = estimate_tokens(&msgs[i..i + 1]);
        if tail < msgs.len() && used + t > budget {
            break;
        }
        used += t;
        tail = i;
    }
    if tail == msgs.len() {
        tail = msgs.len() - 1;
    }
    // The kept tail may not open on a tool result: its call would be gone.
    while tail > head && msgs[tail].get("role").and_then(|r| r.as_str()) == Some("tool") {
        tail -= 1;
    }
    (head, tail)
}

/// The tools whose results ARE the instructions: eliding them would leave a
/// gated tool unlocked with its rules gone.
fn is_skill_result(name: &str) -> bool {
    name == "load_skill" || name == "read_skill_resource"
}

/// How much of the newest history pruning may not touch: the reference
/// window, capped at half the threshold. Without the cap, a fixed 20k
/// protection covers the entire conversation on a small-window model. The
/// cheapest compaction step then becomes unreachable on the small-window
/// models that need it most.
pub(crate) fn prune_protect_tokens(threshold: usize) -> usize {
    KEEP_RECENT_TOKENS.min(threshold / 2)
}

/// The savings a prune pass must clear before it rewrites anything,
/// scaled to the window. A fixed 2k floor is unreachable on a 6k model,
/// so oversized requests would slip past the cheapest compaction step.
pub(crate) fn prune_savings_floor(threshold: usize) -> usize {
    MIN_PRUNE_SAVINGS_TOKENS.min(threshold / 4)
}

/// Replace old tool bodies with a one-line marker, keeping the message
/// itself. A `tool` row must stay paired with its `tool_call`.
///
/// Pruning is monotone. A row old enough to prune stays old as the run
/// grows. The cached prefix is churned once rather than every turn.
/// Returns the tokens freed, or 0 with nothing rewritten when the savings
/// do not clear `floor`.
pub(crate) fn prune_old_tool_bodies(msgs: &mut [Value], protect: usize, floor: usize) -> usize {
    // Which tool each `tool` row answers, so skill bodies can be spared.
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for m in msgs.iter() {
        if let Some(tcs) = m.get("tool_calls").and_then(|c| c.as_array()) {
            for tc in tcs {
                if let (Some(id), Some(n)) = (
                    tc.get("id").and_then(|i| i.as_str()),
                    tc.pointer("/function/name").and_then(|n| n.as_str()),
                ) {
                    names.insert(id.to_string(), n.to_string());
                }
            }
        }
    }

    // Rows newer than `protect` tokens may not be touched: index `prunable`
    // and onward is the protected suffix. A history that never exceeds the
    // window is protected whole.
    let mut used = 0usize;
    let mut prunable = 0usize;
    for i in (0..msgs.len()).rev() {
        used += estimate_tokens(&msgs[i..i + 1]);
        if used > protect {
            prunable = i + 1;
            break;
        }
    }

    // Markers are computed before anything is rewritten. A pass that cannot
    // free enough tokens to justify the cache miss must leave the request
    // untouched. Such a pass must not elide the transcript and then report
    // that it did nothing.
    let mut marks: Vec<(usize, String)> = Vec::new();
    let mut freed = 0usize;
    for (i, m) in msgs[..prunable].iter().enumerate() {
        if m.get("role").and_then(|r| r.as_str()) != Some("tool") {
            continue;
        }
        let answered = m
            .get("tool_call_id")
            .and_then(|i| i.as_str())
            .and_then(|id| names.get(id))
            .map(String::as_str)
            .unwrap_or("");
        if is_skill_result(answered) {
            continue;
        }
        let Some(body) = m.get("content").and_then(|c| c.as_str()) else {
            continue;
        };
        if body.chars().count() < 200 {
            continue;
        }
        // The spill pointer already in the body is the recovery path: keep
        // it, drop the pasted output around it.
        let marker = match body
            .split("FULL output saved: ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
        {
            Some(path) => format!("[output truncated — full text: {path}]"),
            None => format!("[older output elided — {} chars]", body.chars().count()),
        };
        freed += estimate_text(body.chars().count()) - estimate_text(marker.chars().count());
        marks.push((i, marker));
    }
    if freed < floor {
        return 0;
    }
    for (i, marker) in marks {
        let id = msgs[i]
            .get("tool_call_id")
            .cloned()
            .unwrap_or(Value::Null);
        msgs[i] = serde_json::json!({ "role": "tool", "tool_call_id": id, "content": marker });
    }
    freed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(t: &str) -> Value {
        serde_json::json!({"role": "user", "content": t})
    }
    fn assistant(t: &str) -> Value {
        serde_json::json!({"role": "assistant", "content": t})
    }
    fn tool(id: &str, body: &str) -> Value {
        serde_json::json!({"role": "tool", "tool_call_id": id, "content": body})
    }
    fn call(id: &str, name: &str) -> Value {
        serde_json::json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": id, "type": "function", "function": {"name": name, "arguments": "{}"}}
        ]})
    }

    #[test]
    fn images_are_counted_as_an_allowance_not_as_base64_length() {
        let big = "A".repeat(400_000);
        let msgs = serde_json::json!([
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": big}}
            ]}
        ]);
        let msgs = msgs.as_array().unwrap();
        assert!(
            estimate_tokens(msgs) < 5_000,
            "a megabyte of base64 must not read as a megabyte of tokens"
        );
        assert_eq!(estimate_tokens(msgs), IMAGE_TOKENS + 1);
    }

    #[test]
    fn the_history_cut_lands_on_a_user_turn() {
        // Two tool-heavy turns, each opened by the instruction the cut
        // must preserve.
        let mut msgs = vec![user("restructure the log pipeline")];
        for i in 0..20 {
            msgs.push(assistant("working"));
            msgs.push(tool(&format!("c{i}"), &format!("output {}", "x".repeat(400))));
        }
        msgs.push(user("now rotate the archive"));
        for i in 20..40 {
            msgs.push(assistant("working"));
            msgs.push(tool(&format!("c{i}"), &format!("output {}", "x".repeat(400))));
        }
        let cut = turn_anchored_cut(&msgs, 1_000);
        assert!(cut > 0, "the budget must actually cut something");
        assert!(is_turn_start(&msgs[cut]), "a request may not begin mid-turn");
        assert_eq!(turn_anchored_cut(&msgs, 100_000), 0);
    }

    #[test]
    fn a_run_with_one_long_turn_cuts_its_middle_not_its_instruction() {
        // The shape a real run reaches: one user row, then a long tool loop.
        let mut msgs = vec![user("restructure the log pipeline")];
        for i in 0..40 {
            msgs.push(assistant("working"));
            msgs.push(tool(&format!("c{i}"), &format!("output {}", "x".repeat(400))));
        }
        assert_eq!(
            turn_anchored_cut(&msgs, 500),
            0,
            "anchoring a single turn keeps it whole — which is why runs need middle-out"
        );
        let (head, tail) = middle_out_cut(&msgs, 1_000);
        assert_eq!(head, 1, "the instruction is never dropped");
        assert!(tail > head && tail < msgs.len());
        assert_ne!(
            msgs[tail]["role"].as_str().unwrap(),
            "tool",
            "the kept tail may not open on a result whose call was dropped"
        );
    }

    #[test]
    fn a_small_window_still_has_output_worth_pruning() {
        // A fixed 20k protection on a 12k window protects the whole
        // conversation, making the cheapest compaction step unreachable.
        let threshold = threshold_tokens(12_000, 60);
        let protect = prune_protect_tokens(threshold);
        assert!(protect < threshold, "the newest half is not the whole window");
        let mut msgs = vec![tool("c1", &"y".repeat(40_000)), tool("c2", "short")];
        let freed = prune_old_tool_bodies(&mut msgs, protect, prune_savings_floor(threshold));
        assert!(freed > 0, "the older body must be prunable at this size");
        assert_eq!(
            msgs[1]["content"].as_str().unwrap(),
            "short",
            "the protected newest row is untouched"
        );
        // A big window keeps the reference protection and the reference floor.
        // A small window must be able to reach its cache miss at all.
        assert_eq!(
            prune_protect_tokens(threshold_tokens(200_000, 85)),
            KEEP_RECENT_TOKENS
        );
        assert_eq!(
            prune_savings_floor(threshold_tokens(200_000, 85)),
            MIN_PRUNE_SAVINGS_TOKENS
        );
        assert!(
            prune_savings_floor(threshold_tokens(6_000, 85)) < MIN_PRUNE_SAVINGS_TOKENS,
            "a fixed 2k floor is unreachable on a 6k window"
        );
    }

    #[test]
    fn pruning_keeps_the_pair_and_the_spill_pointer() {
        let pointer = r"C:\Users\dusti\.firstmate\output\out-1-2-3.txt";
        let body = format!(
            "head line\n...[9000 of 12000 chars omitted; FULL output saved: {pointer} — read_file for the middle]...\n{}",
            "x".repeat(9_000)
        );
        let mut msgs = vec![call("c0", "run_command"), tool("c0", &body)];
        let freed = prune_old_tool_bodies(&mut msgs, 0, MIN_PRUNE_SAVINGS_TOKENS);
        assert!(freed > MIN_PRUNE_SAVINGS_TOKENS);
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "c0");
        assert_eq!(msgs[1]["tool_call_id"], "c0", "the pair survives elision");
        let content = msgs[1]["content"].as_str().unwrap();
        assert!(content.contains(pointer), "elided text stays recoverable");
        assert!(!content.contains("tail line"));
    }

    #[test]
    fn pruning_spares_skill_instructions_and_the_recent_window() {
        let rules = "## Rules\n".to_string() + &"do it right ".repeat(400);
        let mut msgs = vec![call("s1", "load_skill"), tool("s1", &rules)];
        prune_old_tool_bodies(&mut msgs, 0, MIN_PRUNE_SAVINGS_TOKENS);
        assert_eq!(
            msgs[1]["content"].as_str().unwrap(),
            rules,
            "a gated tool must not outlive its rules"
        );

        // A history still inside the protected window is left alone whole,
        // however big its rows are.
        let mut recent = vec![tool("c1", &"y".repeat(40_000))];
        assert_eq!(
            prune_old_tool_bodies(&mut recent, KEEP_RECENT_TOKENS, MIN_PRUNE_SAVINGS_TOKENS),
            0
        );
        assert_eq!(recent[0]["content"].as_str().unwrap().len(), 40_000);

        // Old rows below the savings floor do not earn a cache miss.
        let mut small = vec![tool("c2", &"z".repeat(2_000))];
        assert_eq!(
            prune_old_tool_bodies(&mut small, 0, MIN_PRUNE_SAVINGS_TOKENS),
            0
        );
        assert_eq!(small[0]["content"].as_str().unwrap().len(), 2_000);
    }

    #[test]
    fn a_second_compaction_replaces_the_brief_instead_of_stacking() {
        let mut system = user("You are First Mate.");
        set_summary(&mut system, Some("first brief"));
        assert!(system["content"].as_str().unwrap().contains("first brief"));
        set_summary(&mut system, Some("second brief"));
        let text = system["content"].as_str().unwrap();
        assert!(text.contains("second brief") && !text.contains("first brief"));
        assert_eq!(text.matches(SUMMARY_HEADER).count(), 1);
        assert!(text.starts_with("You are First Mate."));
        assert_eq!(get_summary(&system).as_deref(), Some("second brief"));
        set_summary(&mut system, None);
        assert_eq!(system["content"], "You are First Mate.");
        assert_eq!(get_summary(&system), None);
    }

    #[test]
    fn the_threshold_is_a_share_of_the_real_window() {
        assert_eq!(threshold_tokens(200_000, 85), 170_000);
        // Garbage percentages cannot disable compaction or trigger it at once.
        assert_eq!(threshold_tokens(1_000, 0), 100);
        assert_eq!(threshold_tokens(1_000, 255), 1_000);
    }
}
