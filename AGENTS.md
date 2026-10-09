# AGENTS.md — guidance for coding agents working on this repo

First Mate is a Windows desktop assistant that the user summons with a hotkey.
Tauri 2 provides the Rust backend. Vue 3 provides the frontend. An agent drives
the desktop through skills.

The Rust core is deliberately small, because content carries the product's
behavior. Most work in this repo is writing content rather than writing code.

## Philosophy

- **Content is not code.** First Mate's identity is `prompts/AGENTS.md`. First
  Mate's capabilities are `prompts/skills/*/SKILL.md`. The binary contains no
  prompt text, and a test enforces that. Before you implement a behavior in Rust,
  ask whether that behavior is really content. `docs/SKILLS.md` explains the
  design.
- **Every capability is a skill.** A behavior that the product agent learns ships
  as an Agent Skills folder. `SKILL.md` frontmatter stays spec-pure: `name` and
  `description` only, and the reference validator must pass. Skill content never
  names a tool, because tools belong to the client. The skill-to-tool mapping is
  Rust policy in `skill_required()`. A gated tool is refused at dispatch until its
  skill is loaded. `src-tauri/tests/packaging.rs` enforces packaging.
- **The user's machine is the user's.** The product runs without elevation by
  default. Administrator reach exists only behind explicit user consent, and the
  user can revoke that consent. Every surface the agent drives is a surface the
  user could drive in person.
- **One feature module per concern, and IPC glue in `lib.rs` only.** A feature
  module exposes plain functions. The command wrappers live beside the handler
  table in `lib.rs`. That placement keeps the webview runtime out of a binary that
  merely links the module.
- **Gates are not optional.** Every backend change passes `cargo test` and
  `cargo clippy --all-targets -- -D warnings`. Every frontend change passes
  `npm run lint`, which runs eslint and prettier. Every commit follows
  Conventional Commits, and `.githooks/commit-msg` checks that. A commit body
  states what is true now. A commit body never narrates how you found that out.
- **Done means observed.** A change is finished when the running app has shown
  the effect. The proof is a screenshot, a process state, or a log line. Compiling
  cleanly is not proof, and a passing mock is not proof.

## Layout

The README section "Layout" says where code belongs. In short: `src-tauri/src/`
holds feature modules, `src/` holds the frontend, `prompts/` holds runtime
content, and `docs/` holds design documentation.

`docs/ROADMAP.md` holds direction and build order. That document is living, so
consult it before feature work and update it when priorities move.

## Commands (Windows host; repo at C:\repos\firstmate)

- Dev loop: `npm run tauri dev`. Vite serves port 1420.
- Gates: run `cargo test` and clippy from `src-tauri/`. Run `npm run lint` for the
  frontend, and `npm run format` fixes what prettier reports.
- Full frontend build: `npm run build`, which runs lint, then `tsc`, then
  `vite build`.
