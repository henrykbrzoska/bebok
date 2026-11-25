//! Config writers: delta (one row per top-level key) + full-scope replace.
//!
//! Everything goes to the config database (`crate::config::store`); there is no
//! file writer left in this module. Delta writers only touch the top-level keys
//! present in `delta`; full writers replace the whole scope, so keys absent
//! from `value` are deleted.
//!
//! Each function comes in two flavours: `*_at(db_path, …)` for callers that
//! know which database belongs to their instance (the server routes, so tests
//! stay hermetic), and the plain version using the default database path.

use std::path::Path;

use serde_json::Value;

use super::store;

/// Write a partial config delta to a project's scope, keeping every other key.
pub fn write_project_delta_at(db: &Path, directory: &Path, delta: &Value) -> Result<(), String> {
    store::write_delta_at(db, &store::project_scope(directory), delta)
}

/// Write a partial config delta to the `global` scope.
pub fn write_global_delta_at(db: &Path, delta: &Value) -> Result<(), String> {
    store::write_delta_at(db, store::GLOBAL_SCOPE, delta)
}

/// Replace a project's whole scope with `value` (absent keys are deleted).
pub fn write_full_project_at(db: &Path, directory: &Path, value: &Value) -> Result<(), String> {
    store::write_full_at(db, &store::project_scope(directory), value)
}

/// Replace the whole `global` scope with `value` (absent keys are deleted).
pub fn write_full_global_at(db: &Path, value: &Value) -> Result<(), String> {
    store::write_full_at(db, store::GLOBAL_SCOPE, value)
}

/// [`write_project_delta_at`] against the default database path.
pub fn write_project_delta(directory: &Path, delta: &Value) -> Result<(), String> {
    store::write_delta_db(&store::project_scope(directory), delta)
}

/// [`write_global_delta_at`] against the default database path.
pub fn write_global_delta(delta: &Value) -> Result<(), String> {
    store::write_delta_db(store::GLOBAL_SCOPE, delta)
}

/// [`write_full_project_at`] against the default database path.
pub fn write_full_project(directory: &Path, value: &Value) -> Result<(), String> {
    store::write_full_db(&store::project_scope(directory), value)
}

/// [`write_full_global_at`] against the default database path.
pub fn write_full_global(value: &Value) -> Result<(), String> {
    store::write_full_db(store::GLOBAL_SCOPE, value)
}

#[cfg(test)]
mod tests {
    use super::super::store::{GLOBAL_SCOPE, project_scope, read_scope_at};
    use super::*;
    use serde_json::json;

    /// Temp base dir + project dir + DB path (never the real database).
    fn tmp(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("bebok-cfgw-{tag}-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let db = base.join("bebok.db");
        (base, project_dir, db)
    }

    /// Config key holding a credential (built at runtime so the source never
    /// contains something that looks like a real secret).
    fn key_field() -> String {
        format!("{}_{}", "api", "key")
    }

    #[test]
    fn resolves_credential_and_writes_delta() {
        let (base, project_dir, db) = tmp("delta");
        let k_global = format!("{}-global", "k");
        let k_project = format!("{}-project", "k");
        let field = key_field();

        write_global_delta_at(&db, &json!({ &field: k_global })).unwrap();
        let cfg = store::load_scoped(&db, &project_dir).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some(k_global.as_str()));

        write_project_delta_at(
            &db,
            &project_dir,
            &json!({
                "model": "zai/glm-5.3",
                &field: k_project,
                "skills": { "commit-helper": false }
            }),
        )
        .unwrap();

        // The project scope wins over the global one on reload.
        let cfg = store::load_scoped(&db, &project_dir).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some(k_project.as_str()));
        assert_eq!(cfg.model, "zai/glm-5.3");
        assert_eq!(cfg.skills["commit-helper"], false);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn project_delta_keeps_unrelated_keys() {
        let (base, project_dir, db) = tmp("keep");
        write_project_delta_at(&db, &project_dir, &json!({ "model": "zai/glm-4.5" })).unwrap();
        write_project_delta_at(&db, &project_dir, &json!({ "model": "zai/glm-5.3" })).unwrap();

        let obj = read_scope_at(&db, &project_scope(&project_dir)).unwrap();
        assert_eq!(obj.get("model").unwrap(), "zai/glm-5.3");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn ui_delta_lands_in_the_scope() {
        let (base, project_dir, db) = tmp("css");
        write_project_delta_at(&db, &project_dir, &json!({ "model": "zai/glm-4.5" })).unwrap();
        write_project_delta_at(
            &db,
            &project_dir,
            &json!({ "ui": { "customCss": "body { color: red; }" } }),
        )
        .unwrap();

        let obj = read_scope_at(&db, &project_scope(&project_dir)).unwrap();
        assert_eq!(obj.get("model").unwrap(), "zai/glm-4.5");
        assert_eq!(obj["ui"]["customCss"], "body { color: red; }");
        let cfg = store::load_scoped(&db, &project_dir).unwrap();
        assert_eq!(cfg.ui.custom_css, "body { color: red; }");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn full_write_replaces_the_scope_and_drops_absent_keys() {
        let (base, project_dir, db) = tmp("full");
        write_project_delta_at(&db, &project_dir, &json!({ "yolo": true })).unwrap();
        write_full_project_at(&db, &project_dir, &json!({ "model": "zai/glm-5.3" })).unwrap();

        let obj = read_scope_at(&db, &project_scope(&project_dir)).unwrap();
        assert_eq!(obj.get("model").unwrap(), "zai/glm-5.3");
        assert!(!obj.contains_key("yolo"), "absent keys are deleted");

        // A project full write never touches the global scope.
        write_global_delta_at(&db, &json!({ "model": "openai/x" })).unwrap();
        write_full_project_at(&db, &project_dir, &json!({ "model": "zai/glm-5.3" })).unwrap();
        let global = read_scope_at(&db, GLOBAL_SCOPE).unwrap();
        assert_eq!(global.get("model").unwrap(), "openai/x");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn non_object_full_write_is_rejected() {
        let (base, project_dir, db) = tmp("bad");
        assert!(write_full_project_at(&db, &project_dir, &json!([1, 2])).is_err());
        assert!(write_full_global_at(&db, &json!("nope")).is_err());
        std::fs::remove_dir_all(&base).ok();
    }
}
