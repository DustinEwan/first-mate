---
name: elevation
description: "Administrator rights the user grants and can revoke — a persistent elevated PowerShell session for tasks that genuinely need admin (services, registry writes, scheduled tasks, firewall, driving elevated app windows). Load when a task needs admin or failed with \"access denied\" / \"requires elevation\"."
---

# Elevation — Granted by the User

You cannot become admin on your own. Windows fixes your access token when the
process starts, and a process cannot change its own token. What exists instead
is a path the user controls: `elevate` shows the user a real UAC prompt. If
the user approves, an **elevated PowerShell session** starts. It is your
persistent shell with admin rights, and `run_elevated` runs commands in it.
The user sees "Elevated Control" lit in the chat window for as long as it is
ON. The user can end it with one click, and the session ends when the app
exits.

## Rules
- **Ask late, release early.** Load this skill and call `elevate` only when
  the task genuinely needs admin, not when it might. The moment the admin part
  is done, call `delevate`. Never hold elevation between tasks.
- **A declined prompt is an answer.** `elevate` failing means the user said
  No, or the prompt could not appear. Do NOT retry, and do not rephrase and
  ask again. Tell the user what the task needs admin for, and let the
  conversation move on.
- **Never spawn your own elevated processes.** No `Start-Process -Verb
  RunAs`, and no scheduled-task tricks. This skill is the only elevation
  path. The user cannot see an ad-hoc elevated process, and this app forbids
  work the user cannot see.
- **Prefer the surface anyway.** If the job has a first-party UI (Settings
  pages, Device Manager, installer wizards), use that UI. It prompts the user
  itself, in the open. Elevation is for admin work with no UI worth driving:
  batch registry edits, service config, and scheduled tasks.

## Using the session
`run_elevated` takes PowerShell statements. The session has the same
persistent semantics as `shell="powershell"`: `$vars`, functions, and cwd
carry across calls. There is no `shell` parameter; the language is always
PowerShell.

```
# is a service running? (read-only queries usually work WITHOUT elevation — try run_command first)
Get-Service SomeService | Format-List Name,Status,StartType

# the admin part
Set-Service SomeService -StartupType Disabled; Stop-Service SomeService
```

While the session is ON, commands whose first token is `winapp` route through
it automatically. Your `winapp` UI control then reaches elevated windows, such
as installer wizards and Device Manager. Those windows refused your input
before the session existed. No special syntax.

## What the user sees
- UAC prompt on `elevate`. The user answers it, and you wait.
- Elevated Control indicator plus the command count in the chat window while
  ON.
- Every elevated command in the log.
- Toggle OFF or `delevate` ends the helper process, and your normal shell
  resumes.
