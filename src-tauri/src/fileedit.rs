/// LCS-based line diff: lines dropped from `prev` are `-`, added lines `+`.
pub(crate) fn line_diff(prev: &str, now: &str) -> String {
    let a: Vec<&str> = prev.lines().collect();
    let b: Vec<&str> = now.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push_str("- ");
            out.push_str(a[i]);
            out.push('\n');
            i += 1;
        } else {
            out.push_str("+ ");
            out.push_str(b[j]);
            out.push('\n');
            j += 1;
        }
    }
    while i < n {
        out.push_str("- ");
        out.push_str(a[i]);
        out.push('\n');
        i += 1;
    }
    while j < m {
        out.push_str("+ ");
        out.push_str(b[j]);
        out.push('\n');
        j += 1;
    }
    out
}

/// Content hash of a file snapshot (FNV-1a, 4 hex chars). `read_file`
/// stamps it on its output; `edit_file`/`write_file` demand it back, so an
/// edit anchored to stale line numbers is rejected instead of silently
/// corrupting the file.
pub(crate) fn snapshot_tag(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ *b as u64).wrapping_mul(0x1000_0000_01b3);
    }
    format!("{:04X}", ((h >> 32) as u32 & 0xFFFF) as u16)
}

/// Last tag the model actually SAW per path (process-wide: one user, one
/// app). Guards edits against files that changed since the model's read.
static SNAPSHOT_TAGS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();

fn snapshot_tags() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    SNAPSHOT_TAGS.get_or_init(Default::default)
}

pub(crate) fn record_snapshot(path: &str, tag: &str) {
    snapshot_tags()
        .lock()
        .unwrap()
        .insert(path.to_lowercase(), tag.to_string());
}

/// The tag the model last saw for `path`, if it ever read it.
pub(crate) fn seen_snapshot(path: &str) -> Option<String> {
    snapshot_tags()
        .lock()
        .unwrap()
        .get(&path.to_lowercase())
        .cloned()
}

/// One line-anchored instruction for `edit_file`.
pub(crate) enum EditOp {
    Replace { lo: usize, hi: usize, body: Vec<String> },
    InsertBefore { at: usize, body: Vec<String> },
    InsertAfter { at: usize, body: Vec<String> },
    Cut { lo: usize, hi: usize },
}

impl EditOp {
    /// Inclusive line span the op touches (inserts anchor at their gap).
    fn span(&self) -> (usize, usize) {
        match self {
            EditOp::Replace { lo, hi, .. } | EditOp::Cut { lo, hi } => (*lo, *hi),
            EditOp::InsertBefore { at, .. } | EditOp::InsertAfter { at, .. } => (*at, *at),
        }
    }
    fn anchor(&self) -> usize {
        self.span().0
    }
}

fn parse_line(s: &str, op: &str) -> Result<usize, String> {
    s.trim()
        .parse::<usize>()
        .map_err(|_| format!("op '{op}': '{s}' is not a line number"))
}

/// Parse the newline-separated `ops` payload. PUT headers end with ':' and
/// take the following '+'-prefixed lines as their verbatim body.
pub(crate) fn parse_edit_ops(ops: &str) -> Result<Vec<EditOp>, String> {
    let lines: Vec<&str> = ops.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("PUT ") {
            let Some(spec) = rest.strip_suffix(':') else {
                return Err(format!("op '{line}': PUT header must end with ':'"));
            };
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].starts_with('+') {
                body.push(lines[i][1..].trim_end_matches('\r').to_string());
                i += 1;
            }
            if let Some(at) = spec.strip_prefix('<') {
                out.push(EditOp::InsertBefore { at: parse_line(at, line)?, body });
            } else if let Some(at) = spec.strip_prefix('>') {
                out.push(EditOp::InsertAfter { at: parse_line(at, line)?, body });
            } else {
                let (a, b) = match spec.split_once(".=") {
                    Some((a, b)) => (a, b),
                    None => {
                        let a = spec.trim_end_matches('.');
                        (a, a)
                    }
                };
                let lo = parse_line(a, line)?;
                let hi = parse_line(b, line)?;
                if lo > hi {
                    return Err(format!("op '{line}': empty range {lo}.={hi}"));
                }
                out.push(EditOp::Replace { lo, hi, body });
            }
        } else if let Some(rest) = line.strip_prefix("CUT ") {
            let (a, b) = match rest.split_once(".=") {
                Some((a, b)) => (a, b),
                None => {
                    let a = rest.trim_end_matches('.');
                    (a, a)
                }
            };
            let lo = parse_line(a, line)?;
            let hi = parse_line(b, line)?;
            if lo > hi {
                return Err(format!("op '{line}': empty range {lo}.={hi}"));
            }
            out.push(EditOp::Cut { lo, hi });
            i += 1;
        } else {
            return Err(format!(
                "unknown op '{line}': expected PUT N.=M:, PUT <N:, PUT >N:, or CUT N.=M"
            ));
        }
    }
    if out.is_empty() {
        return Err("no ops given".into());
    }
    Ok(out)
}

/// Validate against `total` lines, then apply bottom-up so earlier line
/// numbers stay valid while later ops are applied. Returns per-op summaries
/// plus a unified-style diff (`-`/`+` lines) in original file order.
pub(crate) fn apply_edit_ops(
    lines: &mut Vec<String>,
    ops: &[EditOp],
    total: usize,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut spans = Vec::new();
    for op in ops {
        let (lo, hi) = op.span();
        if lo < 1 || hi > total {
            return Err(format!(
                "op anchors lines {lo}-{hi} but the file has {total} lines; re-read it"
            ));
        }
        spans.push((lo, hi));
    }
    spans.sort();
    for w in spans.windows(2) {
        if w[1].0 <= w[0].1 {
            return Err(format!(
                "ops overlap around line {} (and two inserts at the same anchor are ambiguous)",
                w[1].0
            ));
        }
    }
    let mut sorted: Vec<&EditOp> = ops.iter().collect();
    sorted.sort_by_key(|o| std::cmp::Reverse(o.anchor()));
    let mut summaries = Vec::new();
    // (original anchor, removed lines, added lines) per op.
    let mut changes: Vec<(usize, Vec<String>, Vec<String>)> = Vec::new();
    for op in sorted {
        match op {
            EditOp::Replace { lo, hi, body } => {
                let removed = lines[(lo - 1)..*hi].to_vec();
                lines.splice((lo - 1)..(*hi), body.iter().cloned());
                changes.push((*lo, removed, body.clone()));
                summaries.push(format!("PUT {lo}.{hi} -> {} line(s)", body.len()));
            }
            EditOp::Cut { lo, hi } => {
                let removed = lines[(lo - 1)..*hi].to_vec();
                lines.drain((lo - 1)..*hi);
                changes.push((*lo, removed, Vec::new()));
                summaries.push(format!("CUT {lo}.{hi}"));
            }
            EditOp::InsertBefore { at, body } => {
                lines.splice((*at - 1)..(*at - 1), body.iter().cloned());
                changes.push((*at, Vec::new(), body.clone()));
                summaries.push(format!("inserted {} line(s) before {at}", body.len()));
            }
            EditOp::InsertAfter { at, body } => {
                lines.splice(*at..*at, body.iter().cloned());
                changes.push((at + 1, Vec::new(), body.clone()));
                summaries.push(format!("inserted {} line(s) after {at}", body.len()));
            }
        }
    }
    changes.sort_by_key(|(at, _, _)| *at);
    let mut diff = Vec::new();
    for (_, removed, added) in changes {
        diff.extend(removed.iter().map(|l| format!("-{l}")));
        diff.extend(added.iter().map(|l| format!("+{l}")));
    }
    Ok((summaries, diff))
}

/// A fence long enough to survive any backtick run inside `body`.
pub(crate) fn fence_for(body: &str) -> &'static str {
    if body.contains("```") {
        "````"
    } else {
        "```"
    }
}

/// Clip to `keep` lines: head, an omission marker, tail.
pub(crate) fn clip_lines(s: &str, keep: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= keep {
        return s.trim_end().to_string();
    }
    let head = keep - keep / 4 - 1;
    let tail = keep / 4;
    format!(
        "{}\n…[{} of {} lines omitted; the model received the full result]…\n{}",
        lines[..head].join("\n"),
        lines.len() - head - tail,
        lines.len(),
        lines[lines.len() - tail..].join("\n")
    )
}

/// Last path segment, whichever separator the path uses.
pub(crate) fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub(crate) fn clip_chars(t: &str, n: usize) -> String {
    if t.chars().count() <= n {
        return t.to_string();
    }
    let cut = t.char_indices().nth(n).map(|(i, _)| i).unwrap_or(t.len());
    format!("{}…", &t[..cut])
}

/// The snapshot tag embedded in a tool result (`[path#TAG]`), if any.
pub(crate) fn result_tag(result: &str) -> Option<String> {
    let i = result.rfind('#')?;
    let t: String = result[i + 1..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (t.len() == 4).then_some(t)
}

/// The body of the first ``` ```diff ``` fence in `s` - the diff the
/// `edit_file` result already carries; the ledger reuses it verbatim.
pub(crate) fn extract_diff_fence(s: &str) -> Option<String> {
    let marker = "```diff\n";
    let start = s.find(marker)? + marker.len();
    let end = start + s[start..].find("\n```")?;
    Some(s[start..end].to_string())
}
