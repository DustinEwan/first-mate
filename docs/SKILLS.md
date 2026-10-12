# First Mate and Agent Skills

Every capability First Mate teaches arrives as a **skill**. Each skill follows
the [Agent Skills](https://agentskills.io/) open specification. This document
has two parts. The first part states what the specification says. The second
part states how First Mate implements the specification as a client. App-level
behavior (window, tray, persistence) lives in the README.

## The specification

A skill is a **portable folder of knowledge**, not a capability. A skill folder
has this layout:

```
skill-name/
├── SKILL.md          # Required: YAML frontmatter + markdown instructions
├── scripts/          # Optional: executable code (run via the agent's own tools)
├── references/       # Optional: docs loaded on demand
└── assets/           # Optional: templates, data
```

- **Frontmatter.** The specification allows exactly these top-level keys and no
  others:

  - `name` is required, is lowercase, and matches the directory.
  - `description` is required, and states what the skill does and when to use the
    skill.
  - `license`, `compatibility`, and `metadata` are optional.
  - `allowed-tools` is optional and experimental.
- The reference validator rejects an unknown key, so a skill whose frontmatter
  lists client-specific keys is not portable.
- **Progressive disclosure** has three stages.

  - *Discovery* loads only `name: description` at startup (~100 tokens/skill).
  - *Activation* loads the full `SKILL.md` body when a task matches
    (< 5k tokens recommended).
  - *Execution* means the agent follows the instructions with **the tools the
    agent already has**. The agent runs bundled code or reads referenced files
    as needed.
- The specification has **no tool concept**. Tools belong to the agent. A skill
  teaches the effective and safe use of those tools. That split between
  knowledge and tools is what lets one skill work unchanged across every
  compatible client.
- The reference validator checks a folder with
  `uvx --from skills-ref agentskills validate <dir>`. All shipped skills and all
  third-party skills must pass.

## First Mate as a client

**Discovery.** `skill_roots()` scans three roots, highest precedence first. The
first root is the dev repo's `prompts/skills/`, the working copy under test. The
second root is `~/.firstmate/skills/`. That directory lets the user override a
skill by name and add new skills to the set. The third root is the packaged
baseline installed next to the exe, listed under `bundle.resources` in
`tauri.conf.json`. Each new skill file needs one `bundle.resources` entry. The
first root that has a skill with that name wins the name. The root order *is* the
override semantics. No merge logic exists. The system prompt follows the same
(`AGENTS.md`). The binary contains no prompt text at all. The frontend
advertises each skill as one `name: description` line.

**Activation.** The model calls `load_skill("<name>")`. The tool returns the
`SKILL.md` body and nothing else. `read_skill_resource("<skill>/<rel>")` returns
reference files. That tool refuses traversal and any escape out of the skill
directory.

**Execution.** Instructions run through the agent's standing tools, among them
`run_command` and the file tools. `run_command` keeps one PowerShell session
alive across calls. A cmd command runs in a fresh `cmd.exe`. A command that
outlives `auto_bg_secs` becomes a background job. The tool array is **static and
complete on every request**. The tool schema never changes mid-run, so provider
prefix caching stays warm.

**The dispatch gate (client policy, invisible to skills).** Some tools need
rules that belong in a skill's instructions, not in a tool schema. The
`filesystem` skill states the rules for the file tools. The `elevation` skill
states the rules for
`elevate`, `run_elevated`, and `delevate`. First Mate declares that mapping in
Rust, in `skill_required()` in `tooldefs.rs`. First Mate also tracks which
skills each conversation has loaded. The dispatch gate refuses a gated tool
before the agent loads its skill. The refusal names the missing skill, so the
model self-corrects in one turn. The loaded set is the conversation's own
`load_skill` ledger. That ledger persists across every later turn and across a
resume from disk. A new conversation starts unloaded. The gate enforces context,
not security, because
real protection is UAC consent and the trust tiers (roadmap Phase F). A skill body
names First Mate's tools, so those five bodies serve First Mate. The frontmatter
stays spec-pure, and the validator checks the frontmatter. The gate adds no rule
that a skill must state in its own text.
