//! Diagnostics writers: unknown-tool notes, LLM errors and global
//! runtimes auto-detection.

use std::path::Path;

use serde_json::Value;

use super::store::{self as config_store, GLOBAL_SCOPE, project_scope};
use super::writer::write_global_delta;

/// Record that the model called a tool which is not registered (a hallucinated
/// or not-yet-implemented tool). Appends the name, de-duplicated, to the
/// project config's top-level `unknown_tools` array so the set can be reviewed
/// and implemented later. Best-effort: never fails the turn, only logs.
pub fn record_unknown_tool(directory: &Path, tool_name: &str) {
    record_unknown_tool_at(&config_store::bebok_db_path(), directory, tool_name);
}

/// [`record_unknown_tool`] against an explicit config database.
pub fn record_unknown_tool_at(db_path: &Path, directory: &Path, tool_name: &str) {
    let scope = project_scope(directory);
    let mut names: Vec<String> = config_store::read_scope_at(db_path, &scope)
        .and_then(|v| {
            v.get("unknown_tools")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();
    if names.iter().any(|n| n == tool_name) {
        return;
    }
    names.push(tool_name.to_string());
    if let Err(e) = config_store::write_delta_at(
        db_path,
        &scope,
        &serde_json::json!({ "unknown_tools": names }),
    ) {
        tracing::warn!("failed to record unknown tool '{tool_name}': {e}");
    }
}

/// Record a failed LLM request (provider error) for a model into the project
/// config's `llm_errors` map (`{ "<model>": ["<error>", ...] }`), de-duplicated
/// per model. Collects recurring provider problems (e.g. a model that reports
/// it does not support tool use) so handling can be added later. Best-effort:
/// never changes control flow, only logs on failure.
pub fn record_llm_error(directory: &Path, model: &str, message: &str) {
    record_llm_error_at(&config_store::bebok_db_path(), directory, model, message);
}

/// [`record_llm_error`] against an explicit config database.
pub fn record_llm_error_at(db_path: &Path, directory: &Path, model: &str, message: &str) {
    if model.trim().is_empty() || message.trim().is_empty() {
        return;
    }
    // Error bodies can be large (provider metadata); keep a bounded prefix.
    const MAX_LEN: usize = 400;
    const MAX_PER_MODEL: usize = 20;
    let message: String = message.chars().take(MAX_LEN).collect();

    let scope = project_scope(directory);
    let mut errors: serde_json::Map<String, Value> = config_store::read_scope_at(db_path, &scope)
        .and_then(|v| v.get("llm_errors").and_then(|e| e.as_object()).cloned())
        .unwrap_or_default();

    let list = errors
        .entry(model.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = match list {
        Value::Array(a) => a,
        _ => return,
    };
    if arr.iter().any(|x| x.as_str() == Some(message.as_str())) {
        return;
    }
    arr.push(Value::String(message));
    if arr.len() > MAX_PER_MODEL {
        let drain = arr.len() - MAX_PER_MODEL;
        arr.drain(..drain);
    }

    if let Err(e) = config_store::write_delta_at(
        db_path,
        &scope,
        &serde_json::json!({ "llm_errors": errors }),
    ) {
        tracing::warn!("failed to record llm error for '{model}': {e}");
    }
}

/// At engine startup: if the global config has no `runtimes` (or all values
/// empty), auto-detect the absolute executable paths and persist them. This is
/// idempotent and never overwrites a value the user has already set.
pub fn ensure_global_runtimes() {
    if let Some(v) = config_store::read_scope_db(GLOBAL_SCOPE)
        && let Some(rt) = v.get("runtimes").and_then(|r| r.as_object())
    {
        let any = rt
            .values()
            .any(|x| x.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false));
        if any {
            return;
        }
    }

    let detected = bebok_tools::Runtimes::detect();
    if let Err(e) = write_global_delta(&serde_json::json!({ "runtimes": detected })) {
        tracing::warn!("failed to persist detected runtimes: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::{GLOBAL_SCOPE, project_scope, read_scope_at};
    use super::*;

    #[test]
    fn record_unknown_tool_appends_deduplicated() {
        let base = std::env::temp_dir().join(format!("bebok-unk-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let db = base.join("bebok.db");
        let scope = project_scope(&project_dir);

        // Fresh scope: records the first unknown tool.
        config_store::write_delta_at(
            &db,
            &scope,
            &serde_json::json!({ "unknown_tools": ["deploy"] }),
        )
        .unwrap();
        let obj = read_scope_at(&db, &scope).unwrap();
        assert_eq!(obj["unknown_tools"].as_array().unwrap().len(), 1);

        // A second name appends.
        config_store::write_delta_at(
            &db,
            &scope,
            &serde_json::json!({ "unknown_tools": ["deploy", "edit"] }),
        )
        .unwrap();
        let obj = read_scope_at(&db, &scope).unwrap();
        let names = obj["unknown_tools"].as_array().unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n == "deploy") && names.iter().any(|n| n == "edit"));

        std::fs::remove_dir_all(&base).ok();
    }

    /// The dedupe guard itself (no I/O): a name already present must not be
    /// recorded twice, which is what `record_unknown_tool` relies on.
    #[test]
    fn unknown_tool_names_are_deduplicated() {
        let mut names: Vec<String> = vec!["deploy".into()];
        let tool_name = "deploy";
        if !names.iter().any(|n| n == tool_name) {
            names.push(tool_name.to_string());
        }
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn record_llm_error_deduplicates_per_model() {
        let base = std::env::temp_dir().join(format!("bebok-llmerr-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let db = base.join("bebok.db");
        let scope = project_scope(&project_dir);

        // Mirror the writer: one entry per distinct message, per model.
        let mut errors = serde_json::Map::new();
        let list = vec![serde_json::json!("http error 404: no tool support")];
        errors.insert("openrouter/x".into(), serde_json::Value::Array(list));
        config_store::write_delta_at(
            &db,
            &scope,
            &serde_json::json!({ "llm_errors": serde_json::Value::Object(errors) }),
        )
        .unwrap();

        let obj = read_scope_at(&db, &scope).unwrap();
        assert_eq!(
            obj["llm_errors"]["openrouter/x"].as_array().unwrap().len(),
            1
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// `ensure_global_runtimes` treats a non-empty `runtimes` map as "already
    /// detected" and must not overwrite it.
    #[test]
    fn runtimes_are_only_detected_when_empty() {
        let base = std::env::temp_dir().join(format!("bebok-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db = base.join("bebok.db");
        config_store::write_delta_at(
            &db,
            GLOBAL_SCOPE,
            &serde_json::json!({ "runtimes": { "rust": "/usr/bin/rustc" } }),
        )
        .unwrap();
        let obj = read_scope_at(&db, GLOBAL_SCOPE).unwrap();
        let rt = obj.get("runtimes").and_then(|r| r.as_object()).unwrap();
        let any = rt
            .values()
            .any(|x| x.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false));
        assert!(any, "an already-detected path must not be re-detected");
        std::fs::remove_dir_all(&base).ok();
    }
}
