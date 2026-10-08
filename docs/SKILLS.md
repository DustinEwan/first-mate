# First Mate — Agent Skills Design

First Mate is a Windows control agent. Its base system prompt carries only
behavioral rules; every capability — including **winapp**, its primary UI
automation tool — is delivered as a skill and loaded on demand.

First Mate follows the [Agent Skills](https://agentskills.io/) open
specification for extensible, on-demand capabilities.


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
---

# WinApp UI Automation

## When to use
- When the user asks to interact with a Windows application
- When the user asks to take a screenshot of an app
- When the user asks to click, type, or navigate in a GUI app

## Commands

### Inspect the UI tree
```bash
winapp ui inspect -a <app-name>
```

### Search for elements
```bash
winapp ui search <selector> -a <app-name>
```

### Invoke (activate) an element
```bash
winapp ui invoke <selector> -a <app-name>
```

### Click an element
```bash
winapp ui click <selector> -a <app-name>
```

### Take a screenshot
```bash
winapp ui screenshot -a <app-name>
```

### Send keyboard input
```bash
winapp ui send-keys <keys> -a <app-name>
```

### Set a value
```bash
winapp ui set-value <selector> <value> -a <app-name>
```

## Workflow
1. Start with `screenshot` to see the current state
2. Use `inspect` to find element selectors
3. Use `invoke`/`click`/`set-value` to interact
4. Use `screenshot` again to verify the result
```

## Progressive Disclosure

Four stages, minimizing context usage:

| Stage | Tokens | Trigger |
|-------|--------|---------|
| 1. Advertise | ~100/skill | Automatic — injected into system prompt at startup |
| 2. Load | <5,000 | Agent calls `load_skill("<name>")` when task matches |
| 3. Read resources | as needed | Agent calls `read_skill_resource("<path>")` |
| 4. Run scripts | as needed | Agent uses shell tool to execute commands |

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

### Direct Tools (always in tool list)

- **Filesystem**: `read_file`, `write_file`, `list_dir`, `search_files`
- **System**: `run_command`, `list_processes`
- **Skills**: `load_skill`, `read_skill_resource`

These are simple, generic primitives; they need no progressive disclosure.

### Skills (loaded on demand)

All capability knowledge lives here, including the primary one:

- **winapp**: Windows UI automation — commands, rules, and measured latency
  costs (`skills/winapp/`)
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

### Direct tools

`run_command`, `read_file`, `write_file`, `list_dir`, `search_files`
(capped recursive name search), `list_processes` — all in `agent_tools()`.
The tool loop is uncapped; the user stops a run with the stop button
(`stop_chat`, checked between turns and tool calls).

### Example

`skills/git-repo/` — SKILL.md + `references/recipes.md`, loaded on demand.

## Future Work

- **Multi-step workflow coordination**: Manage `WINAPP_UI_WORKFLOW_ID` for coordinated UI interactions
- **Skill approval**: Require user approval for certain skill actions (e.g., clicking, typing)
