# AGENTS.md — guidance for coding agents working on this repo

First Mate: a hotkey-summoned Windows desktop assistant. Tauri 2 (Rust)
backend + Vue 3 / naive-ui / vue-virtual-scroller frontend. The agent it
exposes drives the desktop through skills (winapp UI automation, a
persistent PowerShell session, an opt-in elevated helper).

**Two AGENTS.md files exist and they are NOT the same thing.** This file
(repo root) is for coding agents contributing to the repo.
`prompts/AGENTS.md` is First Mate's own system prompt — runtime content
shipped to end users. Never merge, rename, or "dedupe" them.

## Layout

See README "Layout" for the module map. The invariant it doesn't show:
`src-tauri/src/` is one feature module per concern, and `lib.rs` is the
only place with `#[tauri::command]` glue (see Gotchas).

## Commands (Windows host; repo at C:\repos\firstmate)

- Dev loop: `npm run tauri dev` (vite on port 1420, app auto-rebuilds on Rust changes)
- Frontend gate: `npm run lint` (eslint `--max-warnings 0` + prettier `--check`); `npm run format` to fix
- Rust gate: `cargo clippy --all-targets -- -D warnings` (or `npm run lint:rust`) from `src-tauri/`
- Tests: `cargo test` from `src-tauri/` — unit tests run with cwd = `src-tauri` and read `../prompts/` directly
- Full frontend build: `npm run build` (lint + tsc + vite build)

Commits must pass `.githooks/commit-msg`: Conventional Commits
(`fix(scope): subject`, ≤72 chars). Match the existing history's style —
imperative subject, body explains the _why_ with the evidence.

## Prompt content is content, not code

- The binary ships **zero prompt text**. `prompts/AGENTS.md` +
  `prompts/skills/*/SKILL.md` are resolved at runtime, layered: dev repo
  copy → `~/.firstmate/` (user override) → packaged resources next to the
  exe. A unit test asserts the prompt equals the repo file — if you feel
  the urge to embed prompt-ish text in Rust, the test will fight you.
- Adding a skill: create `prompts/skills/<name>/SKILL.md` (frontmatter
  `name:` must equal the folder), add one `bundle.resources` line per file
  in `src-tauri/tauri.conf.json`, and add the name to both lists in
  `src-tauri/tests/packaging.rs`. The packaging test fails otherwise.

## Gotchas (each one cost a debugging session; do not re-pay)

- **`#[tauri::command]` glue lives in `lib.rs`, not feature modules.**
  A Rust module links as one object; command glue references tauri's IPC
  machinery, so a `#[tauri::command]` inside e.g. `elevate.rs` drags the
  webview stack into every binary that touches any function in that
  module — including the unit-test exe, which then dies at load
  (`STATUS_ENTRYPOINT_NOT_FOUND`). Feature modules expose plain fns;
  `lib.rs` wraps them.
- **Exactly one RT_MANIFEST per binary.** `build.rs` emits
  `/MANIFEST:EMBED` + `/MANIFESTDEPENDENCY` via `cargo:rustc-link-arg`
  (the only manifest hook that also reaches the lib unit-test exe) and
  suppresses tauri_build's embedded copy (`new_without_app_manifest`).
  Re-adding either source collides: `CVTRES CVT1100 duplicate resource`.
  `.cargo/config.toml` rustflags are NOT the fix — they hit the bin too
  and cargo discovers them from CWD, not the manifest path.
- **Elevation pipe security is load-bearing.** The HIGH-IL helper creates
  its named pipe with a LOW mandatory label (`S:(ML;;NW;;;LW)`, needs
  `SeSecurityPrivilege` enabled in-process) so the medium-IL app may
  write through NoWriteUp. Remove the label and elevation fails with
  `Access is denied (5)`; widen the DACL beyond `IU` and you've built a
  UAC bypass. The real gates are the nonce protocol + client PID/exe-path
  verification — keep them.
- **DynamicScroller measures asynchronously.** `scrollHeight` right after
  a bulk message load is an estimate that can momentarily "equal bottom";
  scroll pinning must wait for `scrollHeight` to be _stable_ (see
  `scrollToBottom` in `ChatView.vue` and commit 870f96b). Fixed rAF +
  timeout chains regress on large conversations.
- **cargo config/worktree hygiene:** run cargo commands with cwd inside
  the repo; `git mv` of `prompts/` fails while the app is running (it
  holds the skill dirs open).

## Verifying changes like the maintainers do

- Backend: `cargo test` + clippy gate above; packaging test guards skill
  shipping.
- UI/e2e: launch the debug build and drive it for real — `winapp` CLI
  (installed) does `ui list-windows`, `ui inspect`, `ui screenshot`, and
  coordinate `ui drag` clicks; the webview's DOM is not in the UIA tree,
  so screenshots + computed titlebar coordinates are how buttons get
  clicked. For frontend internals, restart dev with
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`
  and talk CDP (`Runtime.evaluate`) against the page.
- Elevation e2e: this dev box has `ConsentPromptBehaviorAdmin=0` — UAC
  auto-approves, so the whole ON→HIGH-IL-helper→OFF lifecycle is
  automatable; verify the helper's label is `S-1-16-12288` and that
  teardown kills it.
