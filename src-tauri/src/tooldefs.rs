use crate::skills::{parse_frontmatter, skill_dir};

/// The tools the agent can call, in OpenAI function-calling format.
fn agent_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Execute a command on the Windows system and return the output. shell='powershell' is REQUIRED for any PowerShell containing $vars, quotes, or multiple statements: it runs in a PERSISTENT session, so $vars, functions and cwd you set carry into later calls, and $, ' and \" survive exactly as typed — never route PowerShell through the default cmd form (cmd.exe re-quoting corrupts it). shell='cmd' (default) runs argv directly, or via cmd.exe when the text contains pipes/&&/redirection. A foreground command still running after 20s is auto-moved to the background and its completion is announced in the conversation later — NEVER poll for it; use background:true for servers/watchers/interactive programs up front.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command to execute (e.g. 'winapp ui inspect -a notepad', or PowerShell statements when shell='powershell'). With shell='powershell' the text IS PowerShell: never wrap it in 'powershell -Command', because the outer session interpolates $vars before the inner one sees them"},
                        "shell": {"type": "string", "enum": ["cmd", "powershell"], "description": "Interpreter. 'powershell' passes command text as script data (zero re-quoting). Default 'cmd'."},
                        "cwd": {"type": "string", "description": "Absolute directory to run the command in (optional)."},
                        "background": {"type": "boolean", "description": "Start detached and return a pid immediately instead of waiting for completion"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_command_output",
                "description": "Get status and accumulated output of a background command (pid from run_command). Usually UNNECESSARY: completion is announced in the conversation automatically. Use only to peek at partial output of a still-running job.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pid": {"type": "integer", "description": "The pid returned by run_command background:true"}
                    },
                    "required": ["pid"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "kill_command",
                "description": "Force-kill a background command and its whole process tree (taskkill /F /T).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pid": {"type": "integer", "description": "The pid to kill"}
                    },
                    "required": ["pid"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a text file with 1-based line numbers. Output starts with a `[path#TAG]` snapshot header; edit_file and write_file require that TAG to prove you saw the current content. Supports offset (start line) and limit (line count, default 2000). Binary files are refused.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The file path to read"},
                        "offset": {"type": "integer", "description": "1-based first line to return"},
                        "limit": {"type": "integer", "description": "Maximum lines to return (default 2000)"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Create a new file, or replace an existing file ENTIRELY. Overwriting an existing file requires 'tag': the TAG from its [path#TAG] header in your latest read_file - an existing file you never read is refused. Prefer edit_file for partial changes.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The file path to write"},
                        "content": {"type": "string", "description": "The full content to write"},
                        "tag": {"type": "string", "description": "TAG from your last read of this file; required only when the file already exists"}
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Line-anchored edits on a file you have read. 'tag' MUST be the TAG from the `[path#TAG]` header of your latest read_file of it; a stale or never-seen tag is refused (re-read, re-anchor, retry). 'ops' is newline-separated instructions using the 1-based line numbers from that read; body lines are prefixed '+' and inserted verbatim (a lone '+' is a blank line). Apply ops bottom-up (highest line number first). Forms: 'PUT N.=M:' replace lines N..M with the body; 'PUT <N:' insert body before line N; 'PUT >N:' insert body after line N; 'CUT N.=M' delete lines N..M. Ops must not overlap; two inserts at one anchor are rejected.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The file path to edit"},
                        "tag": {"type": "string", "description": "TAG from the [path#TAG] header of your last read of this file"},
                        "ops": {"type": "string", "description": "Newline-separated PUT/CUT instructions, bottom-up"}
                    },
                    "required": ["path", "tag", "ops"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List the contents of a directory. Directory entries end with '/'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "The directory path to list"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_files",
                "description": "Recursively find files/directories whose name contains a case-insensitive substring. Returns up to 200 paths. Pick a narrow root; a walk of C:\\ is slow.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Case-insensitive substring of the file name"},
                        "root": {"type": "string", "description": "Directory to search under (default 'C:\\')"}
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_processes",
                "description": "List running processes as CSV: name, PID, session, memory. To filter, use run_command with 'tasklist | findstr /i <name>' instead.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "Load the full instructions of one of the advertised skills. Call this first whenever a task matches a skill's domain.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Skill name, exactly as advertised in the system prompt"}
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_skill_resource",
                "description": "Read a reference file that belongs to a skill, e.g. 'my-skill/references/api.md'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "'<skill>/<relative/path>' inside the skill directory"}
                    },
                    "required": ["path"]
                }
            }
        }
    ])
}

/// The only tool schemas injected into context before any skill is loaded:
/// command execution plus the skill machinery itself. Everything else is
/// implemented here in Rust but stays OUT of the model's context until a
/// skill's frontmatter `tools:` list enables it (see `enable_skill_tools`).
const CORE_TOOLS: &[&str] = &[
    "run_command",
    "get_command_output",
    "kill_command",
    "load_skill",
    "read_skill_resource",
];

pub(crate) fn tool_schema(name: &str) -> Option<serde_json::Value> {
    agent_tools()
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some(name))
                .cloned()
        })
}

pub(crate) fn core_tools() -> Vec<serde_json::Value> {
    CORE_TOOLS.iter().filter_map(|n| tool_schema(n)).collect()
}

/// Append the tool schemas a skill's frontmatter `tools:` enables that are
/// not already in `active`. Returns the names added (empty = pure-behavior
/// skill). Unknown names are skipped: the skill cannot invent tools.
pub(crate) fn enable_skill_tools(active: &mut Vec<serde_json::Value>, skill_name: &str) -> Vec<String> {
    let Some(dir) = skill_dir(skill_name) else {
        return Vec::new();
    };
    let Ok(md) = std::fs::read_to_string(dir.join("SKILL.md")) else {
        return Vec::new();
    };
    let (_, _, tools, _) = parse_frontmatter(&md);
    let mut added = Vec::new();
    for t in tools {
        if active.iter().any(|v| v.pointer("/function/name").and_then(|n| n.as_str()) == Some(t.as_str())) {
            continue;
        }
        if let Some(schema) = tool_schema(&t) {
            active.push(schema);
            added.push(t);
        }
    }
    added
}

/// Convert OpenAI-format tool schemas to lmkit types.
pub(crate) fn to_tool_defs(values: &[serde_json::Value]) -> Vec<lmkit::ToolDefinition> {
    use lmkit::{FunctionDefinition, ToolDefinition};
    values
        .iter()
        .map(|t| {
            let func = t.get("function").expect("tool has a function");
            let name = func
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = func
                .get("parameters")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let function = match func.get("description").and_then(|d| d.as_str()) {
                Some(desc) => FunctionDefinition::with_description(name, desc, parameters),
                None => FunctionDefinition::new(name, parameters),
            };
            ToolDefinition { function }
        })
        .collect()
}

/// Dispatch gate: a tool may execute only if its schema is disclosed in the
/// current run (core or skill-enabled). Resumed conversations and hallucinated
/// calls must not reach undisclosed implementations.
pub(crate) fn tool_is_disclosed(active: &[serde_json::Value], name: &str) -> bool {
    active
        .iter()
        .any(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_tools_hidden_until_filesystem_skill_enables_them() {
        // Tool availability is skill-driven: the core context carries only
        // command+skill primitives; the filesystem skill's frontmatter
        // enables the fs tools; unknown names can't invent tools.
        let core = core_tools();
        let core_names: Vec<&str> = core
            .iter()
            .map(|t| t.pointer("/function/name").and_then(|n| n.as_str()).unwrap())
            .collect();
        assert_eq!(core_names.len(), CORE_TOOLS.len());
        for hidden in ["read_file", "edit_file", "write_file", "list_dir", "search_files", "list_processes"] {
            assert!(!core_names.contains(&hidden), "{hidden} must not be in core");
        }
        let fs_skill = skill_dir("filesystem").expect("filesystem skill dir");
        let md = std::fs::read_to_string(fs_skill.join("SKILL.md")).unwrap();
        let (_, _, declared, _) = parse_frontmatter(&md);
        assert_eq!(declared.len(), 6, "filesystem skill declares 6 tools");
        let mut active = core.clone();
        let added = enable_skill_tools(&mut active, "filesystem");
        assert_eq!(added.len(), 6);
        assert_eq!(active.len(), core.len() + 6);
        // Idempotent: loading twice adds nothing.
        assert!(enable_skill_tools(&mut active, "filesystem").is_empty());
        // Pure-behavior skill enables nothing; unknown skill enables nothing.
        assert!(enable_skill_tools(&mut active, "winapp").is_empty());
        assert!(enable_skill_tools(&mut active, "no-such-skill").is_empty());
    }

    #[test]
    fn undisclosed_tools_are_refused_at_dispatch() {
        // The gate mirrors disclosure: core run cannot execute fs tools;
        // after the skill enables them, the same check passes.
        let core = core_tools();
        assert!(tool_is_disclosed(&core, "run_command"));
        assert!(tool_is_disclosed(&core, "load_skill"));
        for hidden in ["read_file", "write_file", "list_dir", "search_files", "list_processes"] {
            assert!(!tool_is_disclosed(&core, hidden), "{hidden} undisclosed");
        }
        let mut active = core.clone();
        enable_skill_tools(&mut active, "filesystem");
        assert!(tool_is_disclosed(&active, "read_file"));
    }
}
