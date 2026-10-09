//! Elevated Control: a consented, visible, revocable admin capability.
//!
//! A process can never raise its own integrity level — Windows fixes the
//! token at creation. So elevation is a PROXY: this app (MEDIUM) asks
//! ShellExecute `runas` to start an elevated PowerShell SESSION (HIGH)
//! behind the real UAC prompt the user answers. Commands that need admin
//! are sent to it over a named pipe and run in one persistent runspace —
//! the elevated twin of the medium-IL psrep session. `delevate` (user
//! toggle or agent tool) makes it exit; the helper also dies with the app
//! (watchdog + pipe disconnect), so elevation never outlives the consent
//! that opened it.
//!
//! Trust model: the pipe accepts exactly one client, verified as this
//! same executable; every command is logged. This is the sanctioned door;
//! ad-hoc RunAs scripting in the shell stays forbidden because it is
//! invisible.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// The duplex pipe to the elevated helper. Windows carries the real
/// client; other platforms compile the module but can never hold one.
#[cfg(windows)]
type PipeHandle = named_pipe::PipeClient;
#[cfg(not(windows))]
type PipeHandle = ();

pub(crate) struct HelperSession {
    pipe: Mutex<PipeHandle>,
    pid: u32,
    started: Instant,
    commands: AtomicU32,
}

/// One request, one response line — the whole wire protocol. Reading is
/// byte-by-byte on purpose: a BufReader could swallow the start of the
/// NEXT response out of the socket buffer.
fn round_trip(pipe: &mut (impl Read + Write), req: &Request) -> Result<String, String> {
    pipe.write_all(format!("{req}\n").as_bytes())
        .map_err(|e| format!("pipe write: {e}"))?;
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let n = pipe.read(&mut b).map_err(|e| format!("pipe read: {e}"))?;
        if n == 0 || b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
    }
    if out.is_empty() {
        return Err("helper closed the pipe".into());
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[derive(Default)]
pub(crate) struct ElevationState(pub(crate) Mutex<Option<Arc<HelperSession>>>);

#[derive(Serialize, Deserialize)]
struct Request {
    id: u64,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    timeout_ms: u64,
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_string(self) {
            Ok(s) => f.write_str(&s),
            Err(_) => Err(std::fmt::Error),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Response {
    id: u64,
    ok: bool,
    exit: i32,
    stdout: String,
    stderr: String,
}

fn pipe_path(nonce: &str) -> String {
    format!(r"\\.\pipe\firstmate-elev-{nonce}")
}

fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};
    format!("{:016x}", RandomState::new().build_hasher().finish())
}

/// True when a shell command should route through the elevated session
/// automatically: winapp drives other apps' windows, and when the session
/// is ON it must run at HIGH IL or UIPI silently refuses its input against
/// elevated targets. First-token match only — `echo winapp` is not winapp.
pub(crate) fn is_winapp_call(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .is_some_and(|t| t.eq_ignore_ascii_case("winapp"))
}

/// The elevated helper: a persistent PowerShell runspace fed JSON-line
/// requests over a named pipe. Written to the First Mate tmp dir at
/// elevate() time — CONTENT, visible and auditable, not compiled behavior.
/// Guards, in order: only the First Mate executable may connect (PID ->
/// image path); a watchdog kills the helper the moment the app is gone;
/// every command runs with a hard timeout via async invocation.
#[cfg(windows)]
const HELPER_SCRIPT: &str = r#"
param([string]$Nonce, [int]$AppPid, [string]$AppExe)
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class FmPipe {
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern bool GetNamedPipeClientProcessId(IntPtr p, out uint pid);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern IntPtr OpenProcess(int access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
  public static extern bool QueryFullProcessImageNameW(IntPtr h, int flags, StringBuilder name, ref int size);
  [DllImport("advapi32.dll", SetLastError=true)]
  static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr token);
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
  static extern bool LookupPrivilegeValueW(string system, string name, out long luid);
  [StructLayout(LayoutKind.Sequential)] struct LuidAttr { public uint lo, hi, attr; }
  [StructLayout(LayoutKind.Sequential)] struct Privs { public uint count; public LuidAttr p0; }
  [DllImport("advapi32.dll", SetLastError=true)]
  static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref Privs newState, uint len, IntPtr prev, IntPtr released);
  // The pipe's LOW mandatory label lives in the SACL; assigning one needs
  // SeSecurityPrivilege — present in an elevated token but disabled.
  public static bool EnableSeSecurity() {
    IntPtr token;
    if (!OpenProcessToken((IntPtr)(-1), 0x28, out token)) return false;
    long luid;
    if (!LookupPrivilegeValueW(null, "SeSecurityPrivilege", out luid)) return false;
    var privs = new Privs { count = 1, p0 = new LuidAttr { lo = (uint)luid, hi = (uint)(luid >> 32), attr = 2 } };
    if (!AdjustTokenPrivileges(token, false, ref privs, (uint)System.Runtime.InteropServices.Marshal.SizeOf(typeof(Privs)), IntPtr.Zero, IntPtr.Zero)) return false;
    return Marshal.GetLastWin32Error() == 0;
  }
}
'@
$app = $AppExe.ToLower()
# Watchdog FIRST: elevation must end with the app that consented to it,
# even if the app dies before anyone connects to the pipe.
$watch = [powershell]::Create()
$watch.AddScript({ param($p) while ($true) { Start-Sleep -Seconds 2; if (-not (Get-Process -Id $p -ErrorAction SilentlyContinue)) { [Environment]::Exit(0) } } }).AddArgument($AppPid) | Out-Null
$watch.BeginInvoke() | Out-Null
# Explicit security: the default descriptor inherits this (elevated)
# process's HIGH integrity label, and the NoWriteUp mandatory policy then
# rejects the medium-IL app's connect outright. Label LOW + interactive
# read/write lets the app in; the real gate is the client PID + exe-path
# check below, and the single-instance pipe means whoever connects first
# is the only candidate — a squatter gets verified, fails, and kills us.
if (-not [FmPipe]::EnableSeSecurity()) { Write-Error 'cannot enable SeSecurityPrivilege'; exit 4 }
$sec = New-Object System.IO.Pipes.PipeSecurity
$sec.SetSecurityDescriptorSddlForm('D:(A;;GWGR;;;IU)S:(ML;;NW;;;LW)')
$server = New-Object System.IO.Pipes.NamedPipeServerStream("firstmate-elev-$Nonce", [System.IO.Pipes.PipeDirection]::InOut, 1, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::None, 0, 0, $sec)
# Never connect = nobody wants us (failed/duplicate elevate): leave.
$task = $server.WaitForConnectionAsync()
if (-not $task.Wait(120000)) { $server.Dispose(); exit 3 }
# Only the First Mate executable itself may drive this pipe.
$cpid = 0
[void][FmPipe]::GetNamedPipeClientProcessId($server.SafePipeHandle.DangerousGetHandle(), [ref]$cpid)
$h = [FmPipe]::OpenProcess(0x1000, $false, $cpid)
$sb = New-Object System.Text.StringBuilder 1024
$len = 1024
$verified = $false
if ($h -ne [IntPtr]::Zero) { $verified = [FmPipe]::QueryFullProcessImageNameW($h, 0, $sb, [ref]$len) }
if (-not $verified -or $sb.ToString().ToLower() -ne $app) { $server.Dispose(); exit 2 }
$reader = New-Object System.IO.StreamReader($server)
$writer = New-Object System.IO.StreamWriter($server)
$writer.AutoFlush = $true
# One persistent instance = an elevated session: $vars and functions
# survive across requests, mirroring the medium-IL PowerShell session.
$ps = [powershell]::Create()
while ($true) {
  $line = $reader.ReadLine()
  if ($null -eq $line) { break }
  if ($line.Trim() -eq '') { continue }
  try { $req = $line | ConvertFrom-Json } catch { continue }
  if ($req.op -eq 'exit') { break }
  if ($req.op -eq 'ping') {
    (@{ id = $req.id; ok = $true; exit = 0; stdout = 'pong'; stderr = '' } | ConvertTo-Json -Compress) | ForEach-Object { $writer.WriteLine($_) }
    continue
  }
  try {
    $ps.Commands.Clear(); $ps.Streams.Error.Clear()
    [void]$ps.AddScript([string]$req.command)
    $ar = $ps.BeginInvoke()
    $ms = [int]$req.timeout_ms
    if ($ms -lt 1000) { $ms = 60000 }
    if (-not $ar.AsyncWaitHandle.WaitOne($ms)) {
      try { $ps.Stop() } catch {}
      try { $ps.EndInvoke($ar) | Out-Null } catch {}
      # An aborted pipeline poisons the instance: later commands would
      # "succeed" with no output. Never reuse it — start a clean session.
      $ps.Dispose(); $ps = [powershell]::Create()
      (@{ id = $req.id; ok = $false; exit = -1; stdout = ''; stderr = "timed out after $([math]::Round($ms/1000,1))s and was stopped; elevated session state was reset" } | ConvertTo-Json -Compress) | ForEach-Object { $writer.WriteLine($_) }
      continue
    }
    $objs = $ps.EndInvoke($ar)
    $stdout = ($objs | Out-String)
    $stderr = ($ps.Streams.Error | Out-String)
    $ok = ($ps.Streams.Error.Count -eq 0)
    (@{ id = $req.id; ok = $ok; exit = 0; stdout = $stdout; stderr = $stderr } | ConvertTo-Json -Compress) | ForEach-Object { $writer.WriteLine($_) }
  } catch {
    # A terminating error (throw, poisoned instance): report it honestly
    # and reset — a silently-wrong elevated session is the worst outcome.
    try { $ps.Dispose() } catch {}
    $ps = [powershell]::Create()
    $msg = $_.Exception.Message
    if ($_.Exception.InnerException) { $msg += " | " + $_.Exception.InnerException.Message }
    (@{ id = $req.id; ok = $false; exit = 1; stdout = ''; stderr = $msg } | ConvertTo-Json -Compress) | ForEach-Object { $writer.WriteLine($_) }
  }
}
$server.Dispose()
exit 0
"#;

pub(crate) fn elevation_active(app: &AppHandle) -> bool {
    app.state::<ElevationState>()
        .0
        .lock()
        .unwrap()
        .is_some()
}

fn active_session(app: &AppHandle) -> Option<Arc<HelperSession>> {
    app.state::<ElevationState>()
        .0
        .lock()
        .unwrap()
        .clone()
}

fn set_session(app: &AppHandle, session: Option<Arc<HelperSession>>) {
    *app.state::<ElevationState>().0.lock().unwrap() = session.clone();
    let payload = match &session {
        Some(s) => serde_json::json!({
            "active": true,
            "pid": s.pid,
            "commands": s.commands.load(Ordering::Relaxed),
        }),
        None => serde_json::json!({ "active": false }),
    };
    let _ = app.emit("elevation_changed", &payload);
}

/// The helper died (crash, kill, logoff): clear the session so the toggle
/// and the tools agree — but only if it is still the SAME session, so a
/// concurrent fresh `elevate` is never wiped.
fn mark_dead(app: &AppHandle, dead: &Arc<HelperSession>) {
    let state = app.state::<ElevationState>();
    let mut guard = state.0.lock().unwrap();
    if guard.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, dead)) {
        *guard = None;
        drop(guard);
        crate::log::log("elevated session lost (helper exited)");
        let _ = app.emit("elevation_changed", &serde_json::json!({ "active": false }));
    }
}

/// Shared core of the `elevate` tool and the window toggle: write the
/// helper script, fire the real UAC prompt, connect and verify the pipe.
pub(crate) fn elevate_core(app: &AppHandle) -> Result<String, String> {
    if let Some(s) = active_session(app) {
        return Ok(format!(
            "already elevated (helper pid {}, {} elevated commands so far)",
            s.pid,
            s.commands.load(Ordering::Relaxed)
        ));
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        return Err("elevation is a Windows capability".into());
    }
    #[cfg(windows)]
    {
        let nonce = nonce();
        let app_pid = std::process::id();
        let script = write_helper_script()?;
        // Err here means: the prompt could not be shown, or the user said No.
        let app_exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate own executable: {e}"))?;
        let helper_pid =
            spawn_powershell_elevated(&script, &nonce, app_pid, &app_exe.to_string_lossy())?;
        let path = pipe_path(&nonce);
        let mut last_err = String::from("helper never answered");
        for _ in 0..50 {
            // A concurrent elevate won: our helper finds no client and
            // self-exits on its connect deadline — do not overwrite.
            if active_session(app).is_some() {
                return Ok("elevated session is already ON".into());
            }
            match open_session(&path, helper_pid) {
                Ok(session) => {
                    let arc = Arc::new(session);
                    set_session(app, Some(arc));
                    crate::log::log(&format!("elevated session ON (powershell pid {helper_pid})"));
                    return Ok(format!(
                        "elevated session is ON (helper pid {helper_pid}). The user can end it \
                         anytime with the Elevated Control toggle; call delevate when the task is done."
                    ));
                }
                Err(e) => {
                    last_err = e;
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
        Err(format!("elevation did not come up: {last_err}"))
    }
}

#[cfg(windows)]
fn write_helper_script() -> Result<std::path::PathBuf, String> {
    let dir = crate::log::fm_tmp_dir();
    let path = dir.join("elevate-helper.ps1");
    let mut bytes = vec![0xEF, 0xBB, 0xBF]; // BOM: PowerShell 5.1 decodes UTF-8
    bytes.extend_from_slice(HELPER_SCRIPT.as_bytes());
    std::fs::write(&path, &bytes).map_err(|e| format!("cannot write helper script: {e}"))?;
    Ok(path)
}

/// Connect, then prove the other side is the helper with a ping before
/// trusting the session.
#[cfg(windows)]
fn open_session(path: &str, pid: u32) -> Result<HelperSession, String> {
    let mut client =
        named_pipe::PipeClient::connect(path).map_err(|e| format!("pipe connect: {e}"))?;
    let req = Request {
        id: 0,
        command: None,
        op: Some("ping".into()),
        timeout_ms: 0,
    };
    let line = round_trip(&mut client, &req)?;
    let resp: Response = serde_json::from_str(&line).map_err(|e| format!("ping reply: {e}"))?;
    if !resp.ok {
        return Err("helper rejected the session".into());
    }
    Ok(HelperSession {
        pipe: Mutex::new(client),
        pid,
        started: Instant::now(),
        commands: AtomicU32::new(0),
    })
}

pub(crate) fn delevate_core(app: &AppHandle, by_agent: bool) -> String {
    match active_session(app) {
        Some(s) => {
            let req = Request {
                id: u64::MAX,
                command: None,
                op: Some("exit".into()),
                timeout_ms: 0,
            };
            let _ = writeln!(s.pipe.lock().unwrap(), "{req}");
            set_session(app, None);
            crate::log::log(&format!(
                "elevated session OFF ({})",
                if by_agent { "agent relinquished" } else { "user toggle" }
            ));
            "elevated session ended — helper exited, back to non-elevated only".to_string()
        }
        None => "no elevated session is active".to_string(),
    }
}

/// Send one command to the helper and wait for its result. Blocking; call
/// via spawn_blocking. Any pipe failure means the helper is gone: the
/// session is cleared so the toggle and the tools agree.
fn run_elevated_blocking(
    app: &AppHandle,
    command: &str,
    timeout: Duration,
) -> Result<String, String> {
    let session = active_session(app).ok_or_else(|| {
        "elevated session is not active — call elevate first (the user approves a UAC prompt), \
         or ask the user to switch on Elevated Control"
            .to_string()
    })?;
    let id = session.commands.fetch_add(1, Ordering::Relaxed) as u64;
    let req = Request {
        id,
        command: Some(command.to_string()),
        op: None,
        timeout_ms: timeout.as_millis() as u64,
    };
    crate::log::log(&format!("ELEVATED cmd #{id}: {command}"));
    let line = {
        let mut pipe = session.pipe.lock().map_err(|_| "pipe poisoned".to_string())?;
        round_trip(&mut *pipe, &req)
    };
    let line = match line {
        Ok(l) => l,
        Err(e) => {
            mark_dead(app, &session);
            return Err(format!("elevated session ended: {e}"));
        }
    };
    let resp: Response =
        serde_json::from_str(&line).map_err(|e| format!("malformed helper reply: {e}"))?;
    let mut out = resp.stdout;
    if !resp.stderr.is_empty() {
        out.push_str(&format!("\n[stderr]\n{}", resp.stderr));
    }
    if !resp.ok {
        return Err(format!(
            "elevated exit code {}: {}",
            resp.exit,
            if out.trim().is_empty() { "no output" } else { out.trim() }
        ));
    }
    Ok(if out.trim().is_empty() {
        "(no output; exit 0)".to_string()
    } else {
        out
    })
}

/// ShellExecute `runas` on the system PowerShell running the helper script.
/// Returns the helper PID (for the banner) or Err when the user declined
/// the UAC prompt (or it could not be shown).
#[cfg(windows)]
fn spawn_powershell_elevated(
    script: &std::path::Path,
    nonce: &str,
    app_pid: u32,
    app_exe: &str,
) -> Result<u32, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::GetProcessId;
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe");
    let params = format!(
        "-NoProfile -ExecutionPolicy Bypass -File \"{}\" {nonce} {app_pid} \"{app_exe}\"",
        script.display()
    );
    let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (vexe, vverb, vparams) = (w(&exe), w("runas"), w(&params));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: HWND::default(),
        lpVerb: PCWSTR(vverb.as_ptr()),
        lpFile: PCWSTR(vexe.as_ptr()),
        lpParameters: PCWSTR(vparams.as_ptr()),
        lpDirectory: PCWSTR::null(),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|_| "the elevation prompt was declined (or could not be shown)".to_string())?;
    let pid = unsafe { GetProcessId(info.hProcess) };
    unsafe {
        let _ = CloseHandle(info.hProcess);
    }
    Ok(pid)
}

// The #[tauri::command] wrappers for elevate/delevate/elevation_status
// live in lib.rs, NOT here: command glue references the tauri IPC/webview
// machinery, and because a module links as one object, keeping the glue
// here would drag the whole GUI stack (and its comctl32 manifest
// requirement) into every binary that touches a single pure function —
// including the unit-test exe. This module stays light: pure logic, the
// pipe protocol, and plain AppHandle helpers.

#[derive(Serialize)]
pub(crate) struct ElevStatus {
    pub active: bool,
    pub pid: u32,
    pub commands: u32,
    pub age_secs: u64,
}

pub(crate) fn status_snapshot(app: &AppHandle) -> ElevStatus {
    match app.state::<ElevationState>().0.lock().unwrap().as_ref() {
        Some(s) => ElevStatus {
            active: true,
            pid: s.pid,
            commands: s.commands.load(Ordering::Relaxed),
            age_secs: s.started.elapsed().as_secs(),
        },
        None => ElevStatus {
            active: false,
            pid: 0,
            commands: 0,
            age_secs: 0,
        },
    }
}

pub(crate) fn agent_elevate(app: &AppHandle) -> Result<String, String> {
    elevate_core(app)
}

pub(crate) fn agent_delevate(app: &AppHandle) -> Result<String, String> {
    Ok(delevate_core(app, true))
}

pub(crate) async fn agent_run_elevated(
    app: AppHandle,
    command: String,
    timeout: Duration,
) -> Result<String, String> {
    if !elevation_active(&app) {
        return Err(
            "elevated session is not active — call elevate first (the user approves a real UAC \
             prompt), or ask the user to switch on Elevated Control in the chat window"
                .to_string(),
        );
    }
    tokio::task::spawn_blocking(move || run_elevated_blocking(&app, &command, timeout))
        .await
        .map_err(|e| format!("helper task crashed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winapp_detection_is_first_token_only_and_case_insensitive() {
        assert!(is_winapp_call("winapp ui list-windows"));
        assert!(is_winapp_call("  WINAPP ui screenshot -a Setup"));
        assert!(!is_winapp_call("echo winapp"));
        assert!(!is_winapp_call("powershell winapp"));
        assert!(!is_winapp_call(""));
    }

    #[test]
    fn pipe_path_carries_the_nonce() {
        assert_eq!(pipe_path("abc123"), r"\\.\pipe\firstmate-elev-abc123");
    }

    #[test]
    fn protocol_round_trips_as_json_lines() {
        let req = Request {
            id: 7,
            command: Some("Get-Date".into()),
            op: None,
            timeout_ms: 5000,
        };
        let line = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.command.as_deref(), Some("Get-Date"));
        let resp = Response {
            id: 7,
            ok: false,
            exit: 1,
            stdout: "o".into(),
            stderr: "e".into(),
        };
        let back: Response =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back.exit, 1);
        assert!(!back.ok);
    }

    #[test]
    fn nonces_are_unique() {
        assert_ne!(nonce(), nonce());
    }
}
