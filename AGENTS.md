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
- **Comments follow five rules.**

  1. No thinking aloud. A comment that records the author's reasoning, doubts,
     or the path taken to write the code is noise. That noise is deleted.
  2. No provenance. A comment that describes a historical event leading to the
     current state is noise. That comment is thinking aloud in disguise.
  3. No "what" comments. A comment that describes what the code does is noise.
     The reader is assumed to be technically competent. If the code can be
     understood without the comment, the comment serves no purpose.
  4. Strictly why. A surviving comment carries context that the code cannot
     communicate on its own. That context gives the reason a decision was made,
     when that reason is not obvious otherwise.
  5. The tone rules apply. A surviving comment follows the prose rules in the
     next bullet. Those rules govern every sentence in this repo.

- **Prose follows the tone rules everywhere.** These rules govern every
  sentence in a comment, a doc, a prompt, and a commit body.

  - Prose is written in Simplified Technical English.
  - A sentence states one fact in at most 20 words.
  - A sentence is active, and it carries an explicit subject and verb.
  - One verb describes each action.
  - A concrete noun replaces an abstract noun where both fit.
  - A sentence names its referent instead of a pronoun. A pronoun that would
    force the reader to reopen a file or a diff forces a rewrite.
  - A fragment is allowed only in a table cell, a bullet label, or a heading. A
    fragment never carries a causal claim on its own.
  - Explanation runs in this order: conclusion, then evidence, then what
    changes as a result.
  - Mechanism replaces conclusion. Each explanation names the cause, the
    constraint, and the observable result.
  - Writing is concrete: the exact file, symbol, API, state field, edge case,
    and the check that proves the claim.
  - Idiomatic shorthand is replaced with the mechanism it stood for.
  - Ceremony, hedging, filler, and marketing are deleted. The subject, the word
    "because", and any clause that limits a claim are never deleted.
  - The deletions in rules 1 to 3 apply to docs and prompts, not to comments
    alone.

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
