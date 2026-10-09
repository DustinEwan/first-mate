# First Mate and Agent Skills

Every capability First Mate teaches is delivered as a **skill** following
the [Agent Skills](https://agentskills.io/) open specification. This
document is two things: what the spec says, and how First Mate implements
it as a client. App-level behavior (window, tray, persistence) lives in
the README.

## The spec

A skill is a **portable folder of knowledge**, not a capability:

```
skill-name/
├── SKILL.md          # Required: YAML frontmatter + markdown instructions
├── scripts/          # Optional: executable code (run via the agent's own tools)
├── references/       # Optional: docs loaded on demand
└── assets/           # Optional: templates, data
```

- **Frontmatter** allows exactly these top-level keys: `name` (required;
  lowercase, matches the directory), `description` (required; what the
  skill does and when to use it), plus optional `license`,
  `compatibility`, `metadata`, and the experimental `allowed-tools`.
  Unknown keys are a validation error — the reference validator rejects
  them, so a skill carrying client-specific keys is not portable.
- **Progressive disclosure, three stages:** *Discovery* — at startup only
  `name: description` (~100 tokens/skill) is loaded; *Activation* — when
  a task matches, the full `SKILL.md` body (< 5k tokens recommended)
  enters context; *Execution* — the agent follows the instructions with
  **the tools it already has**, running bundled code or reading
  referenced files as needed.
- The spec has **no tool concept**. Tools belong to the agent; skills
  teach their effective and safe use. That is what makes one skill work
  unchanged across every compatible client.
- Skills can be checked with the reference validator
  (`uvx --from skills-ref agentskills validate <dir>`); all shipped and
  third-party skills must pass.

## First Mate as a client

**Discovery.** `skill_roots()` scans, highest precedence first: the dev
repo's `prompts/skills/` (working copy under test), `~/.firstmate/skills/`
(user overrides by name and extends the set), then the packaged baseline
installed next to the exe (`bundle.resources` in `tauri.conf.json` — one
entry per new skill file). Name clashes resolve first-root-wins; that
ordering *is* the override semantics, no merge logic. The system prompt
itself follows the same layering (`AGENTS.md`); the binary ships no
prompt text at all. The frontend advertises each skill as one
`name: description` line.

**Activation.** The model calls `load_skill("<name>")` and receives the
`SKILL.md` body — nothing else. `read_skill_resource("<skill>/<rel>")`
serves reference files; traversal and escapes from the skill directory
are refused.

**Execution.** Instructions run through the agent's standing tools —
`run_command` (persistent PowerShell/cmd sessions, background jobs), the
file tools, and so on. The tool array is **static and complete on every
request**: no mid-run schema changes, so provider prefix caching stays
warm.

**The dispatch gate (client policy, invisible to skills).** Some tools
carry their usage rules inside a skill's instructions — the file tools
belong to `filesystem`, the admin trio to `elevation`. First Mate
declares that mapping in Rust (`skill_required()` in `tooldefs.rs`),
tracks which skills each conversation has loaded, and refuses a gated
tool before its skill is loaded; the refusal names the skill, so the
model self-corrects in one turn. Resumed conversations start unloaded —
capability is re-earned through the skill. This is enforcement of
context, not security: real protection is UAC consent and the trust
tiers (roadmap Phase F). Skill content never names tools, so nothing
here affects portability.
