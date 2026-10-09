//! Packaging invariants, checked against the repo rather than the built
//! binary. Every packaged skill must exist with identity frontmatter, and
//! the installer resource list must cover every packaged file.

use std::path::Path;

fn repo_root() -> &'static Path {
    // CARGO_MANIFEST_DIR is src-tauri; prompt content lives one level up.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn frontmatter_value(md: &str, key: &str) -> Option<String> {
    let body = md.strip_prefix("---")?.strip_prefix("\r\n").or_else(|| md.strip_prefix("---\n"))?;
    let end = body.find("\n---")?;
    for line in body[..end].lines() {
        if let Some(v) = line.strip_prefix(key) {
            return Some(v.to_string());
        }
    }
    None
}

#[test]
fn packaged_skills_have_identity_frontmatter() {
    let root = repo_root().join("prompts").join("skills");
    let mut found = Vec::new();
    for skill in [
        "winapp",
        "powershell",
        "filesystem",
        "git-repo",
        "elevation",
    ] {
        let md = std::fs::read_to_string(root.join(skill).join("SKILL.md"))
            .unwrap_or_else(|e| panic!("packaged skill {skill}/SKILL.md: {e}"));
        let name = frontmatter_value(&md, "name:").unwrap_or_default();
        assert!(
            name.trim() == skill,
            "{skill}: frontmatter name is {name:?}, must match the folder"
        );
        assert!(
            frontmatter_value(&md, "description:").is_some(),
            "{skill}: frontmatter needs a description (it is the load trigger)"
        );
        found.push(skill);
    }
    assert_eq!(found.len(), 5);
}

#[test]
fn installer_packages_every_skill_file() {
    let conf = std::fs::read_to_string(repo_root().join("src-tauri").join("tauri.conf.json"))
        .expect("tauri.conf.json");
    for skill in [
        "winapp",
        "powershell",
        "filesystem",
        "git-repo",
        "elevation",
    ] {
        let entry = format!("../prompts/skills/{skill}/SKILL.md");
        assert!(
            conf.contains(&entry),
            "tauri.conf.json resources must package {entry}"
        );
    }
}

#[test]
fn installer_packages_the_prompt_content() {
    // The binary ships no prompt text. Every prompt file the runtime reads
    // must travel with the installer.
    let conf = std::fs::read_to_string(repo_root().join("src-tauri").join("tauri.conf.json"))
        .expect("tauri.conf.json");
    for name in ["AGENTS.md", "COMPACT.md"] {
        let entry = format!("../prompts/{name}");
        assert!(
            conf.contains(&entry),
            "tauri.conf.json resources must package {entry}"
        );
        assert!(
            repo_root().join("prompts").join(name).is_file(),
            "prompts/{name} must exist to be packaged"
        );
    }
}
