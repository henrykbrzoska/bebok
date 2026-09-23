//! Config layer loading: defaults -> global -> project.
//!
//! Providers merge by name; `ui` merges (`customCss` overwrites per layer,
//! `customCssFiles` concatenates de-duplicated).

use std::path::{Path, PathBuf};

use serde_json::Value;

use bebok_llm::{ProviderSpec, Thinking};

use super::jsonc;
use super::model::{DelegationConfig, FleetConfig, ResolvedConfig, UiConfig};

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
    // Hub step 4: global allowlist of directories (`allowed_paths`; any layer
    // that carries the key replaces the list, project wins). Malformed values
    // (non-array / non-string entries) are ignored; nothing is enforced yet.
    if let Some(paths) = v.get("allowed_paths").and_then(|x| x.as_array()) {
        cfg.allowed_paths = paths
            .iter()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect();
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
        ("browser", &mut cfg.browser),
        ("verify", &mut cfg.verify),
    ] {
        if let Some(section) = v.get(key) {
            *field = section.clone();
        }
    }
    // F7-7: tool safety categories merge per key (project wins per tool),
    // so a project can re-categorize one tool without repeating the global map.
    if let Some(map) = v.get("tool_safety").and_then(|x| x.as_object()) {
        if let Some(existing) = cfg.tool_safety.as_object_mut() {
            for (k, val) in map {
                existing.insert(k.clone(), val.clone());
            }
        } else {
            cfg.tool_safety = Value::Object(map.clone());
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
    // `delegation` carries `max_concurrent` + watchdog knobs
    // (`max_restarts`, `watchdog_secs`); project overrides global.
    if let Some(d) = v.get("delegation") {
        apply_delegation(&mut cfg.delegation, d);
    }
    // Code map: merged per key (project overrides global per-field and
    // per-path), so a project can flip `enabled` or add one override without
    // repeating the whole global section.
    if let Some(cm) = v.get("code_map") {
        apply_code_map(&mut cfg.code_map, cm);
    }
    // Sampling overrides merge per key (project wins per key): global
    // fields (`{ "temperature": 0.5 }`) and per-agent sections
    // (`{ "code": { "temperature": 0.2 } }`) alike.
    if let Some(map) = v.get("sampling").and_then(|x| x.as_object()) {
        if let Some(existing) = cfg.sampling.as_object_mut() {
            for (k, val) in map {
                match (existing.get_mut(k), val.as_object()) {
                    (Some(Value::Object(old)), Some(obj)) => {
                        for (fk, fv) in obj {
                            old.insert(fk.clone(), fv.clone());
                        }
                    }
                    _ => {
                        existing.insert(k.clone(), val.clone());
                    }
                }
            }
        } else {
            cfg.sampling = Value::Object(map.clone());
        }
    }
}

/// Apply one layer's `code_map` section on top of the current value.
/// Malformed values are ignored key-by-key (a non-bool `enabled` keeps the
/// default); `max_tokens` clamps to `100..=2000`, `max_depth` to `1..=6`;
/// `overrides` merge per path (later layer wins per key).
pub fn apply_code_map(cfg: &mut super::model::CodeMapConfig, v: &Value) {
    if let Some(enabled) = v.get("enabled").and_then(|x| x.as_bool()) {
        cfg.enabled = enabled;
    }
    if let Some(n) = v.get("max_tokens").and_then(|x| x.as_u64()) {
        cfg.max_tokens = (n as usize).clamp(100, 2000);
    }
    if let Some(n) = v.get("max_depth").and_then(|x| x.as_u64()) {
        cfg.max_depth = (n as usize).clamp(1, 6);
    }
    if let Some(map) = v.get("overrides").and_then(|x| x.as_object()) {
        for (k, val) in map {
            if let Some(desc) = val.as_str() {
                cfg.overrides.insert(k.clone(), desc.to_string());
            }
        }
    }
}

/// Apply one layer's `delegation` section on top of the current value.
/// `max_concurrent` (`maxConcurrent`), `max_restarts` (`maxRestarts`) and
/// `watchdog_secs` (`watchdogSecs`) are honoured; the removed legacy keys
/// `mode`, `model_policy`/`modelPolicy` are ignored with a warning.
pub fn apply_delegation(cfg: &mut DelegationConfig, v: &Value) {
    let Some(obj) = v.as_object() else {
        return;
    };
    if obj.contains_key("mode")
        || obj.contains_key("model_policy")
        || obj.contains_key("modelPolicy")
    {
        tracing::warn!(
            "ignoring legacy `delegation.mode`/`delegation.model_policy` keys: sub-agents are now chosen only from the fleet list"
        );
    }
    if let Some(n) = obj
        .get("max_concurrent")
        .or_else(|| obj.get("maxConcurrent"))
        .and_then(|x| x.as_u64())
        && n > 0
    {
        cfg.max_concurrent = (n as usize).min(super::model::MAX_DELEGATION_MAX_CONCURRENT);
    }
    if let Some(n) = obj
        .get("max_restarts")
        .or_else(|| obj.get("maxRestarts"))
        .and_then(|x| x.as_u64())
    {
        cfg.max_restarts = (n as u32).min(10);
    }
    if let Some(n) = obj
        .get("watchdog_secs")
        .or_else(|| obj.get("watchdogSecs"))
        .and_then(|x| x.as_u64())
        && n > 0
    {
        cfg.watchdog_secs = n.clamp(10, 600);
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
        Ok(text) => jsonc::parse(&text).ok(),
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
    fn allowed_paths_defaults_empty_and_layers_global_project() {
        let base = std::env::temp_dir().join(format!("bebok-allow-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();

        // 1. Nothing present -> empty list.
        let cfg = load_with_global(&project_dir, None);
        assert!(cfg.allowed_paths.is_empty(), "default must be empty");

        // 2. Global only: parsed into ResolvedConfig.
        std::fs::write(
            &global,
            r#"{
                // hub: workspace allowlist
                "allowed_paths": ["/home/user/hub", "/home/user/work"]
            }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.allowed_paths, vec!["/home/user/hub", "/home/user/work"]);

        // 3. Project layer replaces the global list when it carries the key.
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "allowed_paths": ["/home/user/other"] }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.allowed_paths, vec!["/home/user/other"]);

        // 4. Project without the key keeps the global list.
        std::fs::write(project_dir.join(".bebok").join("config.json"), r#"{}"#).unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.allowed_paths, vec!["/home/user/hub", "/home/user/work"]);

        // 5. Malformed values are ignored gracefully.
        let mut cfg = ResolvedConfig::default();
        apply(&mut cfg, &serde_json::json!({ "allowed_paths": "nope" }));
        assert!(cfg.allowed_paths.is_empty());
        apply(
            &mut cfg,
            &serde_json::json!({ "allowed_paths": ["ok", 42, null] }),
        );
        assert_eq!(cfg.allowed_paths, vec!["ok"]);
        apply(&mut cfg, &serde_json::json!({ "allowed_paths": [] }));
        assert!(cfg.allowed_paths.is_empty(), "empty array clears the list");

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

    // -- WP-DELEGATION (F8-2) -------------------------------------------------

    #[test]
    fn delegation_defaults_three_and_legacy_ignored() {
        let cfg = ResolvedConfig::default();
        assert_eq!(cfg.delegation.max_concurrent, 3);
        assert_eq!(cfg.delegation.effective_max_concurrent(), 3);
    }

    #[test]
    fn delegation_max_concurrent_layers() {
        let base = std::env::temp_dir().join(format!("bebok-deleg-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();
        std::fs::write(&global, r#"{ "delegation": { "max_concurrent": 5 } }"#).unwrap();
        // No project key: global wins entirely.
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.delegation.max_concurrent, 5);

        // Project max_concurrent overrides the global one.
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "delegation": { "max_concurrent": 7 } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert_eq!(cfg.delegation.max_concurrent, 7);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delegation_legacy_keys_are_ignored() {
        let mut cfg = ResolvedConfig::default();
        // Legacy `mode` / `model_policy` keys are ignored, max_concurrent applies.
        apply(
            &mut cfg,
            &serde_json::json!({ "delegation": { "mode": "off", "model_policy": "cheaper", "max_concurrent": 5 } }),
        );
        assert_eq!(cfg.delegation.max_concurrent, 5);
        // Serialised shape holds only max_concurrent.
        let json = serde_json::to_value(&cfg.delegation).unwrap();
        assert!(json.get("mode").is_none());
        assert!(json.get("model_policy").is_none());
    }

    #[test]
    fn delegation_malformed_values_are_ignored_key_by_key() {
        let mut cfg = ResolvedConfig::default();
        apply(
            &mut cfg,
            &serde_json::json!({ "delegation": { "mode": "sometimes", "max_concurrent": 0 } }),
        );
        assert_eq!(cfg.delegation.max_concurrent, 3);
        // camelCase accepted, huge values clamped.
        apply(
            &mut cfg,
            &serde_json::json!({ "delegation": { "maxConcurrent": 999 } }),
        );
        assert_eq!(
            cfg.delegation.max_concurrent,
            super::super::model::MAX_DELEGATION_MAX_CONCURRENT
        );
        // Non-object section: no-op.
        apply(&mut cfg, &serde_json::json!({ "delegation": "off" }));
        assert_eq!(
            cfg.delegation.max_concurrent,
            super::super::model::MAX_DELEGATION_MAX_CONCURRENT
        );
    }

    // -- code map ------------------------------------------------------------

    #[test]
    fn code_map_defaults_disabled() {
        let cfg = ResolvedConfig::default();
        assert!(!cfg.code_map.enabled);
        assert_eq!(
            cfg.code_map.max_tokens,
            super::super::model::DEFAULT_CODE_MAP_MAX_TOKENS
        );
        assert_eq!(
            cfg.code_map.max_depth,
            super::super::model::DEFAULT_CODE_MAP_MAX_DEPTH
        );
        assert!(cfg.code_map.overrides.is_empty());
    }

    #[test]
    fn code_map_project_overrides_global() {
        let base = std::env::temp_dir().join(format!("bebok-codemap-{}", uuid::Uuid::new_v4()));
        let global = base.join("global.json");
        let project_dir = base.join("project");
        std::fs::create_dir_all(project_dir.join(".bebok")).unwrap();
        std::fs::write(&global, r#"{ "code_map": { "enabled": true } }"#).unwrap();

        // Global on, project silent -> on.
        let cfg = load_with_global(&project_dir, Some(&global));
        assert!(cfg.code_map.enabled);

        // Project flips it off (per-field override).
        std::fs::write(
            project_dir.join(".bebok").join("config.json"),
            r#"{ "code_map": { "enabled": false } }"#,
        )
        .unwrap();
        let cfg = load_with_global(&project_dir, Some(&global));
        assert!(!cfg.code_map.enabled);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn code_map_overrides_merge_per_key() {
        let mut cfg = ResolvedConfig::default();
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "enabled": true, "overrides": { "engine": "Global engine desc" } } }),
        );
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "overrides": { "client": "Client desc", "engine": "Project engine desc" } } }),
        );
        assert!(cfg.code_map.enabled);
        assert_eq!(
            cfg.code_map.overrides.get("engine").map(String::as_str),
            Some("Project engine desc"),
            "project layer wins per path"
        );
        assert_eq!(
            cfg.code_map.overrides.get("client").map(String::as_str),
            Some("Client desc"),
            "global-only paths survive the project layer"
        );
    }

    #[test]
    fn code_map_malformed_values_ignored() {
        let mut cfg = ResolvedConfig::default();
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "enabled": "yes", "max_tokens": "lots", "overrides": { "a": 42 } } }),
        );
        assert!(!cfg.code_map.enabled, "non-bool enabled keeps the default");
        assert_eq!(
            cfg.code_map.max_tokens,
            super::super::model::DEFAULT_CODE_MAP_MAX_TOKENS
        );
        assert!(cfg.code_map.overrides.is_empty());
        // A non-object section is a no-op.
        apply(&mut cfg, &serde_json::json!({ "code_map": "on" }));
        assert!(!cfg.code_map.enabled);
    }

    #[test]
    fn code_map_max_tokens_clamped() {
        let mut cfg = ResolvedConfig::default();
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "max_tokens": 99999 } }),
        );
        assert_eq!(cfg.code_map.max_tokens, 2000);
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "max_tokens": 1 } }),
        );
        assert_eq!(cfg.code_map.max_tokens, 100);
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "max_depth": 0 } }),
        );
        assert_eq!(cfg.code_map.max_depth, 1);
        apply(
            &mut cfg,
            &serde_json::json!({ "code_map": { "max_depth": 99 } }),
        );
        assert_eq!(cfg.code_map.max_depth, 6);
    }
}
