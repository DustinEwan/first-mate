---
name: elevation
description: Administrator rights the user grants and can revoke — a persistent elevated PowerShell session for tasks that genuinely need admin (services, registry writes, scheduled tasks, firewall, driving elevated app windows). Load when a task needs admin or failed with "access denied" / "requires elevation".
tools: elevate, delevate, run_elevated
---

# Elevation — the user's door, not yours

You cannot become admin; Windows fixes your token at process creation and
that is not a bug to route around. What exists instead is a **sanctioned
door**: `elevate` shows the user a real UAC prompt. If they approve, an
**elevated PowerShell session** comes up — the admin twin of your
persistent shell — and `run_elevated` runs commands in it. The user sees
"Elevated Control" lit in the chat window the whole time it is on, can
kill it with one click, and it dies with the app no matter what.

## Rules of the door
- **Ask for it late, give it back early.** Load this skill and call
  `elevate` only when the task genuinely needs admin — not "might". The
  moment the admin part is done, `delevate`. Never hold elevation between
  tasks.
- **A declined prompt is an answer.** `elevate` failing means the user
  said No (or the prompt could not appear). Do NOT retry, do not rephrase
  and ask again — tell the user what the task needs admin for and let the
  conversation move on.
- **Never spawn your own elevated processes.** No `Start-Process -Verb
  RunAs`, no scheduled-task tricks, no shell-around for admin. This skill
  is the only elevation path; ad-hoc RunAs is invisible, and invisible
  privilege is exactly what this app forbids.
- **Prefer the surface anyway.** If the job has a first-party UI (Settings
  pages, Device Manager, installer wizards), that UI still wins — it will
  prompt the user itself, in the open. Elevation is for admin work with no
  surface worth driving (batch registry, service config, scheduled tasks).

## Using the session
`run_elevated` takes PowerShell statements — same persistent-session
semantics as `shell="powershell"`: `$vars`, functions and cwd carry across
calls. There is no `shell` parameter; it is always PowerShell.

```
# is a service running? (read-only queries usually work WITHOUT elevation — try run_command first)
Get-Service SomeService | Format-List Name,Status,StartType

# the admin part
Set-Service SomeService -StartupType Disabled; Stop-Service SomeService
```

While the session is ON, commands whose first token is `winapp` route
through it automatically — your `winapp` UI control suddenly works against
elevated windows (installer wizards, Device Manager) that silently refused
input before. No special syntax.

## What the user sees
- UAC prompt on `elevate` (they answer it; you wait).
- Elevated Control indicator + command count in the chat window while ON.
- Every elevated command in the log.
- Toggle OFF or `delevate` → helper exits, you are back to normal shell.
