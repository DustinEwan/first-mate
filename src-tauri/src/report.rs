use crate::fileedit::{base_name, clip_chars, clip_lines, extract_diff_fence, fence_for, result_tag};

/// Facts about one tool call, with no presentation. The frontend renders
/// these fields in the report panel. `transcript()` serializes the same
/// fields into the flat model-facing form persisted to `.chat` and resumed
/// as context. The transcript must stay independent of UI presentation
/// choices.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct ToolReport {
    pub(crate) tool: String,
    /// The target of the call: a command, path, pid, or pattern.
    pub(crate) subject: String,
    /// Per-tool detail in the tool's own notation: a read window, edit op
    /// headers, or a search root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<bool>,
    /// Snapshot tag an edit was anchored to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from_tag: Option<String>,
    pub(crate) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// Background completion status ("exited: 0", "killed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pid: Option<u64>,
    pub(crate) wall_ms: u64,
    /// Snapshot tag after a successful write/edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) count: Option<usize>,
    /// Unit for `count`: "lines" | "entries" | "bytes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<ReportBody>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct ReportBody {
    /// "output" (program output) or "diff" (applied edit lines).
    pub(crate) kind: String,
    pub(crate) text: String,
}

impl ToolReport {
    /// The flat model-facing form. A resumed model re-reads the output as
    /// conversation content. UI markup must not appear in the output.
    pub(crate) fn transcript(&self) -> String {
        let mut out = self.headline();
        if let Some(b) = &self.body {
            if !b.text.trim().is_empty() {
                let f = fence_for(&b.text);
                let lang = if b.kind == "diff" { "diff" } else { "" };
                out.push_str(&format!("\n{f}{lang}\n{}\n{f}", b.text));
            }
        }
        out.push('\n');
        out.push_str(&self.footer());
        out
    }

    fn headline(&self) -> String {
        match self.tool.as_str() {
            "run_command" => {
                let mut id = format!("$ {}", clip_chars(&self.subject, 160));
                if let Some(sh) = &self.shell {
                    if sh != "cmd" {
                        id.push_str(&format!(" [{sh}]"));
                    }
                }
                if let Some(cwd) = &self.cwd {
                    id.push_str(&format!(" in {}", base_name(cwd)));
                }
                if self.background == Some(true) {
                    id.push_str(" [bg]");
                }
                id
            }
            "read_file" => format!(
                "→ {}{}",
                base_name(&self.subject),
                self.detail.as_deref().map(|d| format!(":{d}")).unwrap_or_default()
            ),
            "edit_file" => format!(
                "✎ {} #{} {}",
                base_name(&self.subject),
                self.from_tag.clone().unwrap_or_default(),
                self.detail.clone().unwrap_or_default()
            ),
            "write_file" => format!(
                "✎ {} ({} bytes)",
                base_name(&self.subject),
                self.count.unwrap_or(0)
            ),
            "list_dir" => format!("▸ {}", self.subject),
            "search_files" => format!(
                "⌕ \"{}\" under {}",
                self.subject,
                self.detail.clone().unwrap_or_default()
            ),
            "list_processes" => "≣ running processes".to_string(),
            "get_command_output" => format!("◷ output of pid {}", self.subject),
            "kill_command" => format!("✖ kill pid {}", self.subject),
            "load_skill" => format!("📖 load_skill {}", self.subject),
            "read_skill_resource" => format!("📖 {}", self.subject),
            _ => clip_chars(&format!("{} {}", self.tool, self.detail.clone().unwrap_or_default()), 140),
        }
    }

    fn footer(&self) -> String {
        let mark = if self.ok {
            "✅".to_string()
        } else {
            format!(
                "❌ {}",
                clip_chars(self.error.as_deref().unwrap_or("failed"), 120)
            )
        };
        let mut meta: Vec<String> = Vec::new();
        if let Some(st) = &self.status {
            meta.push(st.clone());
        }
        if self.wall_ms > 0 {
            meta.push(format!("{:.1}s", self.wall_ms as f64 / 1000.0));
        }
        if let Some(tag) = &self.tag {
            meta.push(format!("#{tag}"));
        }
        if let (Some(n), Some(u)) = (&self.count, &self.unit) {
            meta.push(format!("{n} {u}"));
        }
        // A status marks a background completion, so the footer names the
        // job by pid. A foreground tool already carries the pid as subject.
        if let (Some(pid), Some(_)) = (self.pid, &self.status) {
            meta.push(format!("pid {pid}"));
        }
        if meta.is_empty() {
            mark
        } else {
            format!("{mark} {}", meta.join(" · "))
        }
    }
}

pub(crate) fn tool_report(
    name: &str,
    args: &serde_json::Value,
    result: &str,
    wall_ms: u128,
) -> ToolReport {
    let ok = !result.starts_with("Error");
    let s = |k: &str| match args.get(k) {
        // `Value::to_string` would quote a JSON string, so string args come
        // from `as_str`; numeric args such as pids stringify.
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
        None => String::new(),
    };
    let opt = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .filter(|v| !v.is_empty())
    };
    let (subject, detail) = match name {
        "run_command" => (
            s("command")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            None,
        ),
        "read_file" => {
            let detail = args.get("offset").and_then(|v| v.as_u64()).map(|off| {
                let end = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|l| format!("{}", off + l - 1))
                    .unwrap_or_else(|| "…".into());
                format!("{off}-{end}")
            });
            (s("path"), detail)
        }
        "edit_file" => {
            let ops = s("ops");
            let heads: Vec<&str> = ops
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('+'))
                .collect();
            (s("path"), Some(clip_chars(&heads.join(", "), 90)))
        }
        "write_file" => (s("path"), None),
        "list_dir" => (s("path"), None),
        "search_files" => (s("pattern"), Some(s("root"))),
        "list_processes" => (String::new(), None),
        "get_command_output" | "kill_command" => (s("pid"), None),
        "load_skill" => (s("name"), None),
        "read_skill_resource" => (s("path"), None),
        "run_elevated" => (
            s("command")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            Some("elevated".into()),
        ),
        "elevate" | "delevate" => (String::new(), None),
        _ => {
            let kv = args
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| format!("{k}={}", clip_chars(&v.to_string(), 40)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            (name.to_string(), Some(kv))
        }
    };
    let body = if !ok {
        Some(ReportBody {
            kind: "output".into(),
            text: clip_lines(result, 6),
        })
    } else {
        match name {
            "edit_file" => extract_diff_fence(result).map(|text| ReportBody {
                kind: "diff".into(),
                text,
            }),
            "run_command" | "get_command_output" | "run_elevated" => Some(ReportBody {
                kind: "output".into(),
                text: clip_lines(result, 18),
            }),
            _ => None,
        }
    };
    let (count, unit) = if ok {
        match name {
            "write_file" => (Some(s("content").len()), Some("bytes")),
            "read_file" => (
                Some(
                    result
                        .lines()
                        .filter(|l| {
                            l.split_once(':').is_some_and(|(a, _)| {
                                !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())
                            })
                        })
                        .count(),
                ),
                Some("lines"),
            ),
            "list_dir" | "search_files" => (Some(result.lines().count()), Some("entries")),
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    ToolReport {
        tool: name.to_string(),
        subject,
        detail: detail.filter(|d| !d.is_empty()),
        shell: opt("shell"),
        cwd: opt("cwd"),
        background: (args.get("background").and_then(|v| v.as_bool()) == Some(true))
            .then_some(true),
        from_tag: opt("tag"),
        ok,
        error: (!ok)
            .then(|| clip_chars(result.strip_prefix("Error: ").unwrap_or(result), 400)),
        status: None,
        pid: args.get("pid").and_then(|v| v.as_u64()).filter(|_| {
            matches!(name, "get_command_output" | "kill_command")
        }),
        wall_ms: wall_ms as u64,
        tag: (ok && matches!(name, "write_file" | "edit_file"))
            .then(|| result_tag(result))
            .flatten(),
        count,
        unit: unit.map(str::to_string),
        body: body.filter(|b| !b.text.trim().is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_reports_carry_facts_and_transcript_not_presentation() {
        let r = tool_report(
            "run_command",
            &serde_json::json!({ "command": "echo  hi", "shell": "powershell" }),
            "hi\n",
            1400,
        );
        assert_eq!(r.subject, "echo hi");
        assert_eq!(r.shell.as_deref(), Some("powershell"));
        assert_eq!(r.transcript(), "$ echo hi [powershell]\n```\nhi\n```\n✅ 1.4s");
        let edit_result = "edited C:\\x\\notes.md: PUT 3.3 -> 1 line(s)\n```diff\n-for\n+EDITED\n```\n[C:\\x\\notes.md#F926] - line numbers shifted";
        let r = tool_report(
            "edit_file",
            &serde_json::json!({ "path": "C:\\x\\notes.md", "tag": "443A", "ops": "PUT 3.=3:\n+EDITED\n" }),
            edit_result,
            12,
        );
        assert_eq!(r.subject, "C:\\x\\notes.md");
        assert_eq!(r.from_tag.as_deref(), Some("443A"));
        assert_eq!(r.tag.as_deref(), Some("F926"));
        assert_eq!(r.body.as_ref().unwrap().kind, "diff");
        assert_eq!(
            r.transcript(),
            "✎ notes.md #443A PUT 3.=3:\n```diff\n-for\n+EDITED\n```\n✅ 0.0s · #F926"
        );
        assert!(
            !r.transcript().contains("+EDITED\n+EDITED"),
            "op body must not be duplicated"
        );
        // The report carries a byte count. File content must never appear
        // in the report.
        let r = tool_report(
            "write_file",
            &serde_json::json!({ "path": "C:\\x\\d.md", "content": "secret stuff" }),
            "wrote 12 bytes to C:\\x\\d.md\n[C:\\x\\d.md#ABCD]",
            5,
        );
        assert_eq!((r.count, r.unit.as_deref()), (Some(12), Some("bytes")));
        let t = r.transcript();
        assert!(t.starts_with("✎ d.md (12 bytes)") && !t.contains("secret"), "{t}");
        assert!(t.contains("#ABCD"), "{t}");
        let r = tool_report(
            "read_file",
            &serde_json::json!({ "path": "C:\\x\\d.md", "offset": 2, "limit": 3 }),
            "[C:\\x\\d.md#1111]\n2:a\n3:b\n4:c\n",
            3,
        );
        assert_eq!(r.detail.as_deref(), Some("2-4"));
        assert!(r.body.is_none());
        assert_eq!(r.transcript(), "→ d.md:2-4\n✅ 0.0s · 3 lines");
        let r = tool_report(
            "run_command",
            &serde_json::json!({ "command": "dir" }),
            "Error: Blocked: duplicates list_dir",
            1,
        );
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("Blocked: duplicates list_dir"));
        assert_eq!(r.body.as_ref().unwrap().kind, "output");
        let t = r.transcript();
        assert!(
            t.starts_with("$ dir\n")
                && t.ends_with("❌ Blocked: duplicates list_dir 0.0s"),
            "{t}"
        );
        // A numeric pid must stringify as 42, not as a quoted JSON value.
        let r = tool_report("kill_command", &serde_json::json!({ "pid": 42 }), "killed", 0);
        assert_eq!(r.subject, "42");
        assert_eq!(r.pid, Some(42));
        assert_eq!(r.transcript(), "✖ kill pid 42\n✅");
    }
}
