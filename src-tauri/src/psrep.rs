use crate::exec::{MAX_BG_OUTPUT, cmd_timeout};

// ---------------------------------------------------------------------------
// Persistent PowerShell session
// ---------------------------------------------------------------------------
//
// One long-lived `powershell.exe` owned by a dedicated worker thread. Each
// command is script TEXT written to the child's stdin (data, never argv)
// followed by a unique run-marker line; a REPL inside the child executes the
// accumulated text as one scriptblock and answers with a sentinel line
// carrying the exit status. `$vars`, functions, and cwd persist across
// tool calls, and there is zero quoting nesting: the command never passes
// through another interpreter's argument parser.
//
// Failure semantics:
// - Broken: the command demonstrably never ran (spawn/write failure) ->
//   caller may safely fall back to one-shot `powershell -File`.
// - TimedOut / died mid-command: the command may have partially run ->
//   the session is killed, partial output is returned, and NOTHING is
//   re-run (no duplicated side effects).

/// The REPL script that owns the session child's stdin loop. It reads
/// lines until it sees a `__FM_RUN_*` marker, dot-sources everything
/// collected (so assignments land in session scope), then answers
/// `__FM_END_<id>:<ok as int>:<$LASTEXITCODE>`. Success is tracked with an
/// explicit flag because `$?` resets to True once a try/catch completes —
/// a caught `throw` must still report failure — and it also folds in the
/// final `$?` so non-terminating cmdlet errors (missing paths etc.) fail.
/// Protocol variables carry the `$__fm_` prefix: dot-sourcing shares
/// session scope, so a user script assigning common names used to be able
/// to clobber the protocol state (live incident: `$id =
/// [WindowsIdentity]::GetCurrent()` mangled the sentinel id, the harness
/// never matched its line, and the session soft-locked until the
/// timeout cap).
const PS_REPL: &str = r#"[Console]::OutputEncoding=[Text.Encoding]::UTF8;[Console]::InputEncoding=[Text.Encoding]::UTF8;while($true){$__fm_c=[Console]::In.ReadLine();if($null -eq $__fm_c){break};$__fm_b=New-Object System.Collections.ArrayList;while($__fm_c -notlike '__FM_RUN_*'){[void]$__fm_b.Add($__fm_c);$__fm_c=[Console]::In.ReadLine();if($null -eq $__fm_c){break}};if($null -eq $__fm_c){break};$__fm_id=$__fm_c.Substring(9);$__fm_ok=$true;try{. ([scriptblock]::Create(($__fm_b -join [Environment]::NewLine)))}catch{Write-Error $_;$__fm_ok=$false};$__fm_ok=$__fm_ok -and $?;Write-Host "__FM_END_$($__fm_id):$([int]$__fm_ok):$LASTEXITCODE"}"#;

struct PsCommand {
    text: String,
    cwd: Option<String>,
    timeout: std::time::Duration,
    reply: std::sync::mpsc::Sender<PsReply>,
}

enum PsMsg {
    Cmd(PsCommand),
    /// Test-only: kill the child and stop the worker.
    Shutdown(std::sync::mpsc::Sender<()>),
}

enum PsReply {
    /// Command completed: stdout, exit code, stderr (stderr is buffered
    /// separately, so cross-stream interleaving is not preserved).
    Done { stdout: String, code: i32, stderr: String },
    /// Command never ran; one-shot fallback is safe.
    Broken { note: String },
    /// Command may have partially run; partial output only, never re-run.
    TimedOut { partial: String, note: String },
}

#[derive(Debug)]
pub(crate) enum PsRun {
    Done { stdout: String, code: i32, stderr: String },
    Broken { note: String },
    TimedOut { partial: String, note: String },
}

struct PsState {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    errbuf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    nonce: u64,
}

static PS_QUEUE: std::sync::LazyLock<
    std::sync::Mutex<Option<std::sync::mpsc::Sender<PsMsg>>>,
> = std::sync::LazyLock::new(Default::default);

fn ps_worker_tx() -> std::sync::mpsc::Sender<PsMsg> {
    let mut guard = PS_QUEUE.lock().unwrap();
    if let Some(tx) = guard.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = std::sync::mpsc::channel::<PsMsg>();
    std::thread::spawn(move || ps_worker(rx));
    *guard = Some(tx.clone());
    tx
}

fn ps_spawn() -> Result<PsState, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(1)
        % 1_000_000_000;
    let mut child = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", PS_REPL])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("powershell spawn failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("session stdin missing")?;
    let out = child.stdout.take().ok_or("session stdout missing")?;
    let err = child.stderr.take().ok_or("session stderr missing")?;
    let (ltx, lrx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(out);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                        line.pop();
                    }
                    if ltx
                        .send(String::from_utf8_lossy(&line).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    let errbuf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let eb = errbuf.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut err = err;
            let mut chunk = [0u8; 8192];
            loop {
                match err.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut b = eb.lock().unwrap();
                        if b.len() < MAX_BG_OUTPUT {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        });
    }
    Ok(PsState {
        child,
        stdin,
        lines: lrx,
        errbuf,
        nonce,
    })
}

fn ps_worker(rx: std::sync::mpsc::Receiver<PsMsg>) {
    let mut state: Option<PsState> = None;
    let mut seq: u64 = 0;
    for msg in rx {
        match msg {
            PsMsg::Shutdown(ack) => {
                if let Some(mut st) = state.take() {
                    let _ = st.child.kill();
                }
                let _ = ack.send(());
                return;
            }
            PsMsg::Cmd(cmd) => {
                seq += 1;
                ps_handle(cmd, seq, &mut state);
            }
        }
    }
}

fn ps_handle(cmd: PsCommand, seq: u64, state: &mut Option<PsState>) {
    use std::io::Write;
    if state.is_none() {
        match ps_spawn() {
            Ok(st) => *state = Some(st),
            Err(note) => {
                let _ = cmd.reply.send(PsReply::Broken { note });
                return;
            }
        }
    }
    let st = state.as_mut().unwrap();
    let id = format!("{}_{seq}", st.nonce);
    let mut script = String::new();
    if let Some(dir) = &cmd.cwd {
        // Session cwd: persists into later commands (a feature, matching
        // how a human uses one shell for a task).
        script.push_str(&format!("Set-Location -LiteralPath '{}'\n", dir.replace('\'', "''")));
    }
    script.push_str(&cmd.text);
    if !script.ends_with('\n') {
        script.push('\n');
    }
    // No trailing underscores: the REPL derives the id via Substring(9),
    // and the echoed `__FM_END_<id>:` must match Rust's sentinel exactly.
    script.push_str(&format!("__FM_RUN_{id}\n"));
    if st.stdin.write_all(script.as_bytes()).and_then(|_| st.stdin.flush()).is_err() {
        let _ = st.child.kill();
        *state = None;
        let _ = cmd.reply.send(PsReply::Broken {
            note: "session stdin closed; child killed".into(),
        });
        return;
    }
    // Match on the sentinel PREFIX, not the exact id: the id round-trips
    // through user-script scope and could still be clobbered by an
    // assignment. The worker serializes commands and every failure path
    // kills+respawns the child with a fresh line channel, so any
    // `__FM_END_` line on this channel belongs to the in-flight command.
    let deadline = std::time::Instant::now() + cmd.timeout;
    let mut out = String::new();
    let mut disconnected = false;
    let mut stopped = false;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        // Short slices so a user stop can interrupt a wedged command.
        let slice = remaining.min(std::time::Duration::from_millis(250));
        match st.lines.recv_timeout(slice) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("__FM_END_") {
                    let mut it = rest.rsplitn(3, ':');
                    let lastexit = it.next().and_then(|s| s.trim().parse::<i32>().ok());
                    let ok = it
                        .next()
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(1);
                    let code = if ok == 0 { 1 } else { lastexit.unwrap_or(0) };
                    let stderr =
                        std::mem::take(&mut *st.errbuf.lock().unwrap());
                    let _ = cmd.reply.send(PsReply::Done {
                        stdout: out,
                        code,
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                    return;
                }
                out.push_str(&line);
                out.push('\n');
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                disconnected = true; // child died mid-command
                break;
            }
            Err(_) => {
                if std::time::Instant::now() >= deadline {
                    break; // deadline
                }
                // A user stop must interrupt a wedged command: this wait
                // loop is the only thing blocking the agent loop, and
                // CHAT_STOP is otherwise checked only between calls.
                if crate::llm::CHAT_STOP.load(std::sync::atomic::Ordering::SeqCst) {
                    stopped = true;
                    break;
                }
            }
        }
    }
    // No sentinel: the command timed out, or the child exited mid-command.
    // If the child EXITED, its process exit code is the command's own
    // result: a command calling `exit N` (or a bare `exit`) tears down the
    // REPL host and Windows reports N as the process code. A real crash
    // honestly surfaces as a large nonzero code with partial output.
    let mut status = st.child.try_wait().ok().flatten();
    if status.is_none() && disconnected {
        // stdout EOF can land microseconds before the process-exit record;
        // give the OS a moment so `exit N` reports N instead of a false
        // timeout (the common case; a hung-but-closed pipe is not a thing).
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            status = st.child.try_wait().ok().flatten();
            if status.is_some() {
                break;
            }
        }
    }
    let _ = st.child.kill();
    let stderr = std::mem::take(&mut *st.errbuf.lock().unwrap());
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    *state = None; // next command respawns the session transparently
    match status.and_then(|s| s.code()) {
        Some(code) => {
            let _ = cmd.reply.send(PsReply::Done {
                stdout: out,
                code,
                stderr,
            });
        }
        None => {
            let _ = cmd.reply.send(PsReply::TimedOut {
                partial: out,
                note: if stopped {
                    "stopped by user; the command may have partially run".into()
                } else if status.is_some() {
                    "session child exited mid-command without an exit code".into()
                } else {
                    format!(
                        "session command timed out after {}s",
                        cmd.timeout.as_secs()
                    )
                },
            });
        }
    }
}

pub(crate) async fn ps_session_exec(
    text: &str,
    cwd: Option<String>,
    timeout: std::time::Duration,
) -> PsRun {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if ps_worker_tx()
        .send(PsMsg::Cmd(PsCommand {
            text: text.to_string(),
            cwd,
            timeout,
            reply: reply_tx,
        }))
        .is_err()
    {
        return PsRun::Broken { note: "session worker thread gone".into() };
    }
    // The worker enforces each command's own deadline, but a command may
    // additionally queue behind one wedged predecessor (killed at its own
    // cap). The margin covers the worst case: full foreground cap + slack.
    let margin = timeout + cmd_timeout() + std::time::Duration::from_secs(30);
    let joined = tokio::task::spawn_blocking(move || reply_rx.recv_timeout(margin)).await;
    match joined {
        Ok(Ok(PsReply::Done { stdout, code, stderr })) => {
            PsRun::Done { stdout, code, stderr }
        }
        Ok(Ok(PsReply::Broken { note })) => PsRun::Broken { note },
        Ok(Ok(PsReply::TimedOut { partial, note })) => PsRun::TimedOut { partial, note },
        // Reply never arrived: the command may or may not have run, so
        // this is TimedOut (never re-run), NOT Broken (safe fallback).
        _ => PsRun::TimedOut {
            partial: String::new(),
            note: "session reply lost (worker wedged or died)".into(),
        },
    }
}

#[allow(dead_code)] // called from tests; production keeps the session for app lifetime
/// Test-only: tear the session down so a test cannot leak a powershell.exe.
pub(crate) async fn ps_session_shutdown() {
    let taken = PS_QUEUE.lock().unwrap().take();
    if let Some(tx) = taken {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        let _ = tx.send(PsMsg::Shutdown(ack_tx));
        let _ = tokio::task::spawn_blocking(move || {
            ack_rx.recv_timeout(std::time::Duration::from_secs(5))
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute::execute_tool_inner;

    /// One test fn on a current-thread runtime: the session is global, so
    /// its lifecycle checks must not interleave with each other.
    #[tokio::test]
    async fn ps_session_persists_state_and_survives_failures() {
        use std::time::Duration;
        let t = Duration::from_secs(60);
        // 1. State persists across separate tool-level calls.
        let a = ps_session_exec("$fmTestVar = 6 * 7", None, t).await;
        assert!(matches!(a, PsRun::Done { code: 0, .. }), "spawn+assign: {a:?}");
        let b = ps_session_exec("Write-Output \"val=$fmTestVar\"", None, t).await;
        match b {
            PsRun::Done { stdout, code: 0, .. } => {
                assert!(stdout.contains("val=42"), "state must persist: {stdout}");
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // 2. Multiline script with a real multi-line construct (here-string).
        let c = ps_session_exec(
            "$here = @'\nline one\nline two\n'@\nWrite-Output $here",
            None,
            t,
        )
        .await;
        match c {
            PsRun::Done { stdout, code: 0, .. } => {
                assert!(stdout.contains("line one") && stdout.contains("line two"));
            }
            other => panic!("here-string: {other:?}"),
        }
        // 3. A failing command reports failure but does NOT kill the session.
        let d = ps_session_exec("throw 'boom'", None, t).await;
        match d {
            PsRun::Done { code, stderr, .. } => {
                assert_eq!(code, 1, "throw must surface as failure");
                assert!(stderr.contains("boom"), "error text on stderr: {stderr}");
            }
            other => panic!("throw: {other:?}"),
        }
        let e = ps_session_exec("Write-Output alive", None, t).await;
        assert!(matches!(e, PsRun::Done { code: 0, .. }), "session survives throw: {e:?}");
        // 4. cwd param applies and persists in the session.
        let f = ps_session_exec("", Some("C:\\Windows".into()), t).await;
        assert!(matches!(f, PsRun::Done { code: 0, .. }), "set-location: {f:?}");
        let g = ps_session_exec("(Get-Location).Path", None, t).await;
        match g {
            PsRun::Done { stdout, .. } => {
                assert_eq!(stdout.trim().to_lowercase(), "c:\\windows");
            }
            other => panic!("cwd persist: {other:?}"),
        }
        // 5. Timeout kills the session (no re-run), and the next call
        //    transparently respawns.
        let h = ps_session_exec("Start-Sleep -Seconds 30", None, Duration::from_secs(2)).await;
        match h {
            PsRun::TimedOut { note, .. } => assert!(note.contains("timed out"), "{note}"),
            other => panic!("timeout expected, got {other:?}"),
        }
        let i = ps_session_exec("Write-Output respawned", None, t).await;
        assert!(matches!(i, PsRun::Done { code: 0, .. }), "respawn: {i:?}");
        // 6. Protocol vars survive clobbering (live incident: `$id =
        //    [WindowsIdentity]::GetCurrent()` mangled the sentinel id and
        //    soft-locked the session until the timeout cap).
        let j = ps_session_exec(
            "$id = [Security.Principal.WindowsIdentity]::GetCurrent()\n$c = 'x'\n$b = 'y'\n$ok = $false\nWrite-Output survived",
            None,
            t,
        )
        .await;
        match j {
            PsRun::Done { stdout, code: 0, .. } => assert!(stdout.contains("survived")),
            other => panic!("protocol-var clobber must complete, got {other:?}"),
        }
        // 7. A user stop interrupts a wedged command immediately instead
        //    of waiting out the timeout cap.
        crate::llm::stop_chat();
        let k = ps_session_exec("Start-Sleep -Seconds 30", None, t).await;
        crate::llm::CHAT_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
        match k {
            PsRun::TimedOut { note, .. } => assert!(note.contains("stopped by user"), "{note}"),
            other => panic!("stop must interrupt, got {other:?}"),
        }
        // 8. Through the tool surface: session failure surfaces as tool error.
        let bad = execute_tool_inner(
            "run_command",
            &serde_json::json!({ "command": "exit 3", "shell": "powershell" }),
        )
        .await;
        assert!(bad.unwrap_err().contains("exit code 3"));
        ps_session_shutdown().await;
    }
}
