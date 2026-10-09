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
- **Comments follow five checks, in order.** A developer deletes a comment at
  the first check it fails. A rewrite changes form and adds no information.
  Check 5 never restores a comment that an earlier check deleted.

  1. No thinking aloud. A comment that records the author's reasoning, doubts,
     or the steps the author followed is noise. A developer deletes that noise.
     A comment that explains a language, library, or platform feature repeats a
     manual a developer can read. Check 1 deletes that comment.
  2. No provenance. A comment that describes a historical event leading to the
     current state is noise. A historical note records the author's reasoning
     about the code, so check 2 deletes that note.
  3. No "what" comments. Check 3 names the reader: a developer who knows the
     language, the framework, and the neighbouring files. If that developer
     understands the code without the comment, the comment serves no purpose,
     and check 3 deletes the comment.
  4. Strictly why, and only what no type or test can carry. A comment that
     states a contract belongs in a type. A comment that states a constraint
     belongs in a test or an assertion. A comment that survives checks 1 to 3
     carries a reason no type can hold. No test can assert that reason. That
     reason names a trap this codebase hit, or a measurement about the world
     outside the code.
  5. The tone rules apply. A comment that survives checks 1 to 4 follows the
     prose rules in the next bullet. Those rules govern every sentence in this
     repo.

  A developer who cannot name the decision a comment protects deletes that
  comment. A type or a test holds most such decisions.

- **Prose follows the tone rules everywhere.** These rules govern every
  sentence in a comment, a doc, a prompt, and a commit body.

  - Prose shall be written in Simplified Technical English.
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
  - Idiomatic shorthand shall be replaced with the mechanism it stood for.
  - Ceremony, hedging, filler, and marketing shall be deleted. The subject, the
    word "because", and any clause that limits a claim shall never be deleted.
  - The deletions in checks 1 to 3 apply to docs and prompts, not to comments
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
