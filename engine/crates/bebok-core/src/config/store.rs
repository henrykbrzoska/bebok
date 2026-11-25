//! Config storage: the SQLite database is the single source of truth.
//!
//! Layers live in `config_kv` as one row per top-level key, per scope:
//! - `global`             — the global layer,
//! - `project:<dir>`      — that project's layer.
//!
//! `config_docs.raw_jsonc` keeps the pretty-printed document of each scope so
//! the GUI's raw JSON editor can round-trip it; `config_history` records every
//! key change.
//!
//! The JSON/JSONC config **files** are only ever touched by
//! [`super::import`] (`POST /config/import-to-db`) and by the fresh-install
//! helpers in [`global_config_path`] / [`project_config_path`]. Nothing in the
//! runtime reads them: there is no file fallback, and an empty database simply
//! resolves to [`ResolvedConfig::default`].

use rusqlite::Connection;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::loader::apply;
use super::model::ResolvedConfig;

/// Path of the config/session database: `$BEBOK_DB`, else
/// `<data dir>/bebok/bebok.db`.
pub fn bebok_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BEBOK_DB") {
        return PathBuf::from(p);
    }
    crate::session::persist::data_root().join("bebok.db")
}

/// DB scope key of the global config layer.
pub const GLOBAL_SCOPE: &str = "global";

/// DB scope key of a project config layer: `project:<directory>`.
///
/// The directory is canonicalized first: on Windows `temp_dir()` (and other
/// callers) may hand out 8.3 short names (`RUNNER~1`) while the instance
/// store works with the canonical root (`runneradmin`). Without this, writes
/// and reads land in different scopes and config toggles silently stop
/// working (ssh/code_graph gates, always-allow rules).
pub fn project_scope(directory: &Path) -> String {
    if let Ok(canon) = std::fs::canonicalize(directory) {
        return format!("project:{}", canon.to_string_lossy());
    }
    format!("project:{}", directory.to_string_lossy())
}

/// Location of the legacy **global config file**.
///
/// Only the config import reads it (one-time, `POST /config/import-to-db`).
/// It is never part of the runtime config path.
pub fn global_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("bebok").join("config.json"))
        .unwrap_or_else(|| PathBuf::from("bebok-config.json"))
}

/// Location of the legacy **project config file** (`<dir>/.bebok/config.json`).
///
/// Only the config import reads it. It is never part of the runtime config
/// path.
pub fn project_config_path(directory: &Path) -> PathBuf {
    directory.join(".bebok").join("config.json")
}

/// Open (creating if needed) the config database at the default path.
pub fn open_config_db() -> Result<Connection, String> {
    crate::db::open_db_at(&bebok_db_path())
}

/// Read every key of one scope as a JSON object, over an open connection.
fn read_scope_conn(conn: &Connection, scope: &str) -> Option<serde_json::Map<String, Value>> {
    let mut stmt = conn
        .prepare("SELECT key, value_json FROM config_kv WHERE scope=?1")
        .ok()?;
    let rows = stmt
        .query_map([scope], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .ok()?;
    let mut obj = serde_json::Map::new();
    for (k, vj) in rows.flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&vj) {
            obj.insert(k, v);
        }
    }
    Some(obj)
}

/// Every key of one scope at `db_path` as a JSON object
/// (`None` when the DB cannot be read).
pub fn read_scope_at(db_path: &Path, scope: &str) -> Option<serde_json::Map<String, Value>> {
    if !db_path.exists() {
        return Some(serde_json::Map::new());
    }
    let conn = crate::db::open_db_at(db_path).ok()?;
    read_scope_conn(&conn, scope)
}

/// [`read_scope_at`] against the default database path.
pub fn read_scope_db(scope: &str) -> Option<serde_json::Map<String, Value>> {
    read_scope_at(&bebok_db_path(), scope)
}

/// Resolve the configuration of `directory` from the database at `db_path`:
/// defaults -> `global` scope -> `project:<directory>` scope.
///
/// A missing database or a scope with no rows contributes nothing, so an
/// empty DB resolves to the defaults. This never touches a config file.
pub fn load_scoped(db_path: &Path, directory: &Path) -> Result<ResolvedConfig, String> {
    let mut cfg = ResolvedConfig::default();
    if db_path.exists() {
        let conn = crate::db::open_db_at(db_path)?;
        let project = project_scope(directory);
        for scope in [GLOBAL_SCOPE, project.as_str()] {
            if let Some(obj) = read_scope_conn(&conn, scope)
                && !obj.is_empty()
            {
                apply(&mut cfg, &Value::Object(obj));
            }
        }
    }
    cfg.provider = super::loader::provider_from_model(&cfg.model);
    Ok(cfg)
}

/// [`load_scoped`] against the default database path. A read failure is logged
/// and resolves to the defaults: config must never fail an instance.
pub fn load(directory: &Path) -> ResolvedConfig {
    match load_scoped(&bebok_db_path(), directory) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                "failed to read config for {} from {}: {e}; using defaults",
                directory.display(),
                bebok_db_path().display()
            );
            let mut cfg = ResolvedConfig::default();
            cfg.provider = super::loader::provider_from_model(&cfg.model);
            cfg
        }
    }
}

/// Write a partial config delta (one row per top-level key) into `scope` of the
/// database at `db_path`, recording history and refreshing the scope's raw
/// document. A JSON `null` value deletes the key.
pub fn write_delta_at(db_path: &Path, scope: &str, delta: &Value) -> Result<(), String> {
    let conn = crate::db::open_db_at(db_path)?;
    let obj = delta.as_object().ok_or("delta must be object")?;
    let now = crate::util::now_ms();
    conn.execute("BEGIN IMMEDIATE", [])
        .map_err(|e| e.to_string())?;
    let result: Result<(), String> = (|| {
        for (k, v) in obj {
            let vj = serde_json::to_string(v).map_err(|e| e.to_string())?;
            let old: Option<String> = conn
                .query_row(
                    "SELECT value_json FROM config_kv WHERE scope=?1 AND key=?2",
                    rusqlite::params![scope, k],
                    |r| r.get(0),
                )
                .ok();
            if v.is_null() {
                conn.execute(
                    "DELETE FROM config_kv WHERE scope=?1 AND key=?2",
                    rusqlite::params![scope, k],
                )
                .map_err(|e| e.to_string())?;
            } else {
                conn.execute(
                    "INSERT INTO config_kv(scope,key,value_json,revision,updated_at) VALUES(?1,?2,?3,1,?4)
                     ON CONFLICT(scope,key) DO UPDATE SET value_json=excluded.value_json, revision=revision+1, updated_at=excluded.updated_at",
                    rusqlite::params![scope, k, vj, now],
                )
                .map_err(|e| e.to_string())?;
            }
            conn.execute(
                "INSERT INTO config_history(scope,key,old_value,new_value,ts) VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![scope, k, old, if v.is_null() { None } else { Some(vj) }, now],
            )
            .map_err(|e| e.to_string())?;
            // Keep the raw JSONC round-trip document in sync.
            let raw: Option<String> = conn
                .query_row(
                    "SELECT raw_jsonc FROM config_docs WHERE scope=?1",
                    [scope],
                    |r| r.get(0),
                )
                .ok();
            let raw = raw.unwrap_or_else(|| "{}".to_string());
            let doc = super::jsonc::JsoncDocument::parse(&raw).map_err(|e| e.to_string())?;
            let new_raw = if v.is_null() {
                doc.with_remove(k)
            } else {
                doc.with_set(k, v)
            };
            conn.execute(
                "INSERT INTO config_docs(scope,raw_jsonc,dirty) VALUES(?1,?2,1) ON CONFLICT(scope) DO UPDATE SET raw_jsonc=excluded.raw_jsonc, dirty=1",
                rusqlite::params![scope, new_raw],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute("ROLLBACK", []);
        return result;
    }
    conn.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(())
}

/// [`write_delta_at`] against the default database path.
pub fn write_delta_db(scope: &str, delta: &Value) -> Result<(), String> {
    write_delta_at(&bebok_db_path(), scope, delta)
}

/// Replace a whole scope with `value` (keys absent from it are deleted) in the
/// database at `db_path`.
pub fn write_full_at(db_path: &Path, scope: &str, value: &Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("config must be object".into());
    }
    let conn = crate::db::open_db_at(db_path)?;
    let now = crate::util::now_ms();
    let pretty = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    conn.execute("BEGIN IMMEDIATE", [])
        .map_err(|e| e.to_string())?;
    let result: Result<(), String> = (|| {
        conn.execute("DELETE FROM config_kv WHERE scope=?1", [scope])
            .map_err(|e| e.to_string())?;
        for (k, v) in value.as_object().unwrap() {
            let vj = serde_json::to_string(v).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO config_kv(scope,key,value_json,revision,updated_at) VALUES(?1,?2,?3,1,?4)",
                rusqlite::params![scope, k, vj, now],
            )
            .map_err(|e| e.to_string())?;
        }
        conn.execute(
            "INSERT INTO config_docs(scope,raw_jsonc,dirty) VALUES(?1,?2,1) ON CONFLICT(scope) DO UPDATE SET raw_jsonc=excluded.raw_jsonc, dirty=1",
            rusqlite::params![scope, pretty],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO config_history(scope,key,old_value,new_value,ts) VALUES(?1,?2,NULL,?3,?4)",
            rusqlite::params![scope, "__full__", pretty, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute("ROLLBACK", []);
        return result;
    }
    conn.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(())
}

/// [`write_full_at`] against the default database path.
pub fn write_full_db(scope: &str, value: &Value) -> Result<(), String> {
    write_full_at(&bebok_db_path(), scope, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_db(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("bebok-cfgdb-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        (base, project.join("bebok.db"))
    }

    #[test]
    fn missing_db_resolves_to_defaults() {
        let (base, db) = temp_db("empty");
        let project = base.join("project");
        let cfg = load_scoped(&db, &project).unwrap();
        assert_eq!(cfg.model, super::super::model::DEFAULT_MODEL);
        assert!(!db.exists(), "reading must not create the database");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn global_then_project_scopes_layer() {
        let (base, db) = temp_db("layers");
        let project = base.join("project");
        write_delta_at(&db, GLOBAL_SCOPE, &json!({ "model": "zai/glm-4.5" })).unwrap();
        let cfg = load_scoped(&db, &project).unwrap();
        assert_eq!(cfg.model, "zai/glm-4.5");

        write_delta_at(
            &db,
            &project_scope(&project),
            &json!({ "model": "zai/glm-4.6" }),
        )
        .unwrap();
        let cfg = load_scoped(&db, &project).unwrap();
        assert_eq!(cfg.model, "zai/glm-4.6");
        assert_eq!(cfg.provider, "zai");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delta_keeps_unrelated_keys_and_null_deletes() {
        let (base, db) = temp_db("delta");
        let scope = project_scope(&base);
        write_delta_at(&db, &scope, &json!({ "model": "a/b", "yolo": true })).unwrap();
        write_delta_at(&db, &scope, &json!({ "model": "c/d" })).unwrap();
        let obj = read_scope_at(&db, &scope).unwrap();
        assert_eq!(obj.get("model").unwrap(), "c/d");
        assert_eq!(obj.get("yolo").unwrap(), true, "unrelated key survives");

        write_delta_at(&db, &scope, &json!({ "yolo": null })).unwrap();
        let obj = read_scope_at(&db, &scope).unwrap();
        assert!(
            !obj.contains_key("yolo"),
            "a null delta value removes the key"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn full_write_replaces_the_scope() {
        let (base, db) = temp_db("full");
        let scope = project_scope(&base);
        write_delta_at(&db, &scope, &json!({ "model": "a/b", "yolo": true })).unwrap();
        write_full_at(&db, &scope, &json!({ "model": "x/y" })).unwrap();
        let obj = read_scope_at(&db, &scope).unwrap();
        assert_eq!(obj.get("model").unwrap(), "x/y");
        assert!(!obj.contains_key("yolo"), "absent keys are deleted");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Regression: the config files are dead once the DB exists. A file edited
    /// after the import must never change the resolved config (this was a real
    /// bug: a dev engine started before the import kept reading the file).
    #[test]
    fn file_changed_after_import_is_ignored() {
        let (base, db) = temp_db("regression");
        let project = base.join("project");
        write_delta_at(&db, GLOBAL_SCOPE, &json!({ "model": "zai/from-db" })).unwrap();

        std::fs::create_dir_all(project.join(".bebok")).unwrap();
        std::fs::write(
            project.join(".bebok").join("config.json"),
            r#"{ "model": "zai/from-file" }"#,
        )
        .unwrap();
        let global_file = base.join("global.json");
        std::fs::write(&global_file, r#"{ "model": "zai/from-file" }"#).unwrap();

        let cfg = load_scoped(&db, &project).unwrap();
        assert_eq!(cfg.model, "zai/from-db", "the file must be ignored");
        let _ = std::fs::remove_dir_all(&base);
    }
}
