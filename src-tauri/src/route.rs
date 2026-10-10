use crate::tooldefs::skill_required;
use std::collections::HashSet;

/// A text-only turn that declares an action instead of answering: short
/// and action-declaring ("Let me check X"), or ending mid-thought. Small
/// models emit prose when they mean to call a tool.
pub(crate) fn looks_like_unexecuted_intent(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 400 {
        return false;
    }
    let lower = t.to_lowercase();
    let declares = ["let me", "i'll ", "i will ", "now let", "i'm going to", "let's "]
        .iter()
        .any(|p| lower.starts_with(p) || lower.contains(&format!(". {p}")));
    declares || t.ends_with(':') || t.ends_with('…') || t.ends_with("...")
}

/// Inline shell programs that duplicate a dedicated tool. Routing applies
/// ONLY after the owning skill of the duplicated tool is loaded in this
/// conversation. Before activation the model has not seen that tool's
/// rules, so the shell command passes through untouched.
/// Returns `(tool, why)`.
pub(crate) fn route_to_tool(command: &str, loaded: &HashSet<String>) -> Option<(&'static str, &'static str)> {
    let lower = command.to_lowercase();
    // Only program position counts: the first token of a command segment.
    // `git grep` stays legal; `cmd /c "dir"` does not.
    for seg in lower.split([';', '|', '&', '(', '"', '\'']) {
        let Some(tok) = seg.split_whitespace().next() else {
            continue;
        };
        let tok = tok.trim_end_matches(".exe");
        let (tool, why): (&'static str, &'static str) = match tok {
            "get-childitem" | "dir" | "ls" => ("list_dir", "enumerating directories"),
            "get-content" | "type" | "cat" | "more" => {
                ("read_file", "reading files with ranges and line numbers")
            }
            "select-string" | "findstr" => ("search_files", "searching file contents"),
            "get-process" | "tasklist" => ("list_processes", "listing processes"),
            "set-content" | "out-file" => ("write_file", "writing files directly"),
            _ => continue,
        };
        if skill_required(tool).is_some_and(|s| loaded.contains(s)) {
            return Some((tool, why));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_detection_separates_declared_action_from_final_answer() {
        assert!(looks_like_unexecuted_intent("Let me verify the sign-in state."));
        assert!(looks_like_unexecuted_intent(
            "Right — browser session ≠ CLI auth. Those are separate, so `gh` is still unauthenticated. Let me set up the device flow; since your browser session is now signed in, I should be able to complete the authorization myself."
        ));
        assert!(looks_like_unexecuted_intent("First, a quick check:"));
        assert!(!looks_like_unexecuted_intent(
            "Done. Git 2.52 and gh 2.91 are installed and on PATH."
        ));
        assert!(!looks_like_unexecuted_intent(&"Summary line. ".repeat(40)));
        assert!(!looks_like_unexecuted_intent(""));
    }

    #[test]
    fn interceptor_routes_only_after_owning_skill_loaded() {
        let fs: HashSet<String> = ["filesystem".to_string()].into_iter().collect();
        let none: HashSet<String> = HashSet::new();
        assert_eq!(route_to_tool("dir C:\\repos", &fs).map(|(t, _)| t), Some("list_dir"));
        assert_eq!(
            route_to_tool("powershell -Command \"Get-ChildItem\" | more", &fs).map(|(t, _)| t),
            Some("list_dir")
        );
        assert_eq!(route_to_tool("type notes.txt", &fs).map(|(t, _)| t), Some("read_file"));
        assert_eq!(route_to_tool("FINDSTR foo *.rs", &fs).map(|(t, _)| t), Some("search_files"));
        assert_eq!(route_to_tool("dir C:\\repos", &none), None);
        assert_eq!(route_to_tool("git grep list_dir", &fs), None);
        assert_eq!(route_to_tool("echo dir", &fs), None);
        assert_eq!(route_to_tool("cargo run", &fs), None);
    }
}
