//! Persistence of `always allow` rules (project config file).
//!
//! Atomic write (tmp + rename); never clobbers a config that fails to parse.

use std::path::Path;

use serde_json::Value;

use super::rule::{Rule, parse_rules};
use crate::config::jsonc::JsoncDocument;
use crate::error::{CoreError, Result};

/// Read the permission rules from a config file (missing/invalid -> empty).
pub fn read_rules_from(path: &Path) -> Vec<Rule> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match crate::config::jsonc::parse(&text) {
        Ok(v) => parse_rules(v.get("permission").unwrap_or(&Value::Null)),
        Err(e) => {
            tracing::warn!("invalid config {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Persist an upserted rule under `permission.rules` of the project config,
/// preserving every unrelated top-level key, comment and bit of formatting
/// (via `JsoncDocument::with_set`). Atomic write (tmp + rename).
pub fn persist_project_rule(project_config: &Path, rule: &Rule) -> Result<()> {
    let mut rules: Vec<Rule> = Vec::new();
    let existing: Option<JsoncDocument> = match std::fs::read_to_string(project_config) {
        Ok(text) => match JsoncDocument::parse(&text) {
            Ok(doc) => {
                let permission = doc.value().get("permission").unwrap_or(&Value::Null);
                rules = parse_rules(permission);
                Some(doc)
            }
            Err(e) => {
                // Never clobber a config we cannot parse.
                return Err(CoreError::Other(format!(
                    "cannot update invalid project config {}: {e}",
                    project_config.display()
                )));
            }
        },
        Err(_) => None, // file does not exist yet -> create it
    };

    // Upsert: replacing the same pattern keeps first-match ordering sane (a
    // stale `ask`/`deny` for the same pattern must not shadow the `allow`).
    match rules.iter_mut().find(|r| r.pattern == rule.pattern) {
        Some(existing_rule) => existing_rule.action = rule.action,
        None => rules.push(rule.clone()),
    }

    let permission_value = serde_json::json!({ "rules": rules });
    let output = match &existing {
        Some(doc) => doc.with_set("permission", &permission_value),
        None => serde_json::to_string_pretty(&serde_json::json!({
            "permission": permission_value
        }))
        .unwrap_or_default(),
    };

    if let Some(dir) = project_config.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(project_config, &output)?;
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::rule::Action;
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bebok-perm-store-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(".bebok").join("config.json")
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let path = tmp_path("mkdir");
        persist_project_rule(
            &path,
            &Rule { pattern: "bash(pwd)".into(), action: Action::Allow },
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bash(pwd)"));
        // No stray tmp file left behind.
        assert!(
            !path
                .parent()
                .unwrap()
                .read_dir()
                .unwrap()
                .any(|e| e.unwrap().file_name().to_string_lossy().contains(".tmp-"))
        );
    }

    #[test]
    fn refuses_to_clobber_invalid_config() {
        let path = tmp_path("invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not valid json").unwrap();
        let err = persist_project_rule(
            &path,
            &Rule { pattern: "bash(pwd)".into(), action: Action::Allow },
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid project config"));
        // Original bytes untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not valid json");
    }
}
