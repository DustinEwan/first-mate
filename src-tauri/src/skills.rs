use crate::log::home_dir;

/// AGENTS.md locations: cwd, parent (src-tauri dev layout), exe dir and its
/// parent, then ~/.firstmate. First hit wins.
fn agents_md_path() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> =
        vec![std::path::PathBuf::from("AGENTS.md"), std::path::PathBuf::from("../AGENTS.md")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("AGENTS.md"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("AGENTS.md"));
            }
        }
    }
    candidates.push(home_dir().join(".firstmate").join("AGENTS.md"));
    candidates.into_iter().find(|p| p.is_file())
}

/// The system prompt is content, not code: it is AGENTS.md, discovered via
/// `agents_md_path`. The binary ships NO prompt text — with no AGENTS.md
/// anywhere, the agent runs on the skill advertisements alone.
#[tauri::command]
pub(crate) fn get_system_prompt() -> String {
    agents_md_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_default()
}

/// Skills advertised to the frontend for system-prompt injection (stage 1
/// of progressive disclosure, docs/SKILLS.md).
#[tauri::command]
pub(crate) fn list_skills() -> Vec<SkillInfo> {
    discover_skills()
}

/// A skill advertised to the model: name + description from SKILL.md
/// frontmatter (Agent Skills spec, see docs/SKILLS.md). Tool availability
/// is not advertised — it is revealed when the skill is loaded.
#[derive(serde::Serialize)]
pub(crate) struct SkillInfo {
    pub(crate) name: String,
    pub(crate) description: String,
}

/// Directories scanned for `<skill>/SKILL.md`: repo `skills/` (cwd, exe dir,
/// and the parent of the exe dir for the src-tauri dev layout) plus the
/// user directory `~/.firstmate/skills`.
fn skill_roots() -> Vec<std::path::PathBuf> {
    let mut candidates =
        vec![std::path::PathBuf::from("skills"), std::path::PathBuf::from("../skills")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("skills"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("skills"));
            }
        }
    }
    candidates.push(home_dir().join(".firstmate").join("skills"));
    let mut roots = Vec::new();
    for c in candidates {
        let c = c.canonicalize().unwrap_or(c);
        if c.is_dir() && !roots.contains(&c) {
            roots.push(c);
        }
    }
    roots
}

/// Parse `---` frontmatter. Returns (name, description, tools, body).
/// `tools:` is a comma-separated list of Rust-implemented tools the skill
/// enables when loaded; skills without it are pure behavior.
pub(crate) fn parse_frontmatter(md: &str) -> (Option<String>, Option<String>, Vec<String>, String) {
    let empty_tools = Vec::new();
    let Some(rest) = md
        .strip_prefix("---")
        .and_then(|r| r.strip_prefix('\n'))
        .or_else(|| md.strip_prefix("---\r\n"))
    else {
        return (None, None, empty_tools, md.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None, empty_tools, md.to_string());
    };
    let mut name = None;
    let mut description = None;
    let mut tools = Vec::new();
    for line in rest[..end].lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("tools:") {
            tools = v
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    (name, description, tools, body)
}

/// All skills found under the roots, first root winning on name clashes.
pub(crate) fn discover_skills() -> Vec<SkillInfo> {
    let mut skills: Vec<SkillInfo> = Vec::new();
    for root in skill_roots() {
        let Ok(dir) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in dir.flatten() {
            let Ok(md) = std::fs::read_to_string(entry.path().join("SKILL.md")) else {
                continue;
            };
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let (name, description, _, _) = parse_frontmatter(&md);
            let name = name.unwrap_or(dir_name);
            if !skills.iter().any(|s| s.name == name) {
                skills.push(SkillInfo {
                    name,
                    description: description.unwrap_or_default(),
                });
            }
        }
    }
    skills
}

/// Directory of the named skill (frontmatter name, falling back to dir name).
pub(crate) fn skill_dir(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    for root in skill_roots() {
        let Ok(dir) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let Ok(md) = std::fs::read_to_string(path.join("SKILL.md")) else {
                continue;
            };
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let (fm_name, ..) = parse_frontmatter(&md);
            if fm_name.as_deref() == Some(name) || dir_name == name {
                return Some(path);
            }
        }
    }
    None
}

/// Resolve `"<skill>/<relative/path>"` to a real file, refusing anything
/// that escapes the skill directory (traversal or symlink tricks).
pub(crate) fn resolve_skill_resource(path: &str) -> Result<std::path::PathBuf, String> {
    if path.contains("..") {
        return Err("path traversal not allowed".into());
    }
    let name = path
        .split(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && path.len() > s.len() + 1)
        .ok_or("expected '<skill>/<relative/path>'")?;
    let dir = skill_dir(name).ok_or_else(|| format!("no such skill: {name}"))?;
    let full = dir
        .join(&path[name.len() + 1..])
        .canonicalize()
        .map_err(|_| format!("not found: {path}"))?;
    let root = dir.canonicalize().map_err(|e| e.to_string())?;
    if !full.starts_with(&root) {
        return Err("path escapes the skill directory".into());
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_splits_metadata_from_body() {
        let md = "---\nname: demo\ndescription: A demo skill.\ntools: read_file, list_dir\n---\n\n# Body\nDo things.\n";
        let (name, desc, tools, body) = parse_frontmatter(md);
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(desc.as_deref(), Some("A demo skill."));
        assert_eq!(tools, vec!["read_file".to_string(), "list_dir".to_string()]);
        assert_eq!(body, "# Body\nDo things.\n");
        // No frontmatter: body passes through untouched.
        let (n2, d2, t2, b2) = parse_frontmatter("plain text");
        assert!(n2.is_none() && d2.is_none());
        assert!(t2.is_empty());
        assert_eq!(b2, "plain text");
    }

    #[test]
    fn repo_skill_is_discoverable_and_loadable() {
        // cargo test runs with cwd = src-tauri, so ../skills is the repo root.
        let skills = discover_skills();
        let git = skills.iter().find(|s| s.name == "git-repo").expect("git-repo skill");
        assert!(git.description.contains("git repositories"));
        let dir = skill_dir("git-repo").expect("skill dir");
        let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        let (.., body) = parse_frontmatter(&md);
        assert!(body.starts_with("# Git Repos"));
        assert!(!body.contains("description:"));
    }

    #[test]
    fn winapp_skill_is_discoverable_with_cost_guidance() {
        // winapp is a skill, not prompt-hardcoded: discovery must find it and
        // the body must carry the measured latency guidance. It is pure
        // behavior: it enables no tools (it drives run_command).
        let skills = discover_skills();
        let win = skills
            .iter()
            .find(|s| s.name == "winapp")
            .expect("winapp skill");
        assert!(win.description.contains("winapp CLI"), "{}", win.description);
        let dir = skill_dir("winapp").expect("winapp skill dir");
        let md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        let (_, _, tools, body) = parse_frontmatter(&md);
        assert!(body.contains("# WinApp UI Automation"));
        assert!(body.contains("measured on this machine"));
        assert!(tools.is_empty(), "winapp must not enable tools");
    }

    #[test]
    fn system_prompt_comes_from_agents_md_not_the_binary() {
        // cargo test runs with cwd = src-tauri: ../AGENTS.md is the repo
        // file. The binary carries no prompt text at all, so anything
        // present here necessarily came from the file.
        let p = get_system_prompt();
        assert!(p.contains("First Mate"));
        assert!(p.contains("file://"), "prompt must come from AGENTS.md");
    }

    #[test]
    fn skill_resource_resolution_is_locked_down() {
        // Traversal and malformed shapes fail before any filesystem access.
        assert!(resolve_skill_resource("git-repo/../secrets.txt").is_err());
        assert!(resolve_skill_resource("git-repo").is_err());
        assert!(resolve_skill_resource("no-such-skill/references/x.md").is_err());
        // A real reference file resolves inside the skill dir.
        let f = resolve_skill_resource("git-repo/references/recipes.md").unwrap();
        assert!(f.is_file());
        assert!(f.starts_with(skill_dir("git-repo").unwrap().canonicalize().unwrap()));
    }
}
