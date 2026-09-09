//! Diagnostics writers: unknown-tool notes, LLM errors and global
//! runtimes auto-detection.

use std::path::Path;

use serde_json::Value;

use super::jsonc;
use super::loader::{global_config_path, project_config_path};
use super::writer::{write_global_delta, write_project_delta};

/// Record that the model called a tool which is not registered (a hallucinated
/// or not-yet-implemented tool). Appends the name, de-duplicated, to the
/// project config's top-level `unknown_tools` array so the set can be reviewed
/// and implemented later. Best-effort: never fails the turn, only logs.
pub fn record_unknown_tool(directory: &Path, tool_name: &str) {
    let path = project_config_path(directory);
    let mut names: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| jsonc::parse(&t).ok())
        .and_then(|v| {
            v.get("unknown_tools").and_then(|a| a.as_array()).map(|arr| {
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
    if let Err(e) = write_project_delta(directory, &serde_json::json!({ "unknown_tools": names })) {
        tracing::warn!("failed to record unknown tool '{tool_name}': {e}");
    }
}

/// Record a failed LLM request (provider error) for a model into the project
/// config's `llm_errors` map (`{ "<model>": ["<error>", ...] }`), de-duplicated
/// per model. Collects recurring provider problems (e.g. a model that reports
/// it does not support tool use) so handling can be added later. Best-effort:
/// never changes control flow, only logs on failure.
pub fn record_llm_error(directory: &Path, model: &str, message: &str) {
    if model.trim().is_empty() || message.trim().is_empty() {
        return;
    }
    // Error bodies can be large (provider metadata); keep a bounded prefix.
    const MAX_LEN: usize = 400;
    const MAX_PER_MODEL: usize = 20;
    let message: String = message.chars().take(MAX_LEN).collect();

    let path = project_config_path(directory);
    let mut errors: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| jsonc::parse(&t).ok())
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

    if let Err(e) = write_project_delta(directory, &serde_json::json!({ "llm_errors": errors })) {
        tracing::warn!("failed to record llm error for '{model}': {e}");
    }
}

/// At engine startup: if the global config has no `runtimes` (or all values
/// empty), auto-detect the absolute executable paths and persist them. This is
/// idempotent and never overwrites a value the user has already set.
pub fn ensure_global_runtimes() {
    let path = global_config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = jsonc::parse(&text) {
            if let Some(rt) = v.get("runtimes").and_then(|r| r.as_object()) {
                let any = rt
                    .values()
                    .any(|x| x.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false));
                if any {
                    return;
                }
            }
        }
    }

    let detected = bebok_tools::Runtimes::detect();
    if let Err(e) = write_global_delta(&serde_json::json!({ "runtimes": detected })) {
        tracing::warn!("failed to persist detected runtimes: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::super::jsonc;
    use super::super::loader::project_config_path;
    use super::*;

    #[test]
    fn record_unknown_tool_appends_deduplicated() {
        let base = std::env::temp_dir().join(format!("bebok-unk-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        // Fresh file: creates it and records the first unknown tool.
        record_unknown_tool(&project_dir, "deploy");
        let mut text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        assert!(text.contains("unknown_tools"));
        assert!(text.contains("deploy"));

        // Second unknown tool appends.
        record_unknown_tool(&project_dir, "edit");
        text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        assert!(text.contains("deploy") && text.contains("edit"));
        // Reload sees both, still valid JSONC.
        let v = jsonc::parse(&text).unwrap();
        assert_eq!(v["unknown_tools"].as_array().unwrap().len(), 2);

        // Re-recording the same tool is a no-op.
        record_unknown_tool(&project_dir, "deploy");
        text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        let v = jsonc::parse(&text).unwrap();
        assert_eq!(
            v["unknown_tools"].as_array().unwrap().len(),
            2,
            "duplicate names must be collapsed"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn record_llm_error_deduplicates_per_model() {
        let base = std::env::temp_dir().join(format!("bebok-llmerr-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        record_llm_error(&project_dir, "openrouter/x", "http error 404: no tool support");
        let text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        let v = jsonc::parse(&text).unwrap();
        assert_eq!(v["llm_errors"]["openrouter/x"].as_array().unwrap().len(), 1);

        // Same message for the same model is not stored twice.
        record_llm_error(&project_dir, "openrouter/x", "http error 404: no tool support");
        let text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        let v = jsonc::parse(&text).unwrap();
        assert_eq!(v["llm_errors"]["openrouter/x"].as_array().unwrap().len(), 1);

        // A different error for the same model, and one for another model.
        record_llm_error(&project_dir, "openrouter/x", "http error 500: boom");
        record_llm_error(&project_dir, "openrouter/y", "http error 429: rate limited");
        let text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        let v = jsonc::parse(&text).unwrap();
        assert_eq!(v["llm_errors"]["openrouter/x"].as_array().unwrap().len(), 2);
        assert_eq!(v["llm_errors"]["openrouter/y"].as_array().unwrap().len(), 1);

        std::fs::remove_dir_all(&base).ok();
    }
}
