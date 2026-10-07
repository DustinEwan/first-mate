# First Mate — Agent Skills Design

## Overview

First Mate is a Windows control agent. Its primary capability is **winapp** — inspecting and interacting with running Windows applications. This is first-class: always available, always in the system prompt.

First Mate also supports the [Agent Skills](https://agentskills.io/) open specification for extensible, on-demand capabilities.


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

### First-Class (always in system prompt)

- **winapp**: Windows UI automation — the primary purpose of this agent. Its instructions are always available; no loading needed.

### Direct Tools (always in tool list)

- **Filesystem**: `read_file`, `write_file`, `list_dir`, `search_files`
- **System**: `run_command`, `list_processes`

These are simple, frequently used, and don't need progressive disclosure.

### Skills (loaded on demand)

- **Future skills**: Domain-specific workflows, user-added capabilities


## Implementation

### 1. Skill Discovery (Rust)

```rust
// Scan skills/ directory for SKILL.md files
// Parse YAML frontmatter (name, description)
// Return list of available skills
#[tauri::command]
fn list_skills() -> Vec<SkillInfo> { ... }
```

### 2. Load Skill (Rust)

```rust
// Read the full SKILL.md body
#[tauri::command]
fn load_skill(name: String) -> Result<String, String> { ... }
```

### 3. Read Skill Resource (Rust)

```rust
// Read a resource file from a skill directory
#[tauri::command]
fn read_skill_resource(path: String) -> Result<String, String> { ... }
```

### 4. System Prompt Injection (Frontend)

On startup, call `list_skills()`. Inject skill descriptions into the system prompt:

```
You have access to the following skills:
- winapp: Inspect and interact with running Windows applications...
- ...

When a task matches a skill's domain, call load_skill("<name>") to get detailed instructions.
```

### 5. Shell Tool (Rust)

A generic `run_command` tool that executes commands on the Windows system. The agent uses this to run `winapp` commands (as instructed by the skill).

## Future Work

- **User-added skills**: Allow users to add their own skills to `~/.firstmate/skills/`
- **UI tree efficiency**: Truncate/summarize large `winapp ui inspect` output before sending to the LLM
- **Multi-step workflow coordination**: Manage `WINAPP_UI_WORKFLOW_ID` for coordinated UI interactions
- **Skill approval**: Require user approval for certain skill actions (e.g., clicking, typing)
