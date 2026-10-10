use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use crate::elevate::{
    agent_delevate, agent_elevate, agent_run_elevated, elevation_active, is_winapp_call,
};
use crate::exec::{AppNotifier, BgNotifier, NoopNotifier, bg_registry, budget_output, cmd_timeout, exec_or_background, needs_shell, parse_command, spawn_background_notify};
use crate::fileedit::{apply_edit_ops, parse_edit_ops, record_snapshot, seen_snapshot, snapshot_tag};
use crate::log::{fm_tmp_dir, log};
use crate::psrep::{PsRun, ps_session_exec};
use crate::redact::stray_argument;
use crate::route::route_to_tool;
use crate::skills::{discover_skills, parse_frontmatter, resolve_skill_resource, skill_dir};
use crate::tooldefs::skill_required;

/// `app` enables background-job completion announcements; `loaded`
/// is the set of skills loaded into this conversation (the dispatch
/// gate's input).
pub(crate) async fn execute_tool(
    app: &tauri::AppHandle,
    loaded: &std::collections::HashSet<String>,
    name: &str,
    args: &serde_json::Value,
    seen: &mut HashMap<String, String>,
) -> Result<String, String> {
    log(&format!("TOOL CALL: {} args={}", name, args));
    // The elevation tools need the app handle (managed state,
    // window events), so they dispatch here, not in the headless core.
    let result = match name {
        "elevate" | "delevate" | "run_elevated" => execute_elevation(app, name, args).await,
        _ => {
            let notifier = std::sync::Arc::new(AppNotifier(app.clone()));
            execute_tool_notify(Some(app), notifier, loaded, name, args).await
        }
    };
    let summary = match &result {
        Ok(s) => format!("OK len={}", s.len()),
        Err(e) => format!("ERR {}", e.chars().take(200).collect::<String>()),
    };
    log(&format!("TOOL RESULT: {} -> {}", name, summary));
    result.map(|r| budget_output(seen, format!("{}{}", name, args), r))
}

/// Test / headless convenience: completions are never announced and
/// no skill is loaded, so gated tools refuse.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn execute_tool_inner(name: &str, args: &serde_json::Value) -> Result<String, String> {
    execute_tool_notify(
        None,
        std::sync::Arc::new(NoopNotifier),
        &std::collections::HashSet::new(),
        name,
        args,
    )
    .await
}

/// A token that survived detokenize was never this run's handle, so the tool
/// would run on a name the disk does not have. Authored content is checked on
/// the same call: a handle written into a file is a wrong value nobody sees.
fn refuse_stray_handle(args: &serde_json::Value) -> Result<(), String> {
    let Some(key) = stray_argument(args) else {
        return Ok(());
    };
    let place = if key.is_empty() {
        "an argument".to_string()
    } else {
        format!("the argument `{key}`")
    };
    Err(format!(
        "Blocked: {place} carries a redaction handle this run cannot restore. Re-run the call \
         that produced the value, then use the real path from its result."
    ))
}

async fn execute_elevation(
    app: &tauri::AppHandle,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    refuse_stray_handle(args)?;
    match name {
        "elevate" => {
            let a = app.clone();
            tokio::task::spawn_blocking(move || agent_elevate(&a))
                .await
                .map_err(|e| format!("elevate task crashed: {e}"))?
        }
        "delevate" => agent_delevate(app),
        "run_elevated" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if command.is_empty() {
                return Err("run_elevated needs a command".into());
            }
            let secs = args
                .get("timeout_secs")
                .and_then(|s| s.as_u64())
                .unwrap_or(120);
            agent_run_elevated(app.clone(), command.to_string(), std::time::Duration::from_secs(secs))
                .await
        }
        _ => Err(format!("unknown elevation tool: {name}")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_notify(
    app: Option<&tauri::AppHandle>,
    notifier: std::sync::Arc<dyn BgNotifier>,
    loaded: &std::collections::HashSet<String>,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    refuse_stray_handle(args)?;
    // This is the dispatch gate, a client policy rather than a spec rule. A
    // tool whose usage rules live in a skill's instructions is refused until
    // that skill is loaded.
    if let Some(skill) = skill_required(name) {
        if !loaded.contains(skill) {
            return Err(format!(
                "tool '{name}' requires the '{skill}' skill: call load_skill(\"{skill}\"), follow its instructions, then call {name} again"
            ));
        }
    }
    match name {
        "run_command" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if let Some((tool, why)) = route_to_tool(command, loaded) {
                return Err(format!(
                    "Blocked: {tool} answers this command ({why}: structured output, no shell \
                     quoting, no truncation). Call {tool} instead. Command: `{}`",
                    command.chars().take(160).collect::<String>()
                ));
            }
            if let Some(app) = app {
                if elevation_active(app) && is_winapp_call(command) {
                    log("RUN winapp via elevated session");
                    return agent_run_elevated(
                        app.clone(),
                        command.to_string(),
                        cmd_timeout(),
                    )
                    .await;
                }
            }
            let background = args
                .get("background")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            let shell = args.get("shell").and_then(|s| s.as_str()).unwrap_or("cmd");
            let cwd = args.get("cwd").and_then(|c| c.as_str());
            if let Some(cwd) = cwd {
                if !std::path::Path::new(cwd).is_dir() {
                    return Err(format!("cwd is not a directory: {cwd}"));
                }
            }
            if shell == "powershell" && !background {
                // The persistent session is the preferred path.
                // Variables, functions, and cwd carry across tool
                // calls, and each call skips a ~0.5s powershell
                // startup.
                match ps_session_exec(command, cwd.map(str::to_string), cmd_timeout()).await {
                    PsRun::Done { stdout, code, stderr } => {
                        return if code == 0 {
                        // Non-terminating errors exit 0 and appear
                        // only on stderr. Surface stderr; never
                        // swallow an error.
                            if stderr.trim().is_empty() {
                                Ok(stdout)
                            } else {
                                Ok(format!("{}\n--- stderr ---\n{}", stdout, stderr))
                            }
                        } else {
                            Err(format!("exit code {code}: {}\n{}", stdout, stderr))
                        };
                    }
                    PsRun::TimedOut { partial, note } => {
                        return Err(format!(
                            "{note}; session killed; partial output (NOT re-run — side effects may already have happened):\n{partial}"
                        ));
                    }
                    PsRun::Broken { note } => {
                        log(&format!("PS session unusable ({note}); one-shot powershell -File instead"));
                    }
                }
            }
            let mut script_path: Option<std::path::PathBuf> = None;
            let mut cmd = if shell == "powershell" {
                // The command is DATA, never a quoted argv fragment. Write the
                // command verbatim to a scratch .ps1 and run it with -File. The
                // UTF-8 BOM makes Windows PowerShell 5.1 decode non-ASCII. Zero
                // quoting layers remove the quote-nesting failure class that
                // spans cmd, powershell, and Add-Type.
                let script = fm_tmp_dir().join(format!(
                    "cmd-{}-{}.ps1",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
                let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
                bytes.extend_from_slice(command.as_bytes());
                std::fs::write(&script, &bytes)
                    .map_err(|e| format!("cannot write scratch script: {e}"))?;
                log(&format!(
                    "RUN powershell -File {} ({} bytes of script)",
                    script.display(),
                    command.len()
                ));
                script_path = Some(script.clone());
                let mut c = tokio::process::Command::new("powershell.exe");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
                c.arg(&script);
                c
            } else if needs_shell(command) {
                // /S /C "..." strips only the outermost quotes, so
                // inner quotes survive; plain /C mangles them.
                // raw_arg keeps Rust from re-escaping the string.
                log(&format!("RUN via cmd shell: {}", command));
                let mut c = tokio::process::Command::new("cmd.exe");
                c.raw_arg("/S")
                    .raw_arg("/C")
                    .raw_arg(format!("\"{}\"", command));
                c
            } else {
                // This branch parses quotes itself, so multi-word
                // quoted arguments reach the program intact.
                let argv = parse_command(command);
                let (program, rest) = match argv.split_first() {
                    Some((p, r)) => (p, r),
                    None => return Err("empty command".into()),
                };
                log(&format!("RUN direct: {} {:?}", program, rest));
                let mut c = tokio::process::Command::new(program);
                c.args(rest);
                c
            };
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            if background {
                return spawn_background_notify(notifier, cmd, command).await;
            }
            exec_or_background(notifier, cmd, command, script_path).await
        }
        "get_command_output" => {
            let pid = args.get("pid").and_then(|p| p.as_u64()).ok_or("pid required")? as u32;
            let reg = bg_registry().lock().unwrap();
            let job = reg
                .get(&pid)
                .ok_or_else(|| format!("no background job with pid {pid}"))?;
            let status = job.status.lock().unwrap().clone();
            let out = String::from_utf8_lossy(&job.stdout.lock().unwrap()).to_string();
            let err = String::from_utf8_lossy(&job.stderr.lock().unwrap()).to_string();
            Ok(format!(
                "pid {pid} [{}] status: {status}, {}s elapsed\n--- stdout ---\n{out}\n--- stderr ---\n{err}",
                job.command,
                job.started.elapsed().as_secs()
            ))
        }
        "kill_command" => {
            let pid = args.get("pid").and_then(|p| p.as_u64()).ok_or("pid required")? as u32;
            let out = tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!(
                    "taskkill failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            log(&format!("KILLED background pid {pid}"));
            Ok(format!("killed pid {pid} and its process tree"))
        }
        "read_file" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let bytes = tokio::fs::read(path).await.map_err(|e| format!("read {path}: {e}"))?;
            if bytes.iter().take(8192).any(|b| *b == 0) {
                return Err(format!(
                    "{path} is binary (NUL byte in first 8 KiB); read_file refuses. \
                     Use run_command for binary inspection."
                ));
            }
            let tag = snapshot_tag(&bytes);
            record_snapshot(path, &tag);
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.split('\n').collect();
            let total = lines.len();
            // The offset is a 1-based inclusive start line.
            let start = args
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|v| v.max(1) as usize)
                .unwrap_or(1);
            if start > total {
                return Err(format!(
                    "offset {start} is past the end of {path} ({total} lines)"
                ));
            }
            let count = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v.max(1) as usize)
                .unwrap_or(2000);
            let end = start.saturating_add(count).saturating_sub(1).min(total);
            let mut out = format!("[{path}#{tag}]\n");
            for (i, line) in lines[start - 1..end].iter().enumerate() {
                out.push_str(&format!("{}:{}\n", start + i, line));
            }
            if end < total {
                out.push_str(&format!(
                    "[showing lines {start}-{end} of {total}; use offset {} to continue]",
                    end + 1
                ));
            }
            Ok(out)
        }
        "write_file" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if tokio::fs::try_exists(path).await.unwrap_or(false) {
                // Overwrite guard: the model must have read this
                // file and must echo its current snapshot tag. The
                // error must not leak the current tag; a leaked tag
                // would defeat the read requirement.
                let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
                let current = snapshot_tag(&bytes);
                let tag = args.get("tag").and_then(|c| c.as_str()).unwrap_or("");
                if tag != current || seen_snapshot(path).as_deref() != Some(tag) {
                    return Err(format!(
                        "{path} already exists and this write would replace its entire \
                         content. read_file it first, then re-issue write_file with the \
                         tag from its [path#TAG] header (or use edit_file for partial \
                         changes). If the tag you have is rejected, the file changed \
                         since your read - read it again."
                    ));
                }
            }
            tokio::fs::write(path, content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            let new_tag = snapshot_tag(content.as_bytes());
            record_snapshot(path, &new_tag);
            Ok(format!(
                "wrote {} bytes to {path}\n[{path}#{new_tag}]",
                content.len()
            ))
        }
        "edit_file" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let tag = args.get("tag").and_then(|c| c.as_str()).unwrap_or("");
            let ops = args.get("ops").and_then(|c| c.as_str()).unwrap_or("");
            let bytes = tokio::fs::read(path).await.map_err(|e| format!("read {path}: {e}"))?;
            if bytes.iter().take(8192).any(|b| *b == 0) {
                return Err(format!("{path} is binary; edit_file refuses"));
            }
            let current = snapshot_tag(&bytes);
            if tag != current || seen_snapshot(path).as_deref() != Some(tag) {
                return Err(format!(
                    "edit refused for {path}: tag '{tag}' is not the snapshot you last saw \
                     (file's current tag is #{current}). read_file it again, re-anchor the \
                     line numbers, and retry with the new tag."
                ));
            }
            let ops = parse_edit_ops(ops)?;
            let text = String::from_utf8_lossy(&bytes);
            let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
            let total = lines.len().max(1);
            let (summaries, diff) = apply_edit_ops(&mut lines, &ops, total)?;
            let new_text = lines.join("\n");
            tokio::fs::write(path, &new_text)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            let new_tag = snapshot_tag(new_text.as_bytes());
            record_snapshot(path, &new_tag);
            Ok(format!(
                "edited {path}: {}\n```diff\n{}\n```\n[{path}#{new_tag}] - line numbers \
                 shifted; re-read before anchoring another edit unless you are certain \
                 of the layout.",
                summaries.join("; "),
                diff.join("\n")
            ))
        }
        "list_dir" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let mut entries = Vec::new();
            let mut dir = tokio::fs::read_dir(path).await.map_err(|e| e.to_string())?;
            while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
                let name = entry.file_name().to_string_lossy().to_string();
                let p = entry.path();
                let is_dir = tokio::fs::metadata(&p).await.map(|m| m.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{name}/") } else { name });
            }
            Ok(entries.join("\n"))
        }
        "search_files" => {
            let pattern = args
                .get("pattern")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_lowercase();
            if pattern.is_empty() {
                return Err("pattern is required".into());
            }
            let root = args.get("root").and_then(|c| c.as_str()).unwrap_or("C:\\");
            let mut out = Vec::new();
            search_files_walk(std::path::Path::new(root), &pattern, 8, &mut out).await;
            if out.is_empty() {
                Ok(format!("no files or directories matching '{pattern}' under {root}"))
            } else {
                Ok(out.join("\n"))
            }
        }
        "list_processes" => {
            let output = tokio::process::Command::new("tasklist")
                .args(["/FO", "CSV", "/NH"])
                .output()
                .await
                .map_err(|e| e.to_string())?;
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        "load_skill" => {
            let name = args.get("name").and_then(|c| c.as_str()).unwrap_or("");
            let dir = skill_dir(name).ok_or_else(|| {
                let avail: Vec<String> = discover_skills().into_iter().map(|s| s.name).collect();
                format!("no such skill: {name} (available: {})", avail.join(", "))
            })?;
            let md = tokio::fs::read_to_string(dir.join("SKILL.md"))
                .await
                .map_err(|e| e.to_string())?;
            let (_, _, body) = parse_frontmatter(&md);
            Ok(body)
        }
        "read_skill_resource" => {
            let path = args.get("path").and_then(|c| c.as_str()).unwrap_or("");
            let file = resolve_skill_resource(path)?;
            tokio::fs::read_to_string(&file)
                .await
                .map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

fn search_files_walk<'a>(
    root: &'a std::path::Path,
    pattern: &'a str,
    depth: u32,
    out: &'a mut Vec<String>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        const CAP: usize = 200;
        if depth == 0 || out.len() >= CAP {
            return;
        }
        let Ok(mut dir) = tokio::fs::read_dir(root).await else {
            return;
        };
        while let Ok(Some(entry)) = dir.next_entry().await {
            if out.len() >= CAP {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if name.to_lowercase().contains(pattern) {
                let p = entry.path().to_string_lossy().to_string();
                out.push(if ft.is_dir() { format!("{p}/") } else { p });
            }
            if ft.is_dir() {
                search_files_walk(&entry.path(), pattern, depth - 1, out).await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psrep::ps_session_shutdown;

    #[tokio::test]
    async fn powershell_shell_mode_survives_quote_hell() {
        // This payload mixes a single-quoted string with an escaped
        // quote, embedded double quotes, and $ interpolation. The
        // payload survives only as script DATA with zero quoting
        // layers.
        let cmd = "$name = 'world''s \"best\"'\nWrite-Output \"hello $name\"\n";
        let out = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": cmd, "shell": "powershell" }),
        )
        .await
        .expect("powershell -File run");
        assert!(
            out.contains("hello world's \"best\""),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn cwd_is_validated_and_applied() {
        let bad = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "cd", "cwd": "C:\\no-such-dir-fm" }),
        )
        .await;
        assert!(bad.unwrap_err().contains("cwd is not a directory"));
        let ok = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "cd", "cwd": "C:\\Windows" }),
        )
        .await
        .unwrap();
        assert_eq!(ok.trim().to_lowercase(), "c:\\windows");
    }

    #[tokio::test]
    async fn run_command_blocks_dup_after_owning_skill_loaded() {
        let fs: std::collections::HashSet<String> =
            ["filesystem".to_string()].into_iter().collect();
        let err = execute_tool_notify(
            None,
            std::sync::Arc::new(NoopNotifier),
            &fs,
            "run_command",
            &serde_json::json!({ "command": "dir" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Blocked") && err.contains("list_dir"), "{err}");
        let ok = execute_tool_inner("run_command", &serde_json::json!({ "command": "echo dir" }))
            .await
            .unwrap();
        assert!(ok.contains("dir"), "{ok}");
    }

    #[tokio::test]
    async fn a_handle_this_run_cannot_restore_is_refused_before_dispatch() {
        let err = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "Get-ChildItem ⟦USER_PATH_1⟧" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("redaction handle"), "{err}");
    }

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("fm-test-{}-{name}", std::process::id()))
            .to_string_lossy()
            .to_string()
    }

    #[tokio::test]
    async fn read_file_stamps_tag_and_line_numbers() {
        let p = temp_path("read1.txt");
        std::fs::write(&p, "alpha\nbravo\ncharlie\ndelta\n").unwrap();
        let out = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap();
        let tag = out.lines().next().unwrap();
        assert!(tag.starts_with('[') && tag.contains("#") && tag.ends_with(']'), "{tag}");
        assert!(out.contains("1:alpha") && out.contains("4:delta"), "{out}");
        let win = fs_call(
            "read_file",
            serde_json::json!({ "path": &p, "offset": 2, "limit": 2 }),
        )
        .await
        .unwrap();
        assert!(win.contains("2:bravo") && win.contains("3:charlie"), "{win}");
        assert!(!win.contains("1:alpha") && !win.contains("4:delta"), "{win}");
        assert!(win.contains("use offset 4 to continue"), "{win}");
        let again = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap();
        assert_eq!(again.lines().next().unwrap(), tag);
        std::fs::write(&p, "alpha\nbravo!\ncharlie\ndelta\n").unwrap();
        let after = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap();
        assert_ne!(after.lines().next().unwrap(), tag);
        // Stale offset is an error, not an empty read.
        let err = fs_call("read_file", serde_json::json!({ "path": &p, "offset": 99 }))
            .await
            .unwrap_err();
        assert!(err.contains("past the end"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[tokio::test]
    async fn read_file_refuses_binary() {
        let p = temp_path("binary.bin");
        std::fs::write(&p, [0x48u8, 0, 0x69, 0]).unwrap();
        let err = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap_err();
        assert!(err.contains("binary"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    fn fs_loaded() -> std::collections::HashSet<String> {
        ["filesystem".to_string()].into_iter().collect()
    }

    fn tag_of(out: &str) -> String {
        let h = out.lines().find(|l| l.starts_with('[')).unwrap();
        let start = h.find('#').unwrap() + 1;
        h[start..start + 4].to_string()
    }

    async fn fs_call(name: &str, args: serde_json::Value) -> Result<String, String> {
        execute_tool_notify(None, std::sync::Arc::new(NoopNotifier), &fs_loaded(), name, &args).await
    }

    #[tokio::test]
    async fn edit_file_applies_ops_against_snapshot_tag() {
        let p = temp_path("edit1.txt");
        std::fs::write(&p, "l1\nl2\nl3\nl4\n").unwrap();
        let read = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap();
        let tag = tag_of(&read);
        let res = fs_call(
            "edit_file",
            serde_json::json!({
                "path": &p, "tag": &tag,
                "ops": "PUT 4.=4:\n+FOUR\nCUT 2.=3\nPUT <1:\n+HEAD\n"
            }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "HEAD\nl1\nFOUR\n");
        // The result carries the effect as a diff fence, in file order.
        assert!(
            res.contains("```diff\n+HEAD\n-l2\n-l3\n-l4\n+FOUR\n```"),
            "{res}"
        );
        let new_tag = tag_of(&res);
        let again = fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap();
        assert_eq!(tag_of(&again), new_tag);
        let err = fs_call(
            "edit_file",
            serde_json::json!({ "path": &p, "tag": &tag, "ops": "CUT 1.=1" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("read_file"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[tokio::test]
    async fn edit_file_rejects_bad_ops() {
        let p = temp_path("edit2.txt");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let tag = tag_of(&fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap());
        let err = fs_call(
            "edit_file",
            serde_json::json!({ "path": &p, "tag": &tag, "ops": "CUT 9.=10" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("the file has"), "{err}");
        let err = fs_call(
            "edit_file",
            serde_json::json!({ "path": &p, "tag": &tag, "ops": "PUT 1.=2:\n+x\nCUT 2.=3" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("overlap"), "{err}");
        let err = fs_call(
            "edit_file",
            serde_json::json!({ "path": &p, "tag": &tag, "ops": "REPLACE 1 with x" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown op"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nb\nc\n");
        std::fs::remove_file(&p).ok();
    }

    #[tokio::test]
    async fn write_file_requires_read_before_overwrite() {
        let p = temp_path("guard.txt");
        std::fs::remove_file(&p).ok();
        let res = fs_call(
            "write_file",
            serde_json::json!({ "path": &p, "content": "v1\n" }),
        )
        .await
        .unwrap();
        assert!(res.contains("wrote 3 bytes"), "{res}");
        let err = fs_call(
            "write_file",
            serde_json::json!({ "path": &p, "content": "v2\n" }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("read_file") && !err.contains(&tag_of(&res)), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v1\n");
        let tag = tag_of(&fs_call("read_file", serde_json::json!({ "path": &p }))
            .await
            .unwrap());
        fs_call(
            "write_file",
            serde_json::json!({ "path": &p, "content": "v2\n", "tag": &tag }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2\n");
        std::fs::write(&p, "external\n").unwrap();
        let err = fs_call(
            "write_file",
            serde_json::json!({ "path": &p, "content": "v3\n", "tag": &tag }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("changed"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    /// Records completion announcements so the test can observe what the
    /// production path would emit as a `tool_result` event.
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<(u32, String, String)>>);

    impl BgNotifier for Recorder {
        fn announce(&self, pid: u32, _cmd: &str, status: &str, out: &str, _err: &str) {
            self.0.lock().unwrap().push((pid, status.to_string(), out.to_string()));
        }
    }

    /// One replay of a full agent session against the real machine. Skill-gated
    /// tools execute. The persistent PowerShell session keeps state and cwd. The
    /// interceptor refuses duplicated shell. The read/edit/write guard chain
    /// holds. A slow command auto-backgrounds, and the completion is announced.
    #[tokio::test]
    async fn composite_agent_session_replay() {
        let dir = std::env::temp_dir().join(format!("fm-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dpath = dir.to_string_lossy().to_string();
        let fpath = dir.join("notes.md").to_string_lossy().to_string();

        // The filesystem skill is loaded in this run, so its gated
        // tools execute. The refusal path has its own test.
        let loaded: std::collections::HashSet<String> =
            ["filesystem".to_string()].into_iter().collect();
        let loaded = &loaded;
        let rec = std::sync::Arc::new(Recorder::default());
        let n: std::sync::Arc<dyn BgNotifier> = rec.clone();
        let call = |name: &str, args: serde_json::Value| {
            let n = n.clone();
            let name = name.to_string();
            async move { execute_tool_notify(None, n, loaded, &name, &args).await }
        };

        call(
            "run_command",
            serde_json::json!({ "command": "$x = 21 * 2", "shell": "powershell", "cwd": &dpath }),
        )
        .await
        .unwrap();
        let r = call(
            "run_command",
            serde_json::json!({
                "command": "echo \"x=$x pwd=$((Get-Location).Path)\"", "shell": "powershell"
            }),
        )
        .await
        .unwrap();
        assert!(r.contains("x=42") && r.to_lowercase().contains("fm-e2e"), "{r}");

        let err = call("run_command", serde_json::json!({ "command": "dir" }))
            .await
            .unwrap_err();
        assert!(err.contains("Blocked") && err.contains("list_dir"), "{err}");

        let w = call(
            "write_file",
            serde_json::json!({ "path": &fpath, "content": "# Notes\nline two\nline three\n" }),
        )
        .await
        .unwrap();
        assert!(w.contains("wrote 28 bytes"), "{w}");
        let r = call("read_file", serde_json::json!({ "path": &fpath }))
            .await
            .unwrap();
        assert!(r.contains("2:line two"), "{r}");
        let t = tag_of(&r);
        let e = call(
            "edit_file",
            serde_json::json!({ "path": &fpath, "tag": &t, "ops": "PUT 2.=2:\n+line TWO\n" }),
        )
        .await
        .unwrap();
        assert!(e.contains("PUT 2.2"), "{e}");
        assert_eq!(
            std::fs::read_to_string(&fpath).unwrap(),
            "# Notes\nline TWO\nline three\n"
        );
        let err = call(
            "write_file",
            serde_json::json!({ "path": &fpath, "content": "gone\n", "tag": &t }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("changed") || err.contains("read_file"), "{err}");
        assert!(std::fs::read_to_string(&fpath).unwrap().contains("line TWO"));

        std::env::set_var("FIRSTMATE_AUTO_BG_SECS", "1");
        let r = call(
            "run_command",
            serde_json::json!({ "command": "ping -n 30 127.0.0.1" }),
        )
        .await
        .unwrap();
        std::env::remove_var("FIRSTMATE_AUTO_BG_SECS");
        assert!(r.contains("moved to background") && r.contains("do NOT poll"), "{r}");
        let pid: u32 = r
            .split("pid ")
            .nth(1)
            .unwrap()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap();
        let out = call("get_command_output", serde_json::json!({ "pid": pid }))
            .await
            .unwrap();
        assert!(out.contains("running"), "{out}");
        call("kill_command", serde_json::json!({ "pid": pid }))
            .await
            .unwrap();
        // A killed job must also announce; the model learns the job stopped.
        let mut kill_noted = false;
        for _ in 0..80 {
            if !rec.0.lock().unwrap().is_empty() {
                kill_noted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert!(kill_noted, "killed job must announce its exit");
        rec.0.lock().unwrap().clear();

        let r = call(
            "run_command",
            serde_json::json!({ "command": "ping -n 2 127.0.0.1", "background": true }),
        )
        .await
        .unwrap();
        assert!(r.contains("pid"), "{r}");
        let mut announced = None;
        for _ in 0..80 {
            if let Some(x) = rec.0.lock().unwrap().first() {
                announced = Some(x.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let (apid, status, out) = announced.expect("completion must be announced");
        let bpid: u32 = r
            .split("pid ")
            .nth(1)
            .unwrap()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap();
        assert_eq!(apid, bpid);
        assert!(status.starts_with("exited: 0"), "{status}");
        assert!(out.contains("TTL="), "{out}");

        ps_session_shutdown().await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn gated_tool_refuses_until_its_skill_is_loaded() {
        let err = execute_tool_inner("list_dir", &serde_json::json!({ "path": "." }))
            .await
            .expect_err("filesystem tools must refuse before the skill loads");
        assert!(
            err.contains("requires the 'filesystem' skill") && err.contains("load_skill"),
            "{err}"
        );
        let ok = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "echo gated-ok" }),
        )
        .await
        .unwrap();
        assert!(ok.contains("gated-ok"), "{ok}");
    }
}
