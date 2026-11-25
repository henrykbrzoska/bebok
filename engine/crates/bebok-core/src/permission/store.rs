//! Persistence of `always allow` rules (project config scope).
//!
//! Rules live in the `permission` key of the project's config scope
//! (`project:<dir>`) inside the config database; other top-level keys are
//! untouched because the write is a delta on a single key.

use std::path::Path;

use serde_json::Value;

use super::rule::{Rule, parse_rules};
use crate::config::store::{self, project_scope};
use crate::error::{CoreError, Result};

/// Read the permission rules of one config scope (`global`, `project:<dir>`).
/// A missing scope or a malformed section yields no rules.
pub fn read_rules_at(db_path: &Path, scope: &str) -> Vec<Rule> {
    let Some(obj) = store::read_scope_at(db_path, scope) else {
        return Vec::new();
    };
    let value = obj.get("permission").cloned().unwrap_or(Value::Null);
    parse_rules(&value)
}

/// [`read_rules_at`] against the default database path.
pub fn read_rules_scope(scope: &str) -> Vec<Rule> {
    let Some(obj) = store::read_scope_db(scope) else {
        return Vec::new();
    };
    let value = obj.get("permission").cloned().unwrap_or(Value::Null);
    parse_rules(&value)
}

/// Persist an upserted rule under `permission.rules` of the project scope,
/// preserving every unrelated top-level key (the write is a one-key delta).
pub fn persist_project_rule_at(db_path: &Path, directory: &Path, rule: &Rule) -> Result<()> {
    let scope = project_scope(directory);
    let obj = store::read_scope_at(db_path, &scope).unwrap_or_default();
    let permission = obj.get("permission").cloned().unwrap_or(Value::Null);
    let mut rules: Vec<Rule> = parse_rules(&permission);

    // Upsert: replacing the same pattern keeps first-match ordering sane (a
    // stale `ask`/`deny` for the same pattern must not shadow the `allow`).
    match rules.iter_mut().find(|r| r.pattern == rule.pattern) {
        Some(existing_rule) => existing_rule.action = rule.action,
        None => rules.push(rule.clone()),
    }

    // Keep any non-`rules` keys of the existing `permission` section.
    let mut section = match permission {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    section.insert(
        "rules".to_string(),
        serde_json::to_value(&rules).map_err(|e| CoreError::Other(e.to_string()))?,
    );

    store::write_delta_at(
        db_path,
        &scope,
        &serde_json::json!({ "permission": Value::Object(section) }),
    )
    .map_err(CoreError::Other)
}

/// [`persist_project_rule_at`] against the default database path.
pub fn persist_project_rule(directory: &Path, rule: &Rule) -> Result<()> {
    persist_project_rule_at(&store::bebok_db_path(), directory, rule)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::rule::Action;
    use super::*;

    /// Temp base dir, project dir and the config DB path (never the real one).
    fn tmp(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("bebok-perm-store-{name}-{}", uuid::Uuid::new_v4()));
        let dir = base.join("project");
        std::fs::create_dir_all(&dir).unwrap();
        let db = base.join("bebok.db");
        (base, dir, db)
    }

    fn rule(pattern: &str, action: Action) -> Rule {
        Rule {
            pattern: pattern.into(),
            action,
        }
    }

    #[test]
    fn persist_creates_the_scope_and_round_trips() {
        let (base, dir, db) = tmp("persist");
        // No scope yet: reading yields no rules.
        assert!(read_rules_at(&db, &project_scope(&dir)).is_empty());

        persist_project_rule_at(&db, &dir, &rule("bash(pwd)", Action::Allow)).unwrap();
        let rules = read_rules_at(&db, &project_scope(&dir));
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "bash(pwd)");
        assert_eq!(rules[0].action, Action::Allow);

        // A second, different rule appends.
        persist_project_rule_at(&db, &dir, &rule("bash(git *)", Action::Deny)).unwrap();
        assert_eq!(read_rules_at(&db, &project_scope(&dir)).len(), 2);

        // Re-persisting the same pattern replaces its action in place.
        persist_project_rule_at(&db, &dir, &rule("bash(pwd)", Action::Deny)).unwrap();
        let rules = read_rules_at(&db, &project_scope(&dir));
        assert_eq!(rules.len(), 2, "same pattern must not be duplicated");
        assert_eq!(rules[0].action, Action::Deny);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn persist_keeps_unrelated_keys_and_other_permission_fields() {
        let (base, dir, db) = tmp("keep");
        let scope = project_scope(&dir);
        store::write_delta_at(
            &db,
            &scope,
            &serde_json::json!({
                "model": "zai/glm-5.3",
                "permission": { "edit": "allow", "rules": [] }
            }),
        )
        .unwrap();

        persist_project_rule_at(&db, &dir, &rule("bash(pwd)", Action::Allow)).unwrap();

        let obj = store::read_scope_at(&db, &scope).unwrap();
        assert_eq!(
            obj.get("model").unwrap(),
            "zai/glm-5.3",
            "unrelated keys survive"
        );
        assert_eq!(
            obj["permission"]["edit"], "allow",
            "other permission fields survive"
        );
        let persisted = read_rules_at(&db, &scope);
        assert!(
            persisted.iter().any(|r| r.pattern == "bash(pwd)"),
            "the new rule is persisted: {persisted:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_rules_of_an_unknown_scope_is_empty() {
        let (base, _dir, db) = tmp("unknown");
        store::write_delta_at(
            &db,
            store::GLOBAL_SCOPE,
            &serde_json::json!({ "model": "x/y" }),
        )
        .unwrap();
        assert!(read_rules_at(&db, store::GLOBAL_SCOPE).is_empty());
        assert!(read_rules_at(&db, "project:/nonexistent").is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
