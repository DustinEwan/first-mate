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

- **Behavior** comes from prompt content in `prompts/`, layered: dev repo layout
  wins outright (working copy under test), then `~/.firstmate/<file>` — which
  OVERRIDES the packaged baseline — then the packaged copy next to the exe
  (bundled by the installer). `AGENTS.md` is the system prompt; `COMPACT.md` is
  what the summarizer is told when a long conversation is folded. The binary
  ships no prompt text. Edit behavior as content, not code.
- **Capabilities** are skills in `prompts/skills/`, layered the same way: user skills
  in `~/.firstmate/skills/` override packaged ones by name and extend the
  set. Skills are pure knowledge per the
  [Agent Skills](https://agentskills.io/) spec — `SKILL.md` frontmatter is
  only `name` + `description`, validated. The agent's full tool array rides
  every request; tools whose rules a skill teaches (the file tools, the
  admin trio) are refused at dispatch until that skill is loaded, and the
  refusal names the skill to load.
- **Conversations** persist to `~/.firstmate/conversations/*.chat`, including
  the tool ledger (`🔧 call -> result` lines), so a resumed chat knows what
  was already inspected and verified. Capability is earned per conversation:
  the ledger's `load_skill` entries re-seed the tool gate on every later turn,
  including after a resume — a skill loaded once stays loaded for that chat.
- **Links** in answers leave the chat window via a validated `open_path`
  command (`ShellExecuteW`): `http(s)` opens your default browser, `file://`
  opens the file's default app. The chat webview never navigates.

## Layout

- **`src-tauri/`** — Rust backend. One feature module per concern: the chat
  loops, command execution, tool dispatch, skills, elevation, persistence,
  model endpoints. Tauri's IPC command glue lives only in `lib.rs`, so a
  module never drags the webview runtime into a binary that just links it.
- **`src/` + `settings.html`** — the Vue console and the settings window.
- **`prompts/`** — the product's behavior: `AGENTS.md` (the system prompt),
  `COMPACT.md` (the rules a fold is written under) and `skills/`. Packaged as
  installer resources, never compiled in.
- **`docs/`** — design documentation. `ROADMAP.md` carries direction and
  build order; `SKILLS.md` the skill system; `COMPACTION.md` the context
  window; `roadmap/` the design detail a roadmap item points at.
- **`AGENTS.md`** — guidance for coding agents working on this repo.

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
npm install           # frontend deps
npm run tauri dev     # vite on :1420 plus the Rust core, one command
npm run build         # lint + tsc + production bundle into dist/
```

`cargo` needs the MSVC linker in its environment: run it from a Visual Studio
Developer Prompt, or call `vcvars64.bat` first — otherwise rustc can't find
`link.exe`. The debug binary is `src-tauri/target/debug/firstmate.exe`.

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
cd src-tauri && cargo test
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
