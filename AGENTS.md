# AGENTS.md — guidance for coding agents working on this repo

First Mate is a Windows desktop assistant that the user summons with a hotkey.
Tauri 2 provides the Rust backend. Vue 3 provides the frontend. An agent drives
the desktop through skills.

The Rust core is deliberately small, because content defines the product's
behavior. Most work in this repo is content, not code.

## Philosophy

- **Documentation is the source of truth.** When code and a document under `docs/`
  disagree, the document is right, and the code is wrong. Change the code. Change
  design text only when the user decides that text is wrong. Do not change design
  text in the commit that changes behavior. The behavior commit may move a
  checklist box. When no document governs a behavior, ask the user before you build
  the behavior.
- **Content is not code.** First Mate's identity is `prompts/AGENTS.md`. First
  Mate's capabilities are `prompts/skills/*/SKILL.md`. The binary contains no
  prompt text. A test enforces that requirement. Before you implement a
  behavior in Rust, ask whether that behavior is really content.
  `docs/SKILLS.md` explains the design.
- **Every capability is a skill.** Ship every behavior the product agent learns
  as an Agent Skills folder. Keep `SKILL.md` frontmatter spec-pure: `name` and
  `description` only. Make the reference validator pass on every skill folder.
  A skill body may name a tool, because that body teaches the agent which tool to
  call. The skill-to-tool mapping is Rust policy in `skill_required()`. The dispatch
  gate refuses a gated tool until the agent loads its skill.
  `src-tauri/tests/packaging.rs` enforces packaging.
- **The user's machine is the user's.** The product runs without elevation by
  default. Administrator reach exists only behind explicit user consent. The
  user can revoke that consent. Every surface the agent drives is a surface the
  user could drive in person.
- **One feature module per concern, and IPC glue in `lib.rs` only.** Expose
  plain functions from each feature module. Put the command wrappers beside the
  handler table in `lib.rs`. A binary that only links the module then needs no
  webview runtime.
- **Apply five checks to every comment, in order.** Delete a comment at the
  first check that the comment fails. Do not rewrite a comment that fails a
  check. A rewrite changes the form only. Do not restore a comment that an
  earlier check deleted.

  1. No thinking aloud. If a comment records the author's reasoning, doubts, or
     steps, delete the comment. If a comment explains a language, library, or
     platform feature, delete the comment. Such text repeats a manual the author
     can read.
  2. No provenance. If a comment describes a historical event that led to the
     current state, delete the comment. A historical note records the author's
     reasoning about the code.
  3. No "what" comments. The reader of a comment knows the language, the
     framework, and the neighbouring files. If that reader understands the code
     without the comment, delete the comment.
  4. Strictly why, and only what no type or test can assert. If a comment states
     a contract, put the contract in a type. If a comment states a constraint,
     put the constraint in a test or an assertion. Keep a comment only for a
     reason no type can hold. No test can assert that reason. Write a reason
     that names a trap this codebase hit, or a measurement about the world
     outside the code.
  5. The prose rules apply. Apply the prose rules in the next bullet to a
     comment that survives checks 1 to 4.

  If you cannot name the decision the comment protects, delete the comment. A
  type or a test records most such decisions.

- **Prose rules apply everywhere.** Apply these rules to every sentence in a
  comment, a doc, a prompt, and a commit body. The section "Simplified
  Technical English rules" cites the source for each rule.

  - Write all prose in Simplified Technical English.
  - State one fact in at most 20 words.
  - Use the active voice, with an explicit subject and verb.
  - Use one verb for each action.
  - Use a concrete noun instead of an abstract noun.
  - Name the referent instead of a pronoun. If the reader must reopen a file or
    a diff to resolve a pronoun, rewrite the sentence.
  - Use a fragment only in a table cell, a bullet label, or a heading. Do not
    state a cause in a fragment alone.
  - Write in this order: conclusion, evidence, then the result.
  - Explain the mechanism, not only the conclusion. Name the cause, the
    constraint, and the observable result.
  - Name the exact file, symbol, API, state field, edge case, and the check that
    proves the claim.
  - Write the mechanism, not the idiom.
  - Delete ceremony, hedging, filler, and marketing. Do not delete the subject,
    the word "because", or a clause that limits a claim.
  - Apply checks 1 to 3 to docs and prompts, not to comments alone.

- **Name the operation, and name the performer.** Put the entity that performs
  the action in the subject. If the subject cannot perform the verb, the
  sentence names a container, not an actor. Move the actor into the subject, or
  say where the content lives. `The provider row carries a checkbox` fails,
  because a row shows nothing. The Settings UI shows the checkbox.

- **Gates are not optional.** Run `cargo test` and
  `cargo clippy --all-targets -- -D warnings` for every backend change. Run
  `npm run lint` for every frontend change. That script runs eslint and
  prettier. Follow Conventional Commits in every commit. `.githooks/commit-msg`
  checks the message format. Write in the commit body what is true after the
  change. Do not narrate how you found the problem in the commit body.
- **Proof is what the running app shows.** Do not report a change as done until
  the running app shows the effect. Accept a screenshot, a process state, or a
  log line as proof. A clean build is not proof. A passing mock is not proof.

## Simplified Technical English rules

ASD-STE100 Issue 9 Part 1 states these rules:
`https://www.asd-ste100.org/assets/files/ASD-STE100_ISSUE9.pdf`. The list cites
the rules that decided wording in this repo. No tool checks these rules. A
reviewer reads the prose.

- Rule 3.6 Active voice. The subject must be the noun that does the action.
  `Check 1 deletes the comment` fails, because a check deletes nothing.
- Rule 3.6 Unknown agent. When the reader does the action, use `you`. Do not
  invent `a developer` as the subject.
- Rule 3.6 Passive voice. Use the active voice. When the agent is unknown,
  passive is legal in description.
- Rule 3.7 Verb, not noun. Use an approved verb for an action. Prefer `delete`
  over `removal`.
- Rule 5.1 Sentence length in an instruction. Use at most 20 words.
- Rule 5.2 One instruction per sentence, unless two actions happen together.
- Rule 5.3 Imperative mood. Write an instruction in the command form. A
  declarative restatement leaves the reader unsure who must act.
- Rule 5.3 The verb `must`. Do not put `must` before an imperative, except for
  safety. State a prohibition as `Do not …`.
- Rule 5.4 Condition first. Put the condition before the command. A comma
  divides the condition from the command. The wrong order is
  `Apply the primer when the surface is dry.` The right order is
  `When the surface is dry, apply the primer.`
- Rule 5.5 Notes give information, not instructions. A reason sentence stays
  declarative.
- Rule 6.1 One subject per descriptive sentence. Give information gradually.
- Section 6 Descriptive writing. The imperative is not permitted there. Layout
  and Commands stay declarative.
- Rule 6.3 Sentence length in description. The limit is 25 words. This repo
  keeps the 20-word cap for all prose.
- Rule 9.4 One wording per thing. When the context is the same, reuse one
  sentence style. The prose rules have one name in this file.
- GR-3 Pronouns. When another noun is near, replace the pronoun with that
  noun. The five checks repeat `the comment` for that reason.
- GR-4 The pronoun `this`. Make sure the reader knows which item `this` names.
- Colon and list length. Each item after a colon counts as a new sentence. The
  limit before the colon is 20 words in an instruction, and 25 words in
  description.
- Part 2 Controlled dictionary. Part 1 states the rules. Part 2 states the
  approved words. Rule 1.8 accepts technical nouns from the subject field.

## Layout

The README section "Layout" shows where code belongs. In short:
`src-tauri/src/` contains feature modules. `src/` contains the frontend.
`prompts/` contains runtime content. `docs/` contains design documentation.

Consult `docs/ROADMAP.md` before feature work. When priorities move, update
`docs/ROADMAP.md`.

## Commands (Windows host; repo at C:\repos\firstmate)

- Dev loop: `npm run tauri dev`. Vite serves the app on port 1420. Run that
  command in the background. The command blocks until the app exits, so a
  foreground run holds the shell.
- Before the dev loop, stop every Vite that holds port 1420. An orphan Vite makes
  `tauri dev` exit with `Port 1420 is already in use`.
- Verify the running app through `winapp ui`. Do not write a driver script for a
  UI job. The CLI is the surface.
- Stop every `firstmate.exe` you started before you finish. An orphan whose dev
  parent died stopped answering `Super+Alt+Space`, while its windows still
  answered commands. A second instance cannot register that hotkey, so it exits
  at startup.
- Gates: run `cargo test` and clippy from `src-tauri/`. Run `npm run lint` for
  the frontend. `npm run format` fixes what prettier reports.
- Full frontend build: `npm run build`, which runs lint, then `tsc`, then
  `vite build`.
