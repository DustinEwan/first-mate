use std::collections::HashMap;
use tauri::Emitter;
use crate::fileedit::line_diff;
use crate::log::{fm_out_dir, log};
use crate::report::{ReportBody, ToolReport};

/// Split a command line into arguments, respecting double quotes.
/// `winapp ui search "Qwen 3.8 Flash Next" -a zen`
///   -> ["winapp", "ui", "search", "Qwen 3.8 Flash Next", "-a", "zen"]
pub(crate) fn parse_command(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            ' ' | '\t' if !in_quotes => {
                if started {
                    args.push(current.clone());
                    current.clear();
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// True when the command uses shell features (pipes, `&&`, redirection,
/// cmd built-ins) that require running through `cmd.exe` instead of
/// launching the program directly.
pub(crate) fn needs_shell(cmd: &str) -> bool {
    if cmd.contains(['|', '&', '>', '<', '^', '%', '(', ')']) {
        return true;
    }
    matches!(
        cmd.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str(),
        "if" | "for" | "echo" | "set" | "dir" | "copy" | "move" | "del" | "erase"
            | "type" | "cd" | "chdir" | "mkdir" | "md" | "rmdir" | "rd" | "ren"
            | "rename" | "start" | "call" | "pause" | "timeout" | "exit" | "shift"
            | "setlocal" | "endlocal" | "pushd" | "popd" | "path" | "rem" | "goto"
    )
}

/// Spill names are millisecond-granular, so two spills in the same millisecond
/// (parallel tool calls, or the test harness) would write the same path and
/// silently hand back the other call's file; this counter makes each name unique.
static SPILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cap tool output over `max` chars: spill the FULL text to
/// `~/.firstmate/output/` and return head + pointer + tail. Nothing is
/// unrecoverably lost — `read_file` the pointer path for any omitted middle.
fn truncate_if_over(s: String, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s;
    }
    let head: String = s.chars().take(max * 3 / 4).collect();
    let tail: String = s.chars().skip(count - max / 4).collect();
    let path = fm_out_dir().join(format!(
        "out-{}-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        SPILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let omitted = count - head.chars().count() - tail.chars().count();
    let pointer = if std::fs::write(&path, &s).is_ok() {
        format!(
            "\n...[{omitted} of {count} chars omitted; FULL output saved: {} — read_file path=\"{}\" offset=<line> for the middle]...\n",
            path.display(),
            path.display()
        )
    } else {
        format!("\n...[{omitted} chars truncated; spill write failed]...\n")
    };
    log(&format!("BUDGET: spilled {count} chars to {}", path.display()));
    format!("{head}{pointer}{tail}")
}

/// Keep tool output from exploding the context. An identical repeat result
/// collapses to a pointer; a changed repeat returns only the line diff
/// against the previous output; oversized output is truncated. `seen` maps
/// tool call -> full result, per chat run.
pub(crate) fn budget_output(seen: &mut HashMap<String, String>, key: String, result: String) -> String {
    const MAX: usize = 8000;
    if let Some(prev) = seen.get(&key) {
        if *prev == result {
            log(&format!("BUDGET: deduped identical result ({} bytes)", result.len()));
            seen.insert(key, result.clone());
            return format!(
                "[identical to your earlier call: {} bytes, unchanged — the state has not changed]",
                result.len()
            );
        }
        let diff = line_diff(prev, &result);
        let out = if diff.trim().is_empty() {
            "[no visible line changes vs your earlier call]".to_string()
        } else if diff.chars().count() >= result.chars().count() {
            result.clone() // wholesale change: a diff would save nothing
        } else {
            log(&format!(
                "BUDGET: returned line diff instead of full {}-byte result",
                result.len()
            ));
            format!("[changes vs your earlier call]\n{}", diff)
        };
        seen.insert(key, result);
        return truncate_if_over(out, MAX);
    }
    seen.insert(key, result.clone());
    truncate_if_over(result, MAX)
}

/// Hard cap for one run_command. Detached grandchildren (e.g. `start /b
/// gh auth login`) inherit the output pipes and can hold them open forever;
/// without a cap the agent loop soft-locks waiting for EOF.
/// Override with FIRSTMATE_CMD_TIMEOUT_SECS.
pub(crate) fn cmd_timeout() -> std::time::Duration {
    let secs = std::env::var("FIRSTMATE_CMD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(300);
    std::time::Duration::from_secs(secs)
}

/// Spawn with a deadline. On timeout the direct child dies (kill_on_drop),
/// the process tree is swept, and an actionable error is returned - a tool
/// call must always come back so the agent loop can never wedge.
#[cfg_attr(not(test), allow(dead_code))]
async fn exec_capped(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    // wait_with_output only captures streams piped at spawn time; the old
    // Command::output() set this implicitly, spawn() does not.
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().unwrap_or(0);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("spawn failed: {e}")),
        Err(_) => {
            if pid > 0 {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
            }
            Err(format!(
                "command timed out after {}s; killed pid {pid} and its tree. \
                 Detached grandchildren may still run (taskkill /F /PID them) \
                 and their partial output is lost.",
                timeout.as_secs()
            ))
        }
    }
}

/// A command the agent started with `background: true`. The app owns the
/// pipes and drains them continuously, so the child can never block on a
/// full pipe or hold the agent loop hostage; the agent drives the lifecycle
/// via get_command_output / kill_command.
pub(crate) struct BgJob {
    pub(crate) command: String,
    pub(crate) stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    pub(crate) stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    pub(crate) status: std::sync::Arc<std::sync::Mutex<String>>,
    pub(crate) started: std::time::Instant,
}

pub(crate) const MAX_BG_OUTPUT: usize = 256 * 1024;

const MAX_BG_JOBS: usize = 32;

static BG_REGISTRY: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u32, BgJob>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn bg_registry() -> &'static std::sync::Mutex<std::collections::HashMap<u32, BgJob>> {
    &BG_REGISTRY
}

/// Drain a pipe into the shared buffer, keeping the tail when it overflows.
fn pump_bg_output<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut b = buf.lock().unwrap();
                    b.extend_from_slice(&chunk[..n]);
                    if b.len() > MAX_BG_OUTPUT {
                        let cut = b.len() - MAX_BG_OUTPUT;
                        b.drain(..cut);
                    }
                }
            }
        }
    });
}

/// Evict oldest finished jobs first so the registry cannot grow unbounded.
fn evict_bg_jobs(reg: &mut std::collections::HashMap<u32, BgJob>) {
    if reg.len() < MAX_BG_JOBS {
        return;
    }
    let mut by_age: Vec<(u32, bool, std::time::Instant)> = reg
        .iter()
        .map(|(pid, j)| {
            let running = j.status.lock().unwrap().starts_with("running");
            (*pid, running, j.started)
        })
        .collect();
    // Finished and old die first; running jobs survive longest.
    by_age.sort_by_key(|(_, running, started)| (*running, *started));
    for (pid, _, _) in by_age.iter().skip(MAX_BG_JOBS - 1) {
        reg.remove(pid);
    }
}

/// Foreground grace period before a command is auto-backgrounded. Beyond
/// this, waiting is a wasted agent turn: the job goes to the background and
/// its completion is announced in the conversation ledger by itself.
fn auto_bg_secs() -> u64 {
    std::env::var("FIRSTMATE_AUTO_BG_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(20)
}

/// Announces a finished background job into the conversation: the frontend
/// appends it as a ledger line, so the model sees the outcome on the NEXT
/// user turn without ever polling.
///
/// Deliberately abstract over `AppHandle`: the test binary only references
/// `NoopNotifier`, so the linker's /OPT:REF never pulls the tauri machinery
/// (and its comctl32-v6 imports, which need an app manifest the test exe
/// lacks) into unit tests.
pub(crate) trait BgNotifier: Send + Sync {
    fn announce(&self, pid: u32, command: &str, status: &str, out: &str, err: &str);
}

pub(crate) struct NoopNotifier;

impl BgNotifier for NoopNotifier {
    fn announce(&self, _pid: u32, _command: &str, _status: &str, _out: &str, _err: &str) {}
}

pub(crate) struct AppNotifier(pub(crate) tauri::AppHandle);

impl BgNotifier for AppNotifier {
    fn announce(&self, pid: u32, command: &str, status: &str, out: &str, err: &str) {
        let body = truncate_if_over(
            if err.trim().is_empty() {
                out.to_string()
            } else {
                format!("{out}\n--- stderr ---\n{err}")
            },
            1500,
        );
        let report = ToolReport {
            tool: "run_command".into(),
            subject: command.split_whitespace().collect::<Vec<_>>().join(" "),
            detail: None,
            shell: None,
            cwd: None,
            background: Some(true),
            from_tag: None,
            ok: status.starts_with("exited: 0"),
            error: None,
            status: Some(status.to_string()),
            pid: Some(pid as u64),
            wall_ms: 0,
            tag: None,
            count: None,
            unit: None,
            body: (!body.trim().is_empty()).then_some(ReportBody {
                kind: "output".into(),
                text: body,
            }),
        };
        let _ = self.0.emit(
            "tool_result",
            serde_json::json!({
                "name": format!("bg {pid}"),
                "transcript": report.transcript(),
                "report": &report,
            }),
        );
        log(&format!("BG ANNOUNCE pid {pid}: {status}"));
    }
}

/// Convenience wrapper (tests / headless): completions are recorded in the
/// registry but never announced.
#[cfg_attr(not(test), allow(dead_code))]
async fn spawn_background(
    cmd: tokio::process::Command,
    command: &str,
) -> Result<String, String> {
    spawn_background_notify(std::sync::Arc::new(NoopNotifier), cmd, command).await
}

pub(crate) async fn spawn_background_notify(
    notifier: std::sync::Arc<dyn BgNotifier>,
    mut cmd: tokio::process::Command,
    command: &str,
) -> Result<String, String> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // The job outlives this function; killing is explicit (kill_command).
        .kill_on_drop(false);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().ok_or("child has no pid")?;
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if let Some(out) = child.stdout.take() {
        pump_bg_output(out, stdout.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump_bg_output(err, stderr.clone());
    }
    let status = std::sync::Arc::new(std::sync::Mutex::new("running".to_string()));
    {
        let status = status.clone();
        let notify = notifier.clone();
        let command = command.to_string();
        let (jout, jerr) = (stdout.clone(), stderr.clone());
        tokio::spawn(async move {
            let s = match child.wait().await {
                Ok(s) => format!("exited: {}", s.code().unwrap_or(-1)),
                Err(e) => format!("wait error: {e}"),
            };
            *status.lock().unwrap() = s.clone();
            // Give the pump tasks a beat to flush their final chunk.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let out = String::from_utf8_lossy(&jout.lock().unwrap()).to_string();
            let err = String::from_utf8_lossy(&jerr.lock().unwrap()).to_string();
            notify.announce(pid, &command, &s, &out, &err);
        });
    }
    {
        let mut reg = bg_registry().lock().unwrap();
        evict_bg_jobs(&mut reg);
        reg.insert(
            pid,
            BgJob {
                command: command.to_string(),
                stdout,
                stderr,
                status,
                started: std::time::Instant::now(),
            },
        );
    }
    Ok(format!(
        "started background pid {pid}. Its completion is announced automatically in the conversation \
         — do NOT poll get_command_output for it; use that only if you need partial output early, \
         and kill_command(pid) to stop it."
    ))
}

/// Run a foreground command with an auto-background escape: if it outlives
/// `auto_bg_secs`, it is registered as a background job (same pid, same
/// pipes — nothing is killed or re-run) and the tool returns immediately
/// with the never-poll contract. Completion lands in the ledger by itself.
pub(crate) async fn exec_or_background(
    notifier: std::sync::Arc<dyn BgNotifier>,
    mut cmd: tokio::process::Command,
    command: &str,
    script_path: Option<std::path::PathBuf>,
) -> Result<String, String> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    if let Some(out) = child.stdout.take() {
        pump_bg_output(out, stdout.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump_bg_output(err, stderr.clone());
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(auto_bg_secs()),
        child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => {
            // Streams are EOF (child exited); let the pumps flush.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(p) = &script_path {
                let _ = std::fs::remove_file(p);
            }
            let out = String::from_utf8_lossy(&stdout.lock().unwrap()).to_string();
            let err = String::from_utf8_lossy(&stderr.lock().unwrap()).to_string();
            if status.success() {
                if err.trim().is_empty() {
                    Ok(out)
                } else {
                    Ok(format!("{}\n--- stderr ---\n{}", out, err))
                }
            } else {
                Err(format!("exit {status}: {}\n{}", out, err))
            }
        }
        Ok(Err(e)) => {
            if let Some(p) = &script_path {
                let _ = std::fs::remove_file(p);
            }
            Err(format!("spawn failed: {e}"))
        }
        Err(_) => {
            // Still running: adopt into the background registry WITHOUT
            // touching the process, and announce on completion.
            let pid = child.id().unwrap_or(0);
            let status = std::sync::Arc::new(std::sync::Mutex::new("running".to_string()));
            {
                let status = status.clone();
                let notify = notifier.clone();
                let command = command.to_string();
                let (jout, jerr) = (stdout.clone(), stderr.clone());
                tokio::spawn(async move {
                    let s = match child.wait().await {
                        Ok(s) => format!("exited: {}", s.code().unwrap_or(-1)),
                        Err(e) => format!("wait error: {e}"),
                    };
                    *status.lock().unwrap() = s.clone();
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let out = String::from_utf8_lossy(&jout.lock().unwrap()).to_string();
                    let err = String::from_utf8_lossy(&jerr.lock().unwrap()).to_string();
                    notify.announce(pid, &command, &s, &out, &err);
                });
            }
            {
                let mut reg = bg_registry().lock().unwrap();
                evict_bg_jobs(&mut reg);
                reg.insert(
                    pid,
                    BgJob {
                        command: command.to_string(),
                        stdout,
                        stderr,
                        status,
                        started: std::time::Instant::now(),
                    },
                );
            }
            // The scratch script belongs to the running job now; tmp/ is
            // LRU-trimmed, so it cannot leak.
            Ok(format!(
                "still running after {}s → moved to background (pid {pid}). \
                 Its completion is announced automatically in the conversation — do NOT poll; \
                 use get_command_output(pid) only if you need partial output now, \
                 kill_command(pid) to stop it.",
                auto_bg_secs()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute::execute_tool_inner;

    #[test]
    fn parse_keeps_quoted_phrase_together() {
        assert_eq!(
            parse_command(r#"winapp ui search "Qwen 3.8 Flash Next" -a zen"#),
            vec!["winapp", "ui", "search", "Qwen 3.8 Flash Next", "-a", "zen"]
        );
    }

    #[test]
    fn parse_plain_command() {
        assert_eq!(
            parse_command("winapp ui inspect -a zen"),
            vec!["winapp", "ui", "inspect", "-a", "zen"]
        );
    }

    #[test]
    fn shell_features_route_to_cmd() {
        assert!(needs_shell(r#"tasklist | findstr /i "zen""#));
        assert!(needs_shell("winapp ui click a && winapp ui send-keys ctrl+t"));
        assert!(needs_shell("winapp ui inspect -a zen 2>&1"));
        assert!(needs_shell(r#"if exist "C:\x" (dir /b "C:\x")"#));
        assert!(needs_shell("dir /b C:\\"));
    }

    #[test]
    fn plain_commands_run_direct() {
        assert!(!needs_shell("winapp ui inspect -a zen"));
        assert!(!needs_shell(r#"winapp ui search "Qwen 3.8 Flash Next" -a zen"#));
        assert!(!needs_shell("tasklist /FI \"IMAGENAME eq zen.exe\""));
    }

    #[test]
    fn cmd_shell_path_preserves_quoted_argument() {
        use std::os::windows::process::CommandExt;
        // Same construction as the run_command shell path.
        let cmd = r#"winapp ui search "Qwen 3.8 Flash Next" -a nosuchapp"#;
        let out = std::process::Command::new("cmd.exe")
            .raw_arg("/S")
            .raw_arg("/C")
            .raw_arg(format!("\"{}\"", cmd))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        // If quotes survive, clap parses one selector and winapp fails on the
        // unknown app name; if cmd mangles them, clap rejects the split
        // tokens ("'3.8' was not matched") before the app lookup happens.
        assert!(
            !text.contains("was not matched"),
            "quotes were mangled by cmd: {text}"
        );
    }

    #[test]
    fn identical_repeat_collapses_to_pointer() {
        let mut seen = HashMap::new();
        let out = "x".repeat(100);
        let first = budget_output(&mut seen, "cmd".into(), out.clone());
        assert_eq!(first, out);
        let second = budget_output(&mut seen, "cmd".into(), out.clone());
        assert!(second.contains("identical to your earlier call"));
        // Changed output after an action still comes through fully.
        let changed = "y".repeat(50);
        let third = budget_output(&mut seen, "cmd".into(), changed.clone());
        assert_eq!(third, changed);
    }

    #[test]
    fn oversized_output_is_capped_with_spill_pointer() {
        let mut seen = HashMap::new();
        let huge: String = (0..9000usize).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let out = budget_output(&mut seen, "c".into(), huge.clone());
        assert!(out.contains("chars omitted; FULL output saved:"));
        assert!(out.starts_with('a'));
        assert!(out.ends_with(&huge[huge.len() - 50..]));
        assert!(out.chars().count() < 8600);
        let path = out
            .lines()
            .find(|l| l.contains("FULL output saved:"))
            .unwrap()
            .split("saved: ")
            .nth(1)
            .unwrap()
            .split(" —")
            .next()
            .unwrap()
            .to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), huge);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn changed_repeat_returns_line_diff() {
        let mut seen = HashMap::new();
        budget_output(&mut seen, "inspect".into(), "tab A\ntab B\ntab C".into());
        let out = budget_output(&mut seen, "inspect".into(), "tab A\ntab B2\ntab C".into());
        assert!(out.starts_with("[changes vs your earlier call]"));
        assert!(out.contains("- tab B"));
        assert!(out.contains("+ tab B2"));
        // Unchanged lines are omitted entirely.
        assert!(!out.contains("tab A"));
        assert!(!out.contains("tab C"));
    }

    #[test]
    fn wholesale_change_falls_back_to_full_output() {
        let mut seen = HashMap::new();
        budget_output(&mut seen, "k".into(), "completely\ndifferent\ncontent".into());
        let now = "a\nb".to_string();
        let out = budget_output(&mut seen, "k".into(), now.clone());
        // Diff (3 removals + 2 additions) is bigger than the output itself.
        assert_eq!(out, now);
    }

    #[test]
    fn hung_command_is_killed_at_the_cap() {
        // The soft-lock shape: the process keeps stdout open and never
        // exits (here ping -n 3600; in the wild, `start /b gh auth login`).
        // The call must return with an error, not wait for EOF forever.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let t0 = std::time::Instant::now();
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "3600", "127.0.0.1"]);
            let err = exec_capped(&mut c, std::time::Duration::from_secs(2))
                .await
                .expect_err("hanging command must not return Ok");
            assert!(err.contains("timed out"), "{err}");
            assert!(t0.elapsed() < std::time::Duration::from_secs(30));
            // The child must actually be dead, not orphaned holding pipes.
            // PID-scoped so parallel ping tests can't interfere.
            let pid: u32 = err
                .split("killed pid ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .expect("timeout error must name the killed pid");
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
                "pid {pid} survived the timeout kill"
            );
        });
    }

    fn bg_pid(start_msg: &str) -> u32 {
        start_msg
            .split_whitespace()
            .nth(3)
            .unwrap()
            .trim_end_matches('.')
            .parse()
            .unwrap()
    }

    #[test]
    fn background_command_completes_and_output_is_captured() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "3", "127.0.0.1"]);
            let msg = spawn_background(c, "ping -n 3 127.0.0.1").await.unwrap();
            let pid = bg_pid(&msg);
            let mut poll = String::new();
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                poll = execute_tool_inner("get_command_output", &serde_json::json!({"pid": pid}))
                    .await
                    .unwrap();
                if !poll.contains("status: running") {
                    break;
                }
            }
            assert!(poll.contains("status: exited: 0"), "{poll}");
            let stdout = poll.split("--- stdout ---").nth(1).unwrap();
            assert!(!stdout.trim().is_empty(), "background stdout must be captured");
        });
    }

    #[test]
    fn background_command_can_be_killed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "600", "127.0.0.1"]);
            let msg = spawn_background(c, "ping -n 600 127.0.0.1").await.unwrap();
            let pid = bg_pid(&msg);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let res = execute_tool_inner("kill_command", &serde_json::json!({"pid": pid}))
                .await
                .unwrap();
            assert!(res.contains("killed"), "{res}");
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()),
                "pid {pid} survived kill_command"
            );
        });
    }

    #[test]
    fn foreground_command_captures_stdout_both_paths() {
        // Regression: exec_capped once spawned without piping stdio, so
        // wait_with_output returned empty output for every success.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // cmd-shell branch (built-in).
            let out = execute_tool_inner(
                "run_command",
                &serde_json::json!({"command": "echo CAPTURED_FOREGROUND"}),
            )
            .await
            .unwrap();
            assert!(out.contains("CAPTURED_FOREGROUND"), "shell path lost stdout: {out:?}");
            // Direct branch (real binary with a quoted argument).
            let out = execute_tool_inner(
                "run_command",
                &serde_json::json!({"command": "tasklist /FI \"IMAGENAME eq svchost.exe\" /NH"}),
            )
            .await
            .unwrap();
            assert!(out.contains("svchost"), "direct path lost stdout: {out:?}");
        });
    }

    #[test]
    fn truncate_spills_full_output_behind_a_pointer() {
        let big: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let capped = truncate_if_over(big.clone(), 800);
        assert!(capped.starts_with("line 0\n"));
        assert!(capped.ends_with("line 499\n"));
        let line = capped
            .lines()
            .find(|l| l.contains("FULL output saved:"))
            .expect("pointer line");
        let path = line
            .split("saved: ")
            .nth(1)
            .expect("path")
            .split(" —")
            .next()
            .expect("terminator")
            .to_string();
        let full = std::fs::read_to_string(&path).expect("spill file must exist and be complete");
        assert_eq!(full, big);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn slow_foreground_command_is_auto_backgrounded() {
        std::env::set_var("FIRSTMATE_AUTO_BG_SECS", "1");
        let r = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "ping -n 30 127.0.0.1 > NUL" }),
        )
        .await
        .expect("auto-bg notice");
        std::env::remove_var("FIRSTMATE_AUTO_BG_SECS");
        assert!(r.contains("moved to background (pid"), "got: {r}");
        assert!(r.contains("do NOT poll"), "never-poll contract in notice: {r}");
        let pid: u32 = r
            .split("pid ")
            .nth(1)
            .unwrap()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .expect("pid in notice");
        // The SAME process is adopted by the registry, not restarted.
        let out = execute_tool_inner("get_command_output", &serde_json::json!({ "pid": pid }))
            .await
            .unwrap();
        assert!(out.contains("running"), "{out}");
        execute_tool_inner("kill_command", &serde_json::json!({ "pid": pid }))
            .await
            .unwrap();
    }
}
