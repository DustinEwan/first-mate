# ⚓ First Mate

First Mate is a Windows control agent that lives in the system tray. Summon it
from any application. Describe the job, and First Mate does the job. It runs
commands, it drives other applications' windows, and it reads and writes files.
It cites its work with clickable links. Every capability arrives through a skill
that loads on demand.

Rust on Tauri v2 provides the backend. Vue 3 on WebView2 provides the frontend.
Capability knowledge follows the [Agent Skills](https://agentskills.io/)
specification, which [docs/SKILLS.md](docs/SKILLS.md) explains.

## Summon

- Press `Super+Alt+Space`. The hotkey is global, so it reaches any application.
- Left-click the ⚓ tray icon.

Closing the window hides it to the tray. The app keeps running. The window has
no border, and it stays above other windows.

## How it works

### Behavior is content

The product's behavior lives in prompt files under `prompts/`. Three locations
can carry each file, and the first location that holds a copy wins:

1. `prompts/<file>` in the dev repo. This is the working copy under test, and it
   wins only while `src-tauri/` sits beside it.
2. `~/.firstmate/<file>`. This is your own copy. It overrides the shipped
   baseline.
3. `<file>` beside `firstmate.exe`. The installer bundles this baseline.

`AGENTS.md` is the product agent's system prompt. `COMPACT.md` holds the
instructions the summarizer follows when a long conversation is folded. The
binary contains no prompt text. A prompt file that is missing at every layer
leaves that behavior unconfigured, because nothing compiled-in stands in. Change
behavior by editing content.

### Capabilities are skills

A skill is a folder of instructions under `prompts/skills/`. Skills resolve
through the same three locations in the same order. `~/.firstmate/skills/`
overrides a packaged skill by name. That directory also adds new skills to the
set.

`SKILL.md` frontmatter carries only `name` and `description`. The reference
validator must accept every shipped skill, as `docs/SKILLS.md` explains.

The agent sends its full tool array on every request. Nine tools are gated,
because their usage rules live inside a skill's instructions. The `filesystem`
skill gates `read_file`, `edit_file`, `write_file`, `list_dir`, `search_files`,
and `list_processes`. The `elevation` skill gates `elevate`, `run_elevated`, and
`delevate`. A gated tool is refused until its skill is loaded. The refusal names
the missing skill, so the model loads it and retries. `skill_required()` in
`src-tauri/src/tooldefs.rs` holds that mapping.

### Conversations persist

A conversation is saved at `~/.firstmate/conversations/<name>.chat`. The first
line is the conversation name. The remaining lines are one JSON object per turn.
Tool turns are saved too. Each tool turn carries a structured report: the tool,
its subject, and the outcome. The console re-renders the ledger line from that
report when you reopen the conversation. The same rows ride back into the model's
history. A resumed conversation therefore knows what was already inspected and
verified.

Capability is earned per conversation. The `load_skill` entries in the ledger
re-seed the tool gate on every later turn, including after a resume. A skill that
was loaded once stays loaded for that chat.

### Links leave the chat window

A link in an answer opens outside the chat window, through the `open_path`
command. `open_path` validates the target first, because the chat webview never
navigates itself. An `http(s)` target opens your default browser. A `file://`
target opens the file's default application. `ShellExecuteW` performs the open,
which is the same call that Explorer's own user interface uses.

## Layout

- **`src-tauri/`** — the Rust backend. One feature module owns one concern. The
  chat loops, command execution, tool dispatch, skills, elevation, persistence,
  and model endpoints each have their own module. Tauri's IPC command glue lives
  only in `lib.rs`. A feature module therefore links into a test binary without
  the webview runtime.
- **`src/` + `settings.html`** — the Vue console and the settings window.
- **`prompts/`** — the product's behavior. `AGENTS.md` is the system prompt,
  `COMPACT.md` holds the rules a summary fold follows, and `skills/` holds the
  skill folders. The installer packages these as resources, because the binary
  must contain no prompt text.
- **`docs/`** — design documentation. `ROADMAP.md` carries direction and build
  order. `SKILLS.md` explains the skill system, and `COMPACTION.md` explains the
  context window. `roadmap/` holds the design detail that a roadmap item points
  to.
- **`AGENTS.md`** — guidance for coding agents working on this repo.

## Configuration

LLM settings live in `%APPDATA%\com.dusti.firstmate\settings.json`. Open the tray
menu and choose **Settings** to edit them. The `llm` section holds `provider`,
`model`, `api_key`, `base_url`, `context_window`, and `compact_at_percent`.

The selectable providers come from `src-tauri/src/providers.json`, which the
binary embeds at compile time. Adding a provider is an edit to that file. No Rust
change is needed, because request building switches on the row's `api` and `auth`
fields. Local engines are in the roster too, so no cloud account is required.

`base_url` appears only for the rows marked `isLocal`: `ollama`, `lm-studio`,
`llama-cpp`, and `custom`. `custom` has no default endpoint, so `base_url` names
the OpenAI-compatible server to use. `context_window` of 0 asks the endpoint for
its window size. `compact_at_percent` sets how full the window may get before the
conversation is folded, and the default is 85.

## Development

Prerequisites: Windows 11, Node.js, Rust with the MSVC toolchain and Visual
Studio Build Tools, and WebView2.

```
npm install           # frontend deps
npm run tauri dev     # vite on :1420 plus the Rust core, one command
npm run build         # lint + tsc + production bundle into dist/
```

`cargo` needs the MSVC linker in its environment, because rustc cannot find
`link.exe` otherwise. Run `cargo` from a Visual Studio Developer Prompt, or call
`vcvars64.bat` first. The debug binary is
`src-tauri/target/debug/firstmate.exe`.

## Quality gates

```
npm run check          # eslint (--max-warnings 0) + prettier + clippy -D warnings
git config core.hooksPath .githooks   # once per clone: Conventional Commits hook
```

Rust builds deny warnings workspace-wide through `[lints]` in
`src-tauri/Cargo.toml`. The frontend build fails on any lint error or on
formatting drift. A commit message must be a Conventional
Commit: `<type>(<scope>): <subject>`. The types are `feat`, `fix`, `docs`,
`style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, and `revert`. A `!`
marks a breaking change.

## Tests

```
cd src-tauri && cargo test
```

## Releasing

Releases are automatic. Merge Conventional Commit pull requests into `main`.
[release-please](https://github.com/googleapis/release-please) then opens a
release pull request that bumps `package.json` and `src-tauri/Cargo.toml`. Those
two files are the single source of the version, because `tauri.conf.json` carries
no version field. Merging the release pull request publishes a GitHub Release.

The `Release Build` workflow then builds the Windows bundles on the pipeline.
That workflow signs the bundles with the updater minisign key. The same workflow
uploads the installers plus `latest.json`. The app checks that endpoint on launch
and offers a one-click update from the tray.

Authenticode is not configured, so SmartScreen shows "unknown publisher" once per
version. SignPath Foundation offers free code signing for open-source projects.
Azure Artifact Signing is the paid option.

## License

Apache-2.0. See [LICENSE](LICENSE).
