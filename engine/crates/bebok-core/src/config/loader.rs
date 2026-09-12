//! Config layer loading: defaults -> global -> project.
//!
//! Providers merge by name; `ui` merges (`customCss` overwrites per layer,
//! `customCssFiles` concatenates de-duplicated).

use std::path::{Path, PathBuf};

use serde_json::Value;

use bebok_llm::{ProviderSpec, Thinking};

use super::jsonc;
use super::model::{FleetConfig, ResolvedConfig, UiConfig};

/// Load and resolve configuration for a project directory.
pub fn load(directory: &Path) -> ResolvedConfig {
    let global = dirs::config_dir().map(|d| d.join("bebok").join("config.json"));
    load_with_global(directory, global.as_deref())
}

/// Resolve configuration with an explicit global config file (testable).
pub fn load_with_global(directory: &Path, global_file: Option<&Path>) -> ResolvedConfig {
    let mut cfg = ResolvedConfig::default();

    if let Some(global) = global_file {
        apply_file(&mut cfg, global, "global");
    }

    let project = directory.join(".bebok").join("config.json");
    apply_file(&mut cfg, &project, "project");

    cfg.provider = provider_from_model(&cfg.model);
    cfg
}

/// Read one JSONC file and apply it as a config layer (ignored when missing).
pub fn apply_file(cfg: &mut ResolvedConfig, path: &Path, layer: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    match jsonc::parse(&text) {
        Ok(v) => apply(cfg, &v),
        Err(e) => tracing::warn!("invalid {layer} config {}: {}", path.display(), e),
    }
}

/// Apply a config layer on top of the current value (project wins over global).
pub fn apply(cfg: &mut ResolvedConfig, v: &Value) {
    if let Some(model) = v.get("model").and_then(|x| x.as_str()) {
        cfg.model = model.to_string();
    }
    if let Some(max) = v.get("max_tokens").and_then(|x| x.as_u64()) {
        cfg.max_tokens = max.min(u32::MAX as u64) as u32;
    }
    if let Some(t) = v.get("thinking").and_then(|x| x.as_str()) {
        cfg.thinking = parse_thinking(t);
    }
    if let Some(key) = v.get("api_key").and_then(|x| x.as_str()) {
        cfg.api_key = Some(key.to_string());
    }
    if let Some(budget) = v.get("context_budget").and_then(|x| x.as_u64()) {
        cfg.context_budget = budget as usize;
    }
    if let Some(cap) = v.get("tool_output_cap").and_then(|x| x.as_u64()) {
        cfg.tool_output_cap = cap as usize;
    }
    if let Some(yolo) = v.get("yolo").and_then(|x| x.as_bool()) {
        cfg.yolo = yolo;
    }
    // Per-agent-type model overrides (project layer wins per key).
    if let Some(models) = v.get("models").and_then(|x| x.as_object()) {
        if let Some(existing) = cfg.models.as_object_mut() {
            for (k, val) in models {
                existing.insert(k.clone(), val.clone());
            }
        } else {
            cfg.models = v.get("models").cloned().unwrap_or_default();
        }
    }
    // Providers are merged by name (global providers survive, project
    // providers override/add), so a project can add its own provider.
    if let Some(providers) = v.get("providers").and_then(|x| x.as_array()) {
        let specs: Vec<ProviderSpec> = providers
            .iter()
            .filter_map(|p| serde_json::from_value(p.clone()).ok())
            .collect();
        merge_providers(&mut cfg.providers, &specs);
    }
    for (key, field) in [
        ("permission", &mut cfg.permission),
        ("mcp", &mut cfg.mcp),
        ("skills", &mut cfg.skills),
        ("terminal", &mut cfg.terminal),
        ("runtimes", &mut cfg.runtimes),
    ] {
        if let Some(section) = v.get(key) {
            *field = section.clone();
        }
    }
    // UI overrides (client-only custom CSS, plain text, never executed here).
    // Layer order: defaults -> global -> project. `customCss` overwrites per
    // layer (project non-empty wins; an explicit empty string clears);
    // `customCssFiles` concatenates de-duplicated (global + project).
    if let Some(ui) = v.get("ui") {
        apply_ui(&mut cfg.ui, ui);
    }
    // Fleet section: project layer fully replaces global when present.
    if let Some(fleet) = v.get("fleet") {
        cfg.fleet = parse_fleet(fleet);
    }
}

fn apply_ui(ui: &mut UiConfig, v: &Value) {
    // Accept both camelCase (`customCss`) and snake_case (`custom_css`) keys.
    let css = v
        .get("customCss")
        .or_else(|| v.get("custom_css"))
        .and_then(|x| x.as_str());
    if let Some(css) = css {
        ui.custom_css = UiConfig::sanitize_css(css);
    }
    let files = v
        .get("customCssFiles")
        .or_else(|| v.get("custom_css_files"))
        .and_then(|x| x.as_array());
    if let Some(files) = files {
        let incoming: Vec<String> = files
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        let sanitized = UiConfig::sanitize_files(&incoming);
        for f in sanitized {
            if !ui.custom_css_files.contains(&f) {
                ui.custom_css_files.push(f);
            }
        }
        // Cap total length (sanitize_files already caps fresh lists).
        ui.custom_css_files
            .truncate(super::model::MAX_CUSTOM_CSS_FILES);
    }
}

/// Parse a `fleet` section. Malformed values are ignored gracefully:
/// `enabled` accepts bool only, `members` accepts an array of objects.
fn parse_fleet(v: &Value) -> FleetConfig {
    let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
    let members = v
        .get("members")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let obj = m.as_object()?;
                    Some(super::model::FleetMember {
                        name: obj
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        agent: obj
                            .get("agent")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        model: obj
                            .get("model")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    FleetConfig { enabled, members }
}

/// Upsert `specs` into `base` by name (later entries win).
pub fn merge_providers(base: &mut Vec<ProviderSpec>, specs: &[ProviderSpec]) {
    for spec in specs {
        match base.iter_mut().find(|b| b.name == spec.name) {
            Some(existing) => {
                existing.kind = spec.kind;
                if spec.endpoint.is_some() {
                    existing.endpoint = spec.endpoint.clone();
                }
                if spec.api_key.is_some() {
                    existing.api_key = spec.api_key.clone();
                }
                if !spec.models.is_empty() {
                    existing.models = spec.models.clone();
                }
                // Provider-specific extras merge key-by-key, so a project layer
                // that sets one field keeps the global layer's other fields.
                for (k, v) in &spec.extra {
                    existing.extra.insert(k.clone(), v.clone());
                }
            }
            None => base.push(spec.clone()),
        }
    }
}

/// Parse a `thinking` config value. Accepts `off`/`no`/`none`, `low`,
/// `mid`/`medium`, `high`, `max`; anything else (and empty) -> `Off`.
pub fn parse_thinking(s: &str) -> Thinking {
    match s.trim().to_ascii_lowercase().as_str() {
        "low" => Thinking::Low,
        "mid" | "medium" => Thinking::Medium,
        "high" => Thinking::High,
        "max" => Thinking::Max,
        _ => Thinking::Off,
    }
}

/// The model prefix selects its provider.
pub fn provider_from_model(model: &str) -> String {
    model.split('/').next().unwrap_or(model).to_string()
}

/// The project config file path for a directory.
pub fn project_config_path(directory: &Path) -> PathBuf {
    directory.join(".bebok").join("config.json")
}

/// The global config file path (`~/.config/bebok/config.json`).
pub fn global_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("bebok").join("config.json"))
        .unwrap_or_else(|| PathBuf::from("bebok-config.json"))
}

/// Read a JSONC layer file, returning the parsed object (or `Null` when the
/// file is missing). Returns `None` only when the file exists but is invalid.
pub fn read_layer_json(path: &Path) -> Option<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => match jsonc::parse(&text) {
            Ok(v) => Some(v),
            Err(_) => None,
        },
        Err(_) => Some(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::DEFAULT_MODEL;
    use super::*;

    #[test]
    fn layered_resolution_defaults_global_project() {
        let base = std::env::temp_dir().join(format!("bebok-cfg-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        // 1. Nothing present -> defaults.
        let cfg = load_with_global(&project_dir, None);
        assert_eq!(cfg.model, DEFAULT_MODEL);

        // 2. Global only.
        std::fs::write(
            &global,
            r#"{ "model": "zai/glm-4.5", "terminal": { "shell": "bash" } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.model, "zai/glm-4.5");
        assert_eq!(cfg.terminal["shell"], "bash");

        // 3. Project overrides global.
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{
                // project model wins
                "model": "zai/glm-4.6",
                "permission": { "edit": "allow" }
            }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(
            cfg.model, "zai/glm-4.6",
            "project config must override global"
        );
        assert_eq!(cfg.permission["edit"], "allow");
        // Global terminal survives (shallow merge per top-level key).
        assert_eq!(cfg.terminal["shell"], "bash");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn parses_thinking_levels() {
        assert_eq!(parse_thinking("off"), bebok_llm::Thinking::Off);
        assert_eq!(parse_thinking("no"), bebok_llm::Thinking::Off);
        assert_eq!(parse_thinking("none"), bebok_llm::Thinking::Off);
        assert_eq!(parse_thinking("low"), bebok_llm::Thinking::Low);
        assert_eq!(parse_thinking("mid"), bebok_llm::Thinking::Medium);
        assert_eq!(parse_thinking("medium"), bebok_llm::Thinking::Medium);
        assert_eq!(parse_thinking("high"), bebok_llm::Thinking::High);
        assert_eq!(parse_thinking("max"), bebok_llm::Thinking::Max);
        assert_eq!(parse_thinking("bogus"), bebok_llm::Thinking::Off);
    }

    #[test]
    fn config_reads_thinking_level() {
        let base = std::env::temp_dir().join(format!("bebok-think-{}", uuid::Uuid::new_v4()));
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "thinking": "high" }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, None);
        assert_eq!(cfg.thinking, bebok_llm::Thinking::High);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn ui_custom_css_merges_global_project() {
        let base = std::env::temp_dir().join(format!("bebok-css-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        std::fs::write(
            &global,
            r#"{ "ui": { "customCss": "body { color: red; }", "customCssFiles": ["a.css"] } }"#,
        )
        .unwrap();
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "ui": { "customCssFiles": ["b.css", "a.css"] } }"#,
        )
        .unwrap();

        let cfg = load_with_global(&project_dir, Some(&global));
        // Global CSS survives when project does not override it.
        assert_eq!(cfg.ui.custom_css, "body { color: red; }");
        // Files concatenate de-duplicated, global first.
        assert_eq!(cfg.ui.custom_css_files, vec!["a.css", "b.css"]);

        // Project non-empty customCss wins.
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "ui": { "customCss": "body { color: blue; }" } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.ui.custom_css, "body { color: blue; }");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn fleet_defaults_disabled() {
        let cfg = ResolvedConfig::default();
        assert!(!cfg.is_fleet_enabled());
        assert!(cfg.fleet_members().is_empty());
    }

    #[test]
    fn fleet_project_replaces_global() {
        let base = std::env::temp_dir().join(format!("bebok-fleet-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        // Global fleet inherited when project has no fleet key.
        std::fs::write(
            &global,
            r#"{ "fleet": { "enabled": true, "members": [{ "name": "a", "agent": "code", "model": "zai/glm-4.5" }] } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert!(cfg.is_fleet_enabled());
        assert_eq!(cfg.fleet_members().len(), 1);
        assert_eq!(cfg.fleet_members()[0].name, "a");

        // Project fleet fully replaces global.
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "fleet": { "enabled": false, "members": [] } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert!(!cfg.is_fleet_enabled());
        assert!(cfg.fleet_members().is_empty());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn fleet_empty_members_and_malformed_tolerance() {
        let mut cfg = ResolvedConfig::default();
        // Empty members OK.
        apply(
            &mut cfg,
            &serde_json::json!({ "fleet": { "enabled": true, "members": [] } }),
        );
        assert!(cfg.is_fleet_enabled());
        assert!(cfg.fleet_members().is_empty());

        // Malformed: enabled non-bool ignored, members non-array ignored,
        // non-object entries skipped.
        apply(
            &mut cfg,
            &serde_json::json!({ "fleet": { "enabled": "yes", "members": "nope" } }),
        );
        assert!(!cfg.is_fleet_enabled());
        assert!(cfg.fleet_members().is_empty());

        apply(
            &mut cfg,
            &serde_json::json!({ "fleet": { "enabled": true, "members": ["bad", 42, { "name": "ok" }] } }),
        );
        assert!(cfg.is_fleet_enabled());
        assert_eq!(cfg.fleet_members().len(), 1);
        assert_eq!(cfg.fleet_members()[0].name, "ok");
        assert_eq!(cfg.fleet_members()[0].agent, "");
    }
}
