//! Config writers: delta (JSONC round-trip, comments preserved) + full.
//!
//! All writes are atomic (tmp + rename). Delta writers only touch the
//! top-level keys present in `delta`; full writers replace the whole file.

use std::path::Path;

use serde_json::Value;

use super::jsonc;
use super::loader::{global_config_path, project_config_path};

/// Write a partial config delta to the project config file, preserving all
/// unrelated comments and formatting. Each top-level key in `delta` replaces
/// the same key in the project file (JSONC round-trip via `JsoncDocument`).
/// Atomic write (tmp + rename).
pub fn write_project_delta(directory: &Path, delta: &Value) -> Result<(), String> {
    write_delta_to(&project_config_path(directory), delta)
}

/// Write a partial config delta to the **global** config file
/// (`~/.config/bebok/config.json`), same JSONC round-trip semantics.
pub fn write_global_delta(delta: &Value) -> Result<(), String> {
    write_delta_to(&global_config_path(), delta)
}

/// Overwrite a config file with a full JSON object (pretty + trailing newline,
/// atomic tmp+rename). Unlike the delta writers this replaces the whole file,
/// so keys absent from `value` are deleted.
pub fn write_full_to(path: &Path, value: &Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("config must be a JSON object".to_string());
    }
    let pretty = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, pretty).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Overwrite the project `<dir>/.bebok/config.json` with a full JSON object.
pub fn write_full_project(directory: &Path, value: &Value) -> Result<(), String> {
    write_full_to(&project_config_path(directory), value)
}

/// Overwrite the global `~/.config/bebok/config.json` with a full JSON object.
pub fn write_full_global(value: &Value) -> Result<(), String> {
    write_full_to(&global_config_path(), value)
}

pub fn write_delta_to(path: &Path, delta: &Value) -> Result<(), String> {
    let Some(delta_obj) = delta.as_object() else {
        return Err("config delta must be a JSON object".to_string());
    };

    let mut out = match std::fs::read_to_string(path) {
        Ok(text) => jsonc::JsoncDocument::parse(&text)
            .map_err(|e| format!("invalid config {}: {e}", path.display()))?
            .raw()
            .to_string(),
        Err(_) => "{}".to_string(),
    };

    for (key, value) in delta_obj {
        let cur = jsonc::JsoncDocument::parse(&out).map_err(|e| e.to_string())?;
        out = cur.with_set(key, value);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::loader::load_with_global;
    use super::super::loader::project_config_path;
    use super::*;

    #[test]
    fn resolves_api_key_and_writes_delta() {
        let base = std::env::temp_dir().join(format!("bebok-cfg2-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        // api_key from global config.
        let global = base.join("global.json");
        std::fs::write(&global, r#"{ "api_key": "secret-global" }"#).unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.api_key.as_deref(), Some("secret-global"));

        // write_project_delta creates the project file and preserves comments.
        write_project_delta(
            &project_dir,
            &serde_json::json!({
                "model": "zai/glm-5.3",
                "api_key": "secret-project",
                // json! has no comments; comments are tested below
                "skills": { "commit-helper": false }
            }),
        )
        .unwrap();

        let text = std::fs::read_to_string(project_config_path(&project_dir)).unwrap();
        assert!(text.contains("\"model\""));

        // Project api_key overrides global on reload.
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.api_key.as_deref(), Some("secret-project"));
        assert_eq!(cfg.model, "zai/glm-5.3");
        assert_eq!(cfg.skills["commit-helper"], false);

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn write_delta_preserves_unrelated_comments() {
        let base = std::env::temp_dir().join(format!("bebok-cfg3-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();
        let path = project_config_path(&project_dir);
        std::fs::write(&path, "{\n  // keep me\n  \"model\": \"zai/glm-4.5\"\n}\n").unwrap();

        write_project_delta(&project_dir, &serde_json::json!({ "model": "zai/glm-5.3" })).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep me"), "comment must survive: {text}");
        assert!(text.contains("zai/glm-5.3"));
        assert!(jsonc::parse(&text).is_ok(), "still valid JSONC: {text}");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn write_ui_delta_preserves_comments() {
        let base = std::env::temp_dir().join(format!("bebok-cssw-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();
        let path = project_config_path(&project_dir);
        std::fs::write(&path, "{\n  // keep me\n  \"model\": \"zai/glm-4.5\"\n}\n").unwrap();

        write_project_delta(
            &project_dir,
            &serde_json::json!({ "ui": { "customCss": "body { color: red; }" } }),
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep me"), "comment must survive: {text}");
        assert!(text.contains("customCss"));
        assert!(jsonc::parse(&text).is_ok());

        std::fs::remove_dir_all(&base).ok();
    }
}
