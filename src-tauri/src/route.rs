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
/// conversation. Before activation the model has not seen that tool's rules,
/// so the shell command passes through untouched.
///
/// A block refuses the whole command, so the gate fires only when one
/// statement is exactly the dedicated tool's job. A wrong block costs the agent
/// a turn and the command's other work; a miss costs one shell call.
/// Returns `(tool, why)`.
pub(crate) fn route_to_tool(command: &str, loaded: &HashSet<String>) -> Option<(&'static str, &'static str)> {
    let normalized = command.to_lowercase().replace("&&", ";");
    let statements: Vec<&str> = normalized
        .split([';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if statements.len() != 1 {
        return None;
    }
    let statement = statements[0];
    if let Some(hit) = pipeline_hit(statement, loaded) {
        return Some(hit);
    }
    // A shell host hides the program from program position, so its quoted
    // payload gets the same test.
    if SHELL_HOSTS.iter().any(|host| statement.starts_with(host)) {
        return quoted_payloads(statement).find_map(|payload| route_to_tool(payload, loaded));
    }
    None
}

/// Presentation stages: they change how output looks, never what it says.
const PRESENTATION: &[&str] = &[
    "format-table", "ft", "format-list", "fl", "format-wide", "out-string", "out-default",
    "ogv", "sort-object", "sort", "select-object",
];

/// Drives that hold no files, so `list_dir`, `read_file`, and `search_files`
/// cannot answer for them.
const NON_FILE_STORES: &[&str] = &[
    "hklm:", "hkcu:", "hkcr:", "hku:", "hky:", "registry::", "cert:", "variable:", "function:",
    "alias:",
];

const SHELL_HOSTS: &[&str] = &["powershell ", "pwsh ", "cmd /c", "cmd /k", "cmd "];

/// The duplicate only when the pipeline head is the duplicated program, the
/// store is a file store, and every later stage is presentation. A stage that
/// measures, groups, or filters yields a number or a subset the tool cannot
/// return, and a computed property (`@{`) builds a column from file metadata
/// the tool does not report.
fn pipeline_hit(
    statement: &str,
    loaded: &HashSet<String>,
) -> Option<(&'static str, &'static str)> {
    let mut stages = statement.split('|');
    let head = stages.next()?;
    let token = head.split_whitespace().next()?.trim_end_matches(".exe");
    let (tool, why): (&'static str, &'static str) = match token {
        "get-childitem" | "dir" | "ls" => ("list_dir", "enumerating directories"),
        "get-content" | "type" | "cat" | "more" => {
            ("read_file", "reading files with ranges and line numbers")
        }
        "select-string" | "findstr" => ("search_files", "searching file contents"),
        "get-process" | "tasklist" => ("list_processes", "listing processes"),
        "set-content" | "out-file" => ("write_file", "writing files directly"),
        _ => return None,
    };
    if !skill_required(tool).is_some_and(|s| loaded.contains(s)) {
        return None;
    }
    if NON_FILE_STORES.iter().any(|store| statement.contains(store)) || statement.contains("@{") {
        return None;
    }
    for stage in stages {
        let Some(first) = stage.split_whitespace().next() else {
            continue;
        };
        if !PRESENTATION.contains(&first) {
            return None;
        }
    }
    Some((tool, why))
}

/// The quoted payloads of a statement, so `cmd /c "dir"` reaches the program.
fn quoted_payloads(statement: &str) -> impl Iterator<Item = &str> {
    statement.split(['"', '\'']).skip(1).step_by(2)
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

    #[test]
    fn a_command_that_measures_or_filters_is_not_an_enumeration() {
        let fs: HashSet<String> = ["filesystem".to_string()].into_iter().collect();
        for command in [
            r"Get-ChildItem C:\ -Recurse -File | Measure-Object -Property Length -Sum",
            r"Get-ChildItem C:\ -File | Select-Object Name, @{n='GB';e={$_.Length/1GB}}",
            r"Get-ChildItem C:\ | Where-Object { $_.Length -gt 1GB }",
            r"Get-ChildItem 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion' | Select-Object PSChildName",
            "Get-ChildItem cert: | Format-Table",
            r"Get-Process | Where-Object { $_.Handles -gt 1000 }",
        ] {
            assert_eq!(route_to_tool(command, &fs), None, "{command} yields more than names");
        }
    }

    #[test]
    fn a_multi_statement_command_is_not_blocked_for_one_segment() {
        let fs: HashSet<String> = ["filesystem".to_string()].into_iter().collect();
        for command in [
            r"wsl -l -v | Out-String -Width 200; Get-ChildItem $env:LOCALAPPDATA\wsl -Directory | Select-Object -ExpandProperty Name",
            "$roots = Get-ChildItem C:\\ -Directory -Force\nforeach ($d in $roots) { $sum = (Get-ChildItem $d.FullName -Recurse -File | Measure-Object -Property Length -Sum).Sum }",
            r"robocopy C:\repos \__nowhere__ /L /E /BYTES | Select-String '^\s+Bytes\s*:'",
            r"& (Join-Path $HOME 'du.ps1') -Root C:\ | Out-File (Join-Path $HOME 'report.txt')",
        ] {
            assert_eq!(route_to_tool(command, &fs), None, "{command} does other work");
        }
    }

    #[test]
    fn a_pure_listing_still_routes_through_the_tool() {
        let fs: HashSet<String> = ["filesystem".to_string()].into_iter().collect();
        for command in [
            r"Get-ChildItem C:\repos -Recurse",
            r"Get-ChildItem C:\repos | Sort-Object Name | Select-Object -First 5",
            r##"cmd /c "dir C:\repos""##,
            r"Get-Process | Sort-Object CPU -Descending",
        ] {
            assert!(
                route_to_tool(command, &fs).is_some(),
                "{command} is what the tool does"
            );
        }
    }
}
