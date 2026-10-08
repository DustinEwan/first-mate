# First Mate — Agent Skills Design

Every capability First Mate has is delivered as a **skill** and loaded on
demand, following the [Agent Skills](https://agentskills.io/) open
specification. This document covers the skill system only: structure,
discovery, progressive disclosure, and the tool-availability gate. App-level
behavior (window, tray, links, persistence) lives in the README.

## Skill Structure

```
skills/
└── <skill-name>/
    ├── SKILL.md              # Required — YAML frontmatter + markdown instructions
    ├── references/           # Optional — reference docs loaded on demand
    │   └── <file>.md
    └── scripts/              # Optional — executable scripts
        └── <file>.sh
```

### SKILL.md Format

```yaml
---
name: winapp
description: Inspect and interact with running Windows applications via winapp CLI. Use for UI automation, screenshots, clicking, and app control.
tools: # optional — Rust tools this skill enables when loaded; omit for pure-behavior skills
---
```

`tools:` is a comma-separated list of Rust-implemented tools (see
`agent_tools()`) that are injected into the model's context ONLY after this
skill is loaded. A skill without `tools:` is pure behavior. A skill cannot
invent tools: unknown names are skipped.

## Progressive Disclosure

Five stages, minimizing context usage:

| Stage | Tokens | Trigger |
|-------|--------|---------|
| 1. Advertise | ~100/skill | Automatic — injected into system prompt at startup |
| 2. Load | <5,000 | Agent calls `load_skill("<name>")` when task matches |
| 2b. Enable tools | ~150/tool | Same call: the skill's `tools:` schemas join the request, for the rest of the run |
| 3. Read resources | as needed | Agent calls `read_skill_resource("<path>")` |
| 4. Run scripts | as needed | Agent uses shell tool to execute commands |

Before any skill is loaded, the request carries only the core tools
(`CORE_TOOLS`): `run_command`, `get_command_output`, `kill_command`,
`load_skill`, `read_skill_resource`. Tool *implementation* always lives in
Rust; tool *availability* is a skill decision.

### Stage 1: Advertise

At startup, scan `skills/` for `SKILL.md` files. Parse YAML frontmatter. Inject into system prompt:

```
Available skills:
- winapp: Inspect and interact with running Windows applications via winapp CLI. Use for UI automation, screenshots, clicking, and app control.
- filesystem: Read, write, list, and search files on the Windows filesystem.
- ...
```

### Stage 2: Load

When the agent determines a task matches a skill's domain, it calls:

```
load_skill("winapp")
→ Returns the full SKILL.md body (markdown instructions)
```

### Stage 3: Read Resources

For supplementary reference material:

```
read_skill_resource("winapp/references/ui-automation.md")
→ Returns the file contents
```

### Stage 4: Execute

The agent uses the existing **shell tool** to run commands. The skill instructions tell it what to run. No separate `run_skill_script` tool is needed for CLI-based skills.

## Capability Tiers

### Core Tools (always in context)

- **Command**: `run_command`, `get_command_output`, `kill_command`
- **Skills**: `load_skill`, `read_skill_resource`

The interpreter itself: enough to run anything and load anything.

### Skill-Enabled Tools (in context only after their skill loads)

- **filesystem skill** enables: `read_file`, `write_file`, `list_dir`,
  `search_files`, `list_processes`

Implemented in Rust from startup; invisible to the model until the skill's
`tools:` enables them.

### Skills (loaded on demand)

All capability knowledge lives here, including the primary one:

- **winapp**: Windows UI automation — commands, rules, and measured latency
  costs (`skills/winapp/`) — pure behavior, drives `run_command`
- **filesystem**: file/process tool usage and rules (`skills/filesystem/`)
- **git-repo**: repository inspection and safe operations (`skills/git-repo/`)
- **User skills**: drop a folder into `~/.firstmate/skills/`

## Implementation

Implemented in `src-tauri/src/lib.rs` + `src/ChatView.vue`:

### 1. Skill Discovery (Rust)

`skill_roots()` scans, in order: `skills/` (cwd and `../skills`, plus the exe
directory and its parent for the dev layout), then `~/.firstmate/skills`
(user-added skills — implemented). `discover_skills()` parses each
`<skill>/SKILL.md` frontmatter (`name`, `description`); first root wins on
name clashes.

### 2. Advertise (frontend, stage 1)

`#[tauri::command] list_skills()` returns `Vec<SkillInfo>`. On mount,
ChatView.vue appends the name/description list plus load instructions to the
system prompt.

### 3. Load / Read resources (agent tools, stages 2-3)

`load_skill(name)` and `read_skill_resource("<skill>/<rel/path>")` are tools
in the model's tool list (not tauri commands — the model calls them mid-run).
`resolve_skill_resource` refuses traversal and any path escaping the skill
directory (canonicalize + prefix check).

### 4. Execute (stage 4)

The agent uses the `run_command` tool as instructed by the skill body.

### Tool registry and disclosure

`agent_tools()` is the full registry of Rust-implemented tools. Requests
start with `core_tools()` only; `enable_skill_tools()` appends a loaded
skill's `tools:` schemas to the run's active set (both provider loops
rebuild the request's tool list each turn). The tool loop is uncapped; the
user stops a run with the stop button (`stop_chat`, checked between turns
and tool calls).

### Dispatch gate

`tool_is_disclosed()` runs before every execution: a tool whose schema is
not in the run's active set is refused with an instruction to load the
skill that enables it. Implementation without disclosure is not a path to
execution — resumed conversations and hallucinated calls included.

Resumed conversations reset tool availability to core: the persisted tool
ledger (see README) tells the model what was already inspected, but every
capability must be re-discovered through its skill.

### Example

`skills/git-repo/` — SKILL.md + `references/recipes.md`, loaded on demand.

## Future Work

- **Skill approval**: Require user approval for certain skill actions (e.g., clicking, typing)
