//! Layered JSONC configuration.
//!
//! Precedence: defaults -> global (`~/.config/bebok/config.json`) ->
//! project (`<project>/.bebok/config.json`) -> request args.
//!
//! `model` + provider selection are consumed here; `permission` rules are
//! parsed by `crate::permission` directly from the same files (it needs the
//! per-layer rules, so it re-reads them instead of using the merged view).
//! `mcp.*`, `skills.*` and `terminal.*` are kept as raw sections for later
//! milestones.

pub mod jsonc;

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use bebok_llm::{ProviderKind, ProviderSpec, Thinking};

pub const DEFAULT_MODEL: &str = "zai/glm-5.3-flash";
pub const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Default context budget in tokens before pruning/compaction kicks in.
pub const DEFAULT_CONTEXT_BUDGET: usize = 64_000;
/// Default cap on a single tool output (bytes, tail preserved).
pub const DEFAULT_TOOL_OUTPUT_CAP: usize = 32 * 1024;

/// Fully resolved configuration for one instance.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedConfig {
    pub model: String,
    pub provider: String,
    pub max_tokens: u32,
    /// Reasoning/thinking effort for the model (`off`/`low`/`medium`/`high`/`max`).
    pub thinking: Thinking,
    /// Provider API key. Set from the `api_key` config field; falls back to
    /// the `ZAI_API_KEY` environment variable when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Per-agent-type model overrides (`{ "code": "openai/gpt-4o", ... }`).
    pub models: Value,
    /// Provider registry (merged with built-ins; see `resolve_providers`).
    pub providers: Vec<ProviderSpec>,
    /// Context budget (tokens) before pruning/compaction.
    pub context_budget: usize,
    /// Per-tool-output truncation cap (bytes).
    pub tool_output_cap: usize,
    /// YOLO mode: auto-allow every tool call without asking (dangerous).
    pub yolo: bool,
    pub permission: Value,
    pub mcp: Value,
    pub skills: Value,
    pub terminal: Value,
    /// Executable paths for language runtimes (python/python3/node/php/docker).
    pub runtimes: Value,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            provider: "zai".to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            thinking: Thinking::Off,
            api_key: None,
            models: Value::Object(serde_json::Map::new()),
            providers: Vec::new(),
            context_budget: DEFAULT_CONTEXT_BUDGET,
            tool_output_cap: DEFAULT_TOOL_OUTPUT_CAP,
            yolo: false,
            permission: Value::Object(serde_json::Map::new()),
            mcp: Value::Object(serde_json::Map::new()),
            skills: Value::Object(serde_json::Map::new()),
            terminal: Value::Object(serde_json::Map::new()),
            runtimes: Value::Object(serde_json::Map::new()),
        }
    }
}

impl ResolvedConfig {
    /// Effective provider registry: built-ins merged with any global/project
    /// `providers` config (config entries override built-ins by name).
    pub fn resolved_providers(&self) -> Vec<ProviderSpec> {
        bebok_llm::resolve_provider_specs(&self.providers)
    }

    /// Look up a provider spec by name in the effective registry.
    pub fn provider_spec(&self, name: &str) -> Option<ProviderSpec> {
        bebok_llm::find_provider_spec(&self.resolved_providers(), name).cloned()
    }

    /// The effective model for an agent type: `models.<type>` override, else the
    /// global `model`.
    pub fn model_for(&self, agent_type: &str) -> String {
        self.models
            .get(agent_type)
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.model.clone())
    }
}

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
fn apply_file(cfg: &mut ResolvedConfig, path: &Path, layer: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    match jsonc::parse(&text) {
        Ok(v) => apply(cfg, &v),
        Err(e) => tracing::warn!("invalid {layer} config {}: {}", path.display(), e),
    }
}

/// Apply a config layer on top of the current value (project wins over global).
fn apply(cfg: &mut ResolvedConfig, v: &Value) {
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
}

/// Upsert `specs` into `base` by name (later entries win).
fn merge_providers(base: &mut Vec<ProviderSpec>, specs: &[ProviderSpec]) {
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

fn write_delta_to(path: &Path, delta: &Value) -> Result<(), String> {
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

/// Persist a provider's known models into the **global** config `providers`
/// list (upsert by name, preserving kind/endpoint/api_key).
pub fn save_provider_models_global(
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(&global_config_path(), name, kind, models)?;
    write_global_delta(&delta)
}

/// Persist a provider's known models into the **project** config `providers`
/// list (upsert by name, preserving kind/endpoint/api_key). This is what the
/// GUI's "check available models" button calls for an instance/directory: the
/// project layer is authoritative, so the fetched models surface in `GET /config`
/// (global-only writes would be masked when a project re-declares the provider).
pub fn save_provider_models(
    directory: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(&project_config_path(directory), name, kind, models)?;
    write_project_delta(directory, &delta)
}

/// Read the `providers` array at `path` and upsert `models` into the matching
/// provider (or append it, preserving kind/endpoint/api_key). Returns a config
/// `{ "providers": [...] }` delta — the caller persists it to disk.
fn upsert_provider_models(
    path: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<serde_json::Value, String> {
    let mut providers: Vec<ProviderSpec> = match std::fs::read_to_string(&path) {
        Ok(text) => match jsonc::parse(&text) {
            Ok(v) => v
                .get("providers")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| serde_json::from_value(x.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };

    match providers.iter_mut().find(|p| p.name == name) {
        Some(existing) => {
            existing.models = models.to_vec();
        }
        None => providers.push(ProviderSpec {
            name: name.to_string(),
            kind,
            endpoint: None,
            api_key: None,
            models: models.to_vec(),
        }),
    }

    Ok(serde_json::json!({ "providers": providers }))
}

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
        assert_eq!(cfg.model, "zai/glm-4.6", "project config must override global");
        assert_eq!(cfg.permission["edit"], "allow");
        // Global terminal survives (shallow merge per top-level key).
        assert_eq!(cfg.terminal["shell"], "bash");

        std::fs::remove_dir_all(&base).ok();
    }

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
