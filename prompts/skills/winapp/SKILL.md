---
name: winapp
description: "Inspect and interact with running Windows applications via the winapp CLI. Use for any UI automation — screenshots, clicking, typing, reading app state, window control — for the web (open the page in the browser and read it there), and for OS tasks that have a first-party UI (driver/update install, uninstall, settings): open the surface with `start ms-settings:...` or `start devmgmt.msc`, then drive it."
---

# WinApp UI Automation

Use `winapp ui` through `run_command` to inspect and interact with running
Windows applications.

- The user asks to click, type, navigate, or read anything in a GUI app.
- The user asks for a screenshot or for what an app is showing.
- You need to verify the visible result of an action.
- **A surface for the task already exists. Use that surface instead of writing
  a script for the task.** Open the surface with `start <uri>` (cmd shell),
  then drive it with the commands below:
  - The web: `start <url>` opens the default browser. Read pages by
    screenshot, because you can see the image. Search through the address
    bar. Do not fetch a site's HTML in a terminal. A person at this desk
    would open the browser, and you can do the same.
  - Windows Update (OS and driver updates): `start ms-settings:windowsupdate`
  - Installed apps and uninstall: `start ms-settings:appsfeatures`
  - Device Manager: `start devmgmt.msc`
  - Network & Internet: `start ms-settings:network`
  - Everything else: `start ms-settings:` (search box inside)
  These pages perform privileged operations with the user's UAC approval. Your
  non-elevated shell cannot. Never substitute RunAs for that approval. When a
  privileged job has no surface worth driving, load the **elevation** skill
  (`load_skill("elevation")`).

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
Named keys are enter, tab, and esc. Write a combo as ctrl+shift+t. To
TYPE literal text into a search box or an address bar, use `--verbatim` with
`--via send-input`. Then send enter as a separate command.

### Set a value directly (often better than typing)
```
winapp ui set-value <selector> <value> -a <app-name>
```

### Screenshot (the image is attached to your context — you CAN see it)
```
winapp ui screenshot -a <app-name>
```
The image attaches automatically right after the command. Look at it to judge
UI state or to verify your last action. The image also shows what the UIA tree
does not expose.

## Cost (measured on this machine)
Every winapp call pays a cold start (~0.1 s) and then walks the target's UIA
tree from scratch. winapp caches nothing between calls.

- Small apps: `inspect --depth 2` ≈ 0.3 s, `send-keys` ≈ 0.25 s.
- Browsers (Zen/Firefox/Chromium): ≈ 2 s even at `--depth 2`, and ≈ 24 s at
  depth 30. `-i` adds ~8 s more. A scoped `inspect <selector> --depth 12`
  still costs ~11 s.
- `click`, `invoke`, and `search` resolve their selector by walking the tree.
  On a browser that costs 20–30 s, so none of them is cheap there.
- `screenshot` ≈ 2.5 s regardless of tree size.

On browsers, prefer `screenshot` over `inspect` and `click`. Keep
`--depth <= 4` and skip `-i`. Start from a screenshot instead of a tree walk.

## Rules
- Quote multi-word arguments: `winapp ui search "Qwen 3.8 Flash Next" -a zen`.
- Pipes, `&&`, and redirection work: `tasklist | findstr /i zen`.
- There is NO `winapp ui list` command. Use `winapp ui list-windows`.
- If a command errors with "was not matched", your syntax is wrong. Run
  `winapp ui <command> --help` once, then use that exact syntax. Never guess a
  second time.
- Element selectors go stale after the UI changes. Re-inspect before you click
  by slug. Do not click twice with a selector from an older inspect result.

## Input injection contract
- BEFORE `click` or `send-keys --via send-input`, run `winapp ui focus
  <selector> -a <app>`. `focus` activates the window, focuses the element, and
  verifies the foreground result. `focus` returns an error when Windows refuses,
  so you learn of the refusal before you act, not after.
- `click` refuses to run unless the target window is foreground. `focus` the
  target. Never activate First Mate's own window to satisfy that check.
- The `send-keys` default transport (`post-message`) is window-scoped and needs
  no foreground for classic Win32 and WinForms apps. XAML, WinUI, and UWP apps
  have no such window and IGNORE `post-message` with no error. Accelerators
  such as ctrl+t fire only through `--via send-input`. That transport is
  OS-wide, so the target window must be foreground.
- `set-value` needs no foreground. Prefer it over typing into a text box. Only
  when the control rejects ValuePattern, type the text.
- If keystrokes "didn't land", do NOT retype. Run `get-focused`, or run
  `list-windows`, which marks the foreground window. Fix the focus, then send
  ONCE. Retyping after a partial send duplicates the text.

## Workflow
1. Desktop app: `inspect` with `--depth` to find selectors. Browser: start
   with `screenshot`, and inspect a scoped subtree only if you must.
2. `focus` the target, then `invoke`, `click`, `set-value`, or `send-keys`.
3. Verify the result: re-inspect a small subtree on a desktop app, or take a
   screenshot of a browser.

## Bootstrap
The app only checks for this CLI at startup (`winapp --version`). The app never
installs anything unless the user asks. The Setup wizard offers the `winget`
command below on the user's behalf. If commands report winapp missing, the CLI
is not installed. Point the user at Setup → Install winapp CLI. Only after the
user agrees, run the command yourself:
```
winget install Microsoft.WinAppCli --source winget --accept-package-agreements --accept-source-agreements --disable-interactivity
```
