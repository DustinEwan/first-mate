
/// The registry in OpenAI function-calling format, not the lmkit format.
pub(crate) fn agent_tools() -> serde_json::Value {
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
        },
        // A skill-gated tool schema must be self-describing: minimal prose
        // that names the skill whose instructions govern the tool. The
        // SKILL.md file stays the real documentation.
        {
            "type": "function",
            "function": {
                "name": "run_elevated",
                "description": "Run a command in the persistent elevated (admin) PowerShell session. Governed by the 'elevation' skill: load_skill(\"elevation\") first.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "timeout_secs": {"type": "integer"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "elevate",
                "description": "Request the user's admin consent (UAC) to open the elevated session. Governed by the 'elevation' skill: load_skill(\"elevation\") first.",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delevate",
                "description": "Release the elevated session, returning to standard rights. Governed by the 'elevation' skill: load_skill(\"elevation\") first.",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    ])
}

pub(crate) fn skill_required(tool: &str) -> Option<&'static str> {
    match tool {
        "read_file" | "edit_file" | "write_file" | "list_dir" | "search_files" | "list_processes" => {
            Some("filesystem")
        }
        "elevate" | "delevate" | "run_elevated" => Some("elevation"),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn tool_schema(name: &str) -> Option<serde_json::Value> {
    agent_tools()
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|t| t.pointer("/function/name").and_then(|n| n.as_str()) == Some(name))
                .cloned()
        })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_tools_name_their_owning_skill() {
        // The gate map must agree with the registry. Every gated tool exists,
        // and the agent's standing capability is never gated.
        for t in [
            "read_file",
            "edit_file",
            "write_file",
            "list_dir",
            "search_files",
            "list_processes",
        ] {
            assert_eq!(skill_required(t), Some("filesystem"), "{t}");
            assert!(tool_schema(t).is_some(), "{t} missing from registry");
        }
        for t in ["elevate", "delevate", "run_elevated"] {
            assert_eq!(skill_required(t), Some("elevation"), "{t}");
            assert!(tool_schema(t).is_some(), "{t} missing from registry");
        }
        for t in [
            "run_command",
            "get_command_output",
            "kill_command",
            "load_skill",
            "read_skill_resource",
        ] {
            assert_eq!(skill_required(t), None, "{t} must stay ungated");
            assert!(tool_schema(t).is_some(), "{t} missing from registry");
        }
    }

    #[test]
    fn full_registry_is_disclosed_every_request() {
        let all = agent_tools().as_array().expect("tool array").clone();
        let names: Vec<&str> = all
            .iter()
            .map(|t| t.pointer("/function/name").and_then(|n| n.as_str()).unwrap())
            .collect();
        for gated in ["read_file", "list_dir", "elevate", "run_elevated"] {
            assert!(names.contains(&gated), "{gated} must be in the static array");
        }
    }
}
