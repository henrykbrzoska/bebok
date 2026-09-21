//! Skills & instructions (SPEC §3.8, milestone M4).
//!
//! Discovery of `AGENTS.md` (global + project) and skill directories
//! (lowest to highest priority):
//! `skills/<name>/SKILL.md` next to the engine binary (or the repo's
//! `skills/` dir in dev), `~/.config/bebok/skill/<name>/SKILL.md` and
//! `<project>/.bebok/skill/<name>/SKILL.md`.
//! Phase 1: the full content of enabled skills is appended to the system
//! prompt. Toggles map to the `skills` config section (`{ "<name>": bool }`).

pub mod frontmatter;

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// Where a skill / instruction file was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Shipped with Bebok (`skills/` next to the engine binary / repo root).
    Bundled,
    Global,
    Project,
}

/// One discovered skill.
#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub source: Source,
    pub path: PathBuf,
    /// Effective enable state (from the `skills` config section; default on).
    pub enabled: bool,
}

/// The full discovery result for one project directory.
#[derive(Debug, Clone, Default)]
pub struct Discovered {
    /// Global `AGENTS.md` content (always included).
    pub agents_global: Option<String>,
    /// Project `AGENTS.md` content (always included).
    pub agents_project: Option<String>,
    /// Skills (bundled + global + project; later layers override earlier
    /// ones by name).
    pub skills: Vec<Skill>,
}

/// Global config root: `<dirs::config_dir()>/bebok`.
pub fn global_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("bebok"))
}

/// Global `AGENTS.md` path.
pub fn global_agents_path() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("AGENTS.md"))
}

/// Global skill directory (`~/.config/bebok/skill`).
pub fn global_skill_dir() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("skill"))
}

/// Project skill directory (`<project>/.bebok/skill`).
pub fn project_skill_dir(project: &Path) -> PathBuf {
    project.join(".bebok").join("skill")
}

/// Bundled skill directory: `skills/` next to the running engine binary.
///
/// Resolution order: `<binary-dir>/skills` (installed layout, e.g. the Tauri
/// sidecar dir), then walking up from the binary dir looking for a `skills/`
/// sibling (dev layout: `engine/target/debug/bebok-server` → repo root
/// `skills/`). Returns the first candidate that exists (missing dir is fine —
/// `discover_skills` skips unreadable directories).
pub fn bundled_skill_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    loop {
        let candidate = dir.join("skills");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Discover `AGENTS.md` + skills for a project directory.
pub fn discover(project: &Path) -> Discovered {
    let agents_global = global_agents_path()
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let agents_project = read_non_empty(&project.join("AGENTS.md"));

    let mut skills: Vec<Skill> = Vec::new();
    if let Some(dir) = bundled_skill_dir() {
        discover_skills(&dir, Source::Bundled, &mut skills);
    }
    if let Some(dir) = global_skill_dir() {
        discover_skills(&dir, Source::Global, &mut skills);
    }
    discover_skills(&project_skill_dir(project), Source::Project, &mut skills);

    // Later layers override earlier ones with the same name.
    let merged = merge_skills(skills);

    Discovered {
        agents_global,
        agents_project,
        skills: merged,
    }
}

/// Merge layered skills by name: later entries override earlier ones.
fn merge_skills(skills: Vec<Skill>) -> Vec<Skill> {
    let mut merged: Vec<Skill> = Vec::new();
    for skill in skills {
        if let Some(existing) = merged.iter_mut().find(|s| s.name == skill.name) {
            *existing = skill;
        } else {
            merged.push(skill);
        }
    }
    merged
}

fn read_non_empty(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Walk a skill directory, reading `<dir>/<name>/SKILL.md`.
fn discover_skills(dir: &Path, source: Source, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let sub = entry.path();
        if !sub.is_dir() {
            continue;
        }
        let skill_file = sub.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&skill_file) else {
            continue;
        };
        let (fm, body) = frontmatter::parse(&text);
        let dir_name = sub
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = fm
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(dir_name);
        let description = fm
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(Skill {
            name,
            description,
            content: body.trim().to_string(),
            source,
            path: skill_file,
            enabled: true,
        });
    }
}

/// Apply the `skills` config section (`{ "<name>": bool }`) to each skill.
/// A missing key keeps the skill enabled; an explicit `false` disables it.
pub fn apply_toggles(discovered: &mut Discovered, skills_config: Option<&Value>) {
    let toggles = skills_config.and_then(Value::as_object);
    for skill in &mut discovered.skills {
        if let Some(enabled) = toggles
            .and_then(|t| t.get(&skill.name))
            .and_then(Value::as_bool)
        {
            skill.enabled = enabled;
        }
    }
}

/// Assemble the instruction block appended to the system prompt (phase 1:
/// full content of `AGENTS.md` + enabled skills).
pub fn assemble_prompt(discovered: &Discovered) -> String {
    let mut out = String::new();

    if let Some(text) = &discovered.agents_global {
        push_section(&mut out, "AGENTS.md (global)", text);
    }
    if let Some(text) = &discovered.agents_project {
        push_section(&mut out, "AGENTS.md (project)", text);
    }
    for skill in &discovered.skills {
        if !skill.enabled {
            continue;
        }
        push_section(&mut out, &format!("Skill: {}", skill.name), &skill.content);
    }

    out
}

fn push_section(out: &mut String, title: &str, content: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("<!-- {title} -->\n{content}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, content: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn discovers_project_skills_and_agents() {
        let base = std::env::temp_dir().join(format!("bebok-skills-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("AGENTS.md"), "project instructions\n").unwrap();
        write_skill(
            &project_skill_dir(&project),
            "commit-helper",
            "---\nname: commit-helper\ndescription: writes commits\n---\nBody of the skill.\n",
        );

        let disc = discover(&project);
        assert_eq!(disc.agents_project.as_deref(), Some("project instructions"));
        // Global skills from the developer's own ~/.config/bebok/skill may
        // also appear (the test env is not hermetic for the global dir), so
        // assert on *this* project's skill, not on the total count.
        let skill = disc
            .skills
            .iter()
            .find(|s| s.name == "commit-helper")
            .expect("project skill discovered");
        assert_eq!(skill.source, Source::Project);
        assert_eq!(skill.description.as_deref(), Some("writes commits"));
        assert!(skill.content.contains("Body of the skill."));
        assert!(skill.enabled);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn toggles_and_assembly() {
        let base = std::env::temp_dir().join(format!("bebok-skills-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        write_skill(&project_skill_dir(&project), "a", "---\n---\nskill A\n");
        write_skill(&project_skill_dir(&project), "b", "---\n---\nskill B\n");

        let mut disc = discover(&project);
        apply_toggles(&mut disc, Some(&serde_json::json!({ "b": false })));

        let a = disc.skills.iter().find(|s| s.name == "a").unwrap();
        let b = disc.skills.iter().find(|s| s.name == "b").unwrap();
        assert!(a.enabled);
        assert!(!b.enabled);

        let prompt = assemble_prompt(&disc);
        assert!(prompt.contains("skill A"));
        assert!(
            !prompt.contains("skill B"),
            "disabled skill must be excluded"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    fn test_skill(name: &str, source: Source, content: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: None,
            content: content.to_string(),
            source,
            path: PathBuf::from(format!("/fake/{name}/SKILL.md")),
            enabled: true,
        }
    }

    #[test]
    fn later_layers_override_by_name() {
        let merged = merge_skills(vec![
            test_skill("x", Source::Bundled, "bundled"),
            test_skill("x", Source::Global, "global"),
            test_skill("x", Source::Project, "project"),
            test_skill("y", Source::Bundled, "bundled y"),
        ]);
        assert_eq!(merged.len(), 2);
        let x = merged.iter().find(|s| s.name == "x").unwrap();
        assert_eq!(x.source, Source::Project);
        assert_eq!(x.content, "project");
        let y = merged.iter().find(|s| s.name == "y").unwrap();
        assert_eq!(y.source, Source::Bundled);
    }

    #[test]
    fn bundled_dir_scan_reads_skill_files() {
        let base = std::env::temp_dir().join(format!("bebok-skills-{}", uuid::Uuid::new_v4()));
        let bundled = base.join("skills");
        write_skill(
            &bundled,
            "demo",
            "---\nname: demo\ndescription: a demo\n---\nDemo body.\n",
        );

        let mut out = Vec::new();
        discover_skills(&bundled, Source::Bundled, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "demo");
        assert_eq!(out[0].source, Source::Bundled);
        assert!(out[0].content.contains("Demo body."));

        let _ = std::fs::remove_dir_all(&base);
    }
}
