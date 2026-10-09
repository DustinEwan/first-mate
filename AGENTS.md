# AGENTS.md — guidance for coding agents working on this repo

First Mate is a hotkey-summoned Windows desktop assistant: Tauri 2
(Rust) backend, Vue 3 frontend, and an agent that drives the desktop
through skills. This repo ships a small Rust core whose whole job is to
get out of the way of content — the product's behavior is written, not
coded.

## Philosophy

- **Content is not code.** First Mate's identity is `prompts/AGENTS.md`
  and its capabilities are `prompts/skills/*/SKILL.md`. The binary ships
  zero prompt text; a test enforces it. Any behavior you are tempted to
  implement in Rust, first ask whether it is really content. Design:
  `docs/SKILLS.md`.
- **Every capability is a skill.** Behavior the product agent learns
  ships as Agent Skills folders: `SKILL.md` frontmatter is spec-pure
  (name + description; the reference validator must pass) and never
  names tools. Tools belong to the client; the skill→tools mapping is
  Rust policy (`skill_required()`), and gated tools are refused at
  dispatch until their skill is loaded. Packaging is enforced by
  `src-tauri/tests/packaging.rs`.
- **The user's machine is the user's.** The product runs
  non-elevated by default; admin reach exists only behind explicit,
  revocable user consent, and every surface the agent drives is one the
  user could drive themselves.
- **One feature module per concern; IPC glue in `lib.rs` only.** Feature
  modules expose plain functions; the command wrappers live with the
  handler table. This keeps the webview runtime out of binaries that
  merely link the module.
- **Gates are not optional.** Every backend change passes `cargo test`
  and `cargo clippy --all-targets -- -D warnings`. Every frontend change
  passes `npm run lint` (eslint + prettier). Every commit follows
  Conventional Commits (`.githooks/commit-msg`); commit bodies state
  durable fact — what is true now — not the path taken to find out.
- **Done means observed.** A change is finished when the running app has
  demonstrated the effect — a screenshot, a process state, a log line —
  not when it compiles or a mock passes.

## Layout

See README "Layout" for where code belongs. In short: `src-tauri/src/`
feature modules, `src/` frontend, `prompts/` runtime content, `docs/`
design documentation. Direction and build order live in
`docs/ROADMAP.md` — a living document: consult it before feature work,
update it when priorities move.

## Commands (Windows host; repo at C:\repos\firstmate)

- Dev loop: `npm run tauri dev` (vite on port 1420)
- Gates: `cargo test` + clippy from `src-tauri/`; `npm run lint` (fix with `npm run format`)
- Full frontend build: `npm run build` (lint + tsc + vite build)
