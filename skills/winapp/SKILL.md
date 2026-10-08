---
name: winapp
description: Inspect and interact with running Windows applications via the winapp CLI. Use for any UI automation — screenshots, clicking, typing, reading app state, window control — and for OS tasks that have a first-party UI (driver/update install, uninstall, settings): open the surface with `start ms-settings:...` or `start devmgmt.msc`, then drive it.
---

# WinApp UI Automation

Use `winapp ui` through `run_command` to inspect and interact with running
Windows applications.

## When to use
- The user asks to click, type, navigate, or read anything in a GUI app
- The user asks for a screenshot or what an app is showing
- You need to verify the visible result of an action
- **The task has a first-party UI — do it through the UI, not by scripting around it.** Open the surface with `start <uri>` (cmd shell), then drive it with the commands below:
  - Windows Update (OS + driver updates): `start ms-settings:windowsupdate`
  - Installed apps / uninstall: `start ms-settings:appsfeatures`
  - Device Manager: `start devmgmt.msc`
  - Network & Internet: `start ms-settings:network`
  - Everything else: `start ms-settings:` (search box inside)
  These pages perform privileged operations with the user's UAC approval; your non-elevated shell cannot — do not fake that with RunAs/elevation helpers.

## Commands

### List open windows (find app names / HWNDs)
```
winapp ui list-windows
```

### Inspect the UI tree (use --depth to limit size)
```
winapp ui inspect -a <app-name> [--depth N]
winapp ui inspect <selector> -a <app-name>
```

### Search for elements (walks the whole tree — see Cost; prefer inspect)
```
winapp ui search "<text>" -a <app-name>
```

### Invoke (activate) an element
```
winapp ui invoke <selector> -a <app-name>
```

### Click an element
```
winapp ui click <selector> -a <app-name>
```

### Send keyboard input
```
winapp ui send-keys ctrl+t -a <app-name>
winapp ui send-keys --target <selector> --via send-input --verbatim "literal text" -a <app-name>
winapp ui send-keys enter -a <app-name>
```
Named keys (enter, tab, esc) and combos (ctrl+shift+t). To TYPE literal text
(search boxes, address bars), use --verbatim with --via send-input, then send
enter as a separate command.

### Set a value directly (often better than typing)
```
winapp ui set-value <selector> <value> -a <app-name>
```

### Screenshot (the image is attached to your context — you CAN see it)
```
winapp ui screenshot -a <app-name>
```
The screenshot image is attached automatically right after the command; look
at it to judge UI state, verify your last action, or find things the UIA tree
does not expose.

## Cost (measured on this machine)
Every winapp call is a cold start (~0.1 s) plus a fresh UIA walk of the
target's tree — there is no cache between calls.

- Small apps: `inspect --depth 2` ≈ 0.3 s; `send-keys` ≈ 0.25 s.
- Browsers (Zen/Firefox/Chromium): ≈ 2 s even at `--depth 2`, ≈ 24 s at
  depth 30; `-i` adds ~8 s more. Scoped `inspect <selector> --depth 12`
  still costs ~11 s.
- `click`/`invoke`/`search` resolve their selector by walking the tree: on a
  browser that costs 20–30 s — they are NOT cheap actions there.
- `screenshot` ≈ 2.5 s regardless of tree size.

On browsers, prefer screenshot over inspect/click: keep `--depth <= 4`, skip
`-i`, and start from a screenshot instead of a tree walk.

## Rules
- Quote multi-word arguments: winapp ui search "Qwen 3.8 Flash Next" -a zen
- Pipes, && and redirection work: tasklist | findstr /i zen
- There is NO 'winapp ui list' command — use 'winapp ui list-windows'.
- If a command errors with "was not matched", your syntax is wrong: run
  `winapp ui <command> --help` once, then use the exact syntax. Never re-guess.
- Element selectors go stale after the UI changes — re-inspect before clicking
  by slug, and never click a selector from an older inspect result twice.

## Input injection contract
- BEFORE `click` or `send-keys --via send-input`: `winapp ui focus <selector>
  -a <app>`. It activates the window, focuses the element, and VERIFIES
  foreground — it fails loudly when Windows refuses, so you never act blind.
- `click` refuses to run unless the target window is foreground. Do not
  activate your own app's window to work around anything; `focus` the target.
- `send-keys` default transport (`post-message`) is window-scoped and needs
  no foreground for classic Win32/WinForms — but XAML/WinUI/UWP apps are
  windowless and IGNORE it silently, and accelerators (ctrl+t) only fire via
  `--via send-input` (which is OS-wide: foreground is mandatory there).
- `set-value` needs no foreground at all — prefer it over typing into text
  boxes; type only when the control rejects ValuePattern.
- If keystrokes "didn't land": do NOT retype. Run `get-focused` (or
  `list-windows`, which marks the foreground window), fix focus, send ONCE.
  Retyping after a silently-partial send is how text gets duplicated.

## Workflow
1. Desktop app: `inspect` with `--depth` to find selectors. Browser: start
   with `screenshot` and only inspect a scoped subtree if you must.
2. `focus` the target, then `invoke`/`click`/`set-value`/`send-keys`.
3. Verify: re-inspect a small subtree (desktop) or screenshot (browser).

## Bootstrap
The app only DETECTS this CLI at startup (`winapp --version`) — it never
installs anything without the user asking; the Setup wizard offers the
winget command below on the user's behalf. If commands report winapp
missing, it is simply not installed — point the user at Setup → Install
winapp CLI, and run the command yourself only after the user agrees:
```
winget install Microsoft.WinAppCli --source winget --accept-package-agreements --accept-source-agreements --disable-interactivity
```
