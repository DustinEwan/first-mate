# ⚓ First Mate

A Windows control agent that lives in your system tray. Summon it anywhere,
ask it to do something on the machine, and it gets done: it runs commands,
drives other applications' UIs, reads and writes files, and cites its work
with clickable links — all through skill-loaded tools.

Built on [Tauri v2](https://tauri.app) (Rust backend) + Vue 3 (WebView2
frontend). Capability knowledge follows the
[Agent Skills](https://agentskills.io/) spec — see
[docs/SKILLS.md](docs/SKILLS.md).

## Summon

- **`Super+Alt+Space`** anywhere (global hotkey)
- **Left-click the tray icon** (⚓)

Closing the window hides it to the tray; the app keeps running. The window is
borderless and always-on-top.

## How it works

- **Behavior** comes from `AGENTS.md` — searched in cwd, `../` (dev layout),
  the exe directory and its parent, then `~/.firstmate/AGENTS.md`. Edit
  behavior as content, not code.
- **Capabilities** are skills in `skills/` (plus user skills in
  `~/.firstmate/skills/`). Only five core tools are in context at start
  (`run_command`, `get_command_output`, `kill_command`, `load_skill`,
  `read_skill_resource`); each skill's `tools:` frontmatter unlocks more when
  the model loads it. A tool that isn't disclosed is refused at dispatch.
- **Conversations** persist to `~/.firstmate/conversations/*.chat`, including
  the tool ledger (`🔧 call -> result` lines), so a resumed chat knows what
  was already inspected and verified. Tool availability resets to core on
  resume.
- **Links** in answers leave the chat window via a validated `open_path`
  command (`ShellExecuteW`): `http(s)` opens your default browser, `file://`
  opens the file's default app. The chat webview never navigates.

## Layout

```
src-tauri/src/            Rust backend, one feature module per concern:
  lib.rs                    app wiring: plugins, IPC table, tray, hotkey
  llm.rs                    chat loops (lmkit + OpenAI-compatible), stop button
  execute.rs route.rs       tool dispatch, shell-dup interception
  exec.rs psrep.rs          command runner, background jobs, PowerShell session
  tooldefs.rs skills.rs     tool schemas + disclosure, skill discovery
  report.rs fileedit.rs     tool facts for the report panel, edit ops + diffs
  conversations.rs log.rs settings.rs window.rs modelapi.rs openpath.rs
  harness.rs                persistence, logging, settings, windows, model
                            listing, path opening, winapp CLI bootstrap
src/ChatView.vue          chat window (virtualized message list, markdown)
settings.html             settings window (provider, model, API key, base URL)
skills/                   winapp, filesystem, git-repo
docs/SKILLS.md            skill system design
AGENTS.md                 system prompt (behavior rules for the model)
```

## Configuration

LLM settings live in `%APPDATA%\com.dusti.firstmate\settings.json` and are
edited from the tray menu → **Settings**: provider (`openai`, `ollama`,
`anthropic`, or `custom` OpenAI-compatible via `base_url`), `model`,
`api_key`. Works with local servers (e.g. llama.cpp, Ollama) — no cloud
dependency required.

## Development

Prerequisites: Windows 11, Node.js, Rust (MSVC toolchain + VS Build Tools),
WebView2.

```
npm install            # frontend deps
dev-server.bat         # vite dev server on :1420 (keep running)
cargo-vs.bat build     # build the Rust binary via VS env  (also: run, test)
build-fe.bat           # production frontend bundle into dist/
```

`cargo-vs.bat` wraps cargo with the Visual Studio environment so the MSVC
linker is found; run it from `src-tauri/`. The debug binary is
`src-tauri/target/debug/firstmate.exe`.

## Quality gates

```
npm run check          # eslint (--max-warnings 0) + prettier + clippy -D warnings
git config core.hooksPath .githooks   # once per clone: Conventional Commits hook
```

Rust builds deny warnings workspace-wide (`[lints]` in `src-tauri/Cargo.toml`);
the frontend build fails on any lint error or formatting drift. Commit
messages must be Conventional Commits — `<type>(<scope>): <subject>`, types
feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert, `!` for breaking.

## Tests

```
cd src-tauri && cargo-vs.bat test
```

## Releasing

Releases are automatic: merge Conventional Commit PRs to `main`, and
[release-please](https://github.com/googleapis/release-please) opens a release
PR that bumps `package.json` and `src-tauri/Cargo.toml` (single source of
truth — `tauri.conf.json` carries no version). Merging it publishes a GitHub
Release; the `Release Build` workflow then builds the Windows bundles on the
pipeline, signs them with the updater minisign key, and uploads the
installers plus `latest.json`. The app checks that endpoint on launch and
offers an in-tray one-click update.

Authenticode is not configured (SmartScreen shows "unknown publisher" once
per version). Free option: apply to SignPath Foundation for OSS code
signing; paid: Azure Artifact Signing.

## License

Apache-2.0 — see [LICENSE](LICENSE).
