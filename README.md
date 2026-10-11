# ⚓ First Mate

First Mate is a Windows control agent that lives in the system tray. Summon
First Mate from any application. Describe the job, and First Mate does the job.
First Mate runs commands, drives other applications' windows, and reads and
writes files. First Mate cites its work with clickable links. Every capability
arrives through a skill that loads on demand.

Rust on Tauri v2 provides the backend. Vue 3 on WebView2 provides the frontend.
Capability knowledge follows the [Agent Skills](https://agentskills.io/)
specification, which [docs/SKILLS.md](docs/SKILLS.md) explains.

## Summon

- Press `Super+Alt+Space`. The hotkey is global, so it reaches any application.
- Left-click the ⚓ tray icon.

When you close the window, the app hides the window to the tray and keeps
running. The window has no border and stays above other windows.

## How it works

### Behavior is content

The product's behavior lives in prompt files under `prompts/`. First Mate reads
each file from one of three locations, and the first location with a copy wins:

1. `prompts/<file>` in the dev repo. This is the working copy under test. The
   app reads this layer only while the process runs from `src-tauri/`.
2. `~/.firstmate/<file>`. This is your own copy. It overrides the shipped
   baseline.
3. `<file>` beside `firstmate.exe`. The installer bundles this baseline.

`AGENTS.md` is the product agent's system prompt. `COMPACT.md` states the
instructions the summarizer follows when it folds a long conversation. The
binary contains no prompt text. A prompt file that is missing at every layer
leaves that behavior unconfigured, because no compiled-in text stands in.
Behavior therefore changes in content, not in Rust.

### Capabilities are skills

A skill is a folder of instructions under `prompts/skills/`. Skills resolve
through the same three locations in the same order. `~/.firstmate/skills/`
overrides a packaged skill by name, and First Mate adds new skills from that
directory to the advertised set.

`SKILL.md` frontmatter lists only `name` and `description`. The reference
validator must accept every shipped skill, as `docs/SKILLS.md` explains.

The agent sends its full tool array on every request. Nine tools sit behind the
dispatch gate, because their usage rules live inside a skill's instructions. The
skill gates `read_file`, `edit_file`, `write_file`, `list_dir`, `search_files`,
and `list_processes`. The `elevation` skill gates `elevate`, `run_elevated`, and
`delevate`. The dispatch gate refuses a gated tool until the agent loads its
skill. The refusal names the missing skill, so the model loads it and retries.
`skill_required()` in `src-tauri/src/tooldefs.rs` defines that mapping.

### Conversations persist

First Mate saves each conversation at `~/.firstmate/conversations/<name>.chat`.
The first line is the conversation name. The remaining lines hold one JSON
object per turn, including tool turns. Each tool turn stores a structured
report: the tool, its subject, and the outcome. The console re-renders the
ledger line from that report when you reopen the conversation. The same rows
ride back into the model's history. A resumed conversation therefore knows what
First Mate already inspected and verified.

Each conversation earns capability on its own. The `load_skill` entries in the
ledger re-seed the tool gate on every later turn, including after a resume. A
loaded skill stays loaded for that chat.

### Every store on disk is encrypted

First Mate encrypts every file it writes that holds your data: each `.chat`
transcript, `settings.json`, `entity_registry.json`, `token_map.json`, each exec
spill under `output/`, and each request dump. Encryption is not a setting, and no
store stays plaintext. A debug build also writes `%TEMP%\firstmate.log`, and that
file holds plaintext tool arguments. A release build writes no such file.

`token_map.json` beside `settings.json` holds the marker-to-value table, so the
table needs the same envelope. `read_file` never decrypts `token_map.json`,
because `token_map.json` is the key for every marker First Mate issued.

Each file carries its own AES-256 key. The `FMENC1` header beside the ciphertext
holds that key twice: once wrapped under the Platform Crypto Provider key that
the TPM holds, and once wrapped under your recovery key. That header is the
envelope. The word follows the industry term envelope encryption. Envelope
encryption means a data key that only a second key can unwrap. Every `NCrypt`
call passes `NCRYPT_SILENT_FLAG`, so the app reads its own files at logon with
no prompt.

The setup wizard shows the recovery key, and keeps it on screen until you choose
**I saved it**. `recovery.ack` beside `settings.json` records that choice, so a
run that ends early asks again. A TPM reset or a new PC makes those files
unreadable without the key. The app never shows the key again after you
acknowledge, so keep it somewhere outside this PC. `docs/ENCRYPTION.md` holds
the mechanism, the spike measurement, and the tests that cover the envelope.

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
  `COMPACT.md` states the rules a summary fold follows, and `skills/` contains the
  skill folders. The installer packages these as resources, because the binary
  must contain no prompt text.
- **`docs/`** — design documentation. `ROADMAP.md` records direction and build
  order. `SKILLS.md` explains the skill system, `COMPACTION.md` explains the
  context window, and `ENCRYPTION.md` explains the file envelope. `roadmap/`
  contains the design detail that a roadmap item points to.
- **`AGENTS.md`** — guidance for coding agents working on this repo.

## Configuration

LLM settings live in `%APPDATA%\com.dusti.firstmate\settings.json`. Open the tray
menu and choose **Settings** to edit them. The `llm` section defines `provider`,
`model`, `api_key`, `base_url`, `context_window`, and `compact_at_percent`.

Four keys drive redaction. `hide_sensitive` maps a provider id to the
`Hide sensitive information` choice. A provider with no entry takes the default
its roster row declares, and every hosted provider starts on. The entity registry
lives in `entity_registry.json` beside `settings.json`. It lists exact strings the
pass replaces, for values no shape identifies, such as a bank account number.
`decision_server_url`, `decision_server_enabled`, and `decision_threshold` point
the span gate at the decision server you run. `decision_server_remote_ok` records
that you let a host off this machine receive every span it scores.

The selectable providers come from `src-tauri/src/providers.json`, which the
binary embeds at compile time. Adding a provider is an edit to that file. You
need no Rust change, because request building switches on the row's `api` and
`auth` fields. The roster includes local engines too, so you need no cloud
account.

`base_url` appears only for the rows marked `isLocal`: `ollama`, `lm-studio`,
`llama-cpp`, and `custom`. `custom` has no default endpoint, so `base_url` names
the OpenAI-compatible server to use. `context_window` of 0 asks the endpoint for
its window size. `compact_at_percent` sets how full the window may get before
First Mate folds the conversation, and the default is 85.

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
two files are the single source of the version, because `tauri.conf.json` defines
no version field. Merging the release pull request publishes a GitHub Release.

The `Release Build` workflow then builds the Windows bundles on the pipeline.
That workflow signs the bundles with the updater minisign key. The same workflow
uploads the installers plus `latest.json`. The app checks `latest.json` on
launch and offers a one-click update from the tray.

The project has no Authenticode signature, so SmartScreen shows "unknown
publisher" once per version. SignPath Foundation offers free code signing for
open-source projects. Azure Artifact Signing is the paid option.

## License

Apache-2.0. See [LICENSE](LICENSE).
