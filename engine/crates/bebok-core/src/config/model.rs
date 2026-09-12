//! Resolved configuration model + builder.
//!
//! `ResolvedConfig` is the merged view (defaults -> global -> project).
//! Raw `ui` section carries client-only custom CSS (plain text, never
//! executed in the engine).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bebok_llm::ProviderSpec;

pub const DEFAULT_MODEL: &str = "zai/glm-5.3-flash";
pub const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Default context budget in tokens before pruning/compaction kicks in.
pub const DEFAULT_CONTEXT_BUDGET: usize = 64_000;
/// Default cap on a single tool output (bytes, tail preserved).
pub const DEFAULT_TOOL_OUTPUT_CAP: usize = 32 * 1024;
/// Max length for `ui.customCss` (plain text, truncated when longer).
pub const MAX_CUSTOM_CSS_LEN: usize = 200 * 1024;
/// Max number of entries in `ui.customCssFiles`.
pub const MAX_CUSTOM_CSS_FILES: usize = 32;

/// Client-only UI overrides. Both fields are plain text (CSS), stored and
/// served verbatim; the engine never executes them (consumed by the client).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiConfig {
    /// Additional custom CSS (`ui.customCss` / `ui.custom_css`).
    #[serde(
        default,
        rename = "customCss",
        alias = "custom_css",
        skip_serializing_if = "String::is_empty"
    )]
    pub custom_css: String,
    /// Optional extra CSS files (`ui.customCssFiles` / `ui.custom_css_files`).
    #[serde(
        default,
        rename = "customCssFiles",
        alias = "custom_css_files",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub custom_css_files: Vec<String>,
}

impl UiConfig {
    /// Truncate `customCss` to [`MAX_CUSTOM_CSS_LEN`] chars (plain text).
    pub fn sanitize_css(s: &str) -> String {
        if s.len() > MAX_CUSTOM_CSS_LEN {
            // Char-boundary safe truncation.
            let mut end = MAX_CUSTOM_CSS_LEN;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s[..end].to_string()
        } else {
            s.to_string()
        }
    }

    /// Sanitize a file list: keep non-empty trimmed strings, dedup, cap count.
    pub fn sanitize_files(files: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in files {
            let t = f.trim();
            if t.is_empty() {
                continue;
            }
            // Bound each entry (paths, not content).
            let t: String = t.chars().take(1024).collect();
            if !out.contains(&t) {
                out.push(t);
            }
            if out.len() >= MAX_CUSTOM_CSS_FILES {
                break;
            }
        }
        out
    }
}

/// One member of the parallel-agents fleet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetMember {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub model: String,
}

/// Toggleable parallel-agents fleet configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub members: Vec<FleetMember>,
}

/// Fully resolved configuration for one instance.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedConfig {
    pub model: String,
    pub provider: String,
    pub max_tokens: u32,
    /// Reasoning/thinking effort for the model (`off`/`low`/`medium`/`high`/`max`).
    pub thinking: bebok_llm::Thinking,
    /// Provider API key. Set from the `api_key` config field; falls back to
    /// the `ZAI_API_KEY` environment variable when absent.
    #[serde(skip_serializing)]
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
    /// Client-only UI overrides (custom CSS, plain text).
    #[serde(default)]
    pub ui: UiConfig,
    /// Toggleable parallel-agents fleet.
    #[serde(default)]
    pub fleet: FleetConfig,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            provider: "zai".to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            thinking: bebok_llm::Thinking::Off,
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
            ui: UiConfig::default(),
            fleet: FleetConfig::default(),
        }
    }
}

impl ResolvedConfig {
    /// Fluent builder entry point.
    pub fn builder() -> ResolvedConfigBuilder {
        ResolvedConfigBuilder::new()
    }

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

    /// Whether the parallel-agents fleet is enabled. Capability gate only:
    /// fan-out still requires explicit `fleet: true` on the prompt.
    pub fn is_fleet_enabled(&self) -> bool {
        self.fleet.enabled
    }

    /// Fleet members (empty when the fleet is disabled/unconfigured).
    pub fn fleet_members(&self) -> &[FleetMember] {
        &self.fleet.members
    }
}

/// Fluent builder for [`ResolvedConfig`] (layer order still
/// defaults -> global -> project in the loader; the builder only assembles
/// one value programmatically, e.g. in tests).
#[derive(Debug, Clone)]
pub struct ResolvedConfigBuilder {
    inner: ResolvedConfig,
}

impl Default for ResolvedConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolvedConfigBuilder {
    pub fn new() -> Self {
        Self {
            inner: ResolvedConfig::default(),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.inner.model = model.into();
        self
    }

    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.inner.provider = provider.into();
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.inner.max_tokens = max_tokens;
        self
    }

    pub fn thinking(mut self, thinking: bebok_llm::Thinking) -> Self {
        self.inner.thinking = thinking;
        self
    }

    pub fn api_key(mut self, api_key: Option<String>) -> Self {
        self.inner.api_key = api_key;
        self
    }

    pub fn models(mut self, models: Value) -> Self {
        self.inner.models = models;
        self
    }

    pub fn providers(mut self, providers: Vec<ProviderSpec>) -> Self {
        self.inner.providers = providers;
        self
    }

    pub fn context_budget(mut self, budget: usize) -> Self {
        self.inner.context_budget = budget;
        self
    }

    pub fn tool_output_cap(mut self, cap: usize) -> Self {
        self.inner.tool_output_cap = cap;
        self
    }

    pub fn yolo(mut self, yolo: bool) -> Self {
        self.inner.yolo = yolo;
        self
    }

    pub fn fleet(mut self, fleet: FleetConfig) -> Self {
        self.inner.fleet = fleet;
        self
    }

    pub fn permission(mut self, v: Value) -> Self {
        self.inner.permission = v;
        self
    }

    pub fn mcp(mut self, v: Value) -> Self {
        self.inner.mcp = v;
        self
    }

    pub fn skills(mut self, v: Value) -> Self {
        self.inner.skills = v;
        self
    }

    pub fn terminal(mut self, v: Value) -> Self {
        self.inner.terminal = v;
        self
    }

    pub fn runtimes(mut self, v: Value) -> Self {
        self.inner.runtimes = v;
        self
    }

    pub fn ui(mut self, ui: UiConfig) -> Self {
        self.inner.ui = ui;
        self
    }

    pub fn custom_css(mut self, css: impl Into<String>) -> Self {
        self.inner.ui.custom_css = UiConfig::sanitize_css(&css.into());
        self
    }

    pub fn custom_css_files(mut self, files: Vec<String>) -> Self {
        self.inner.ui.custom_css_files = UiConfig::sanitize_files(&files);
        self
    }

    pub fn build(self) -> ResolvedConfig {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_assembles_config() {
        let cfg = ResolvedConfig::builder()
            .model("openai/gpt-4o")
            .provider("openai")
            .custom_css("body { color: red; }")
            .custom_css_files(vec!["./theme.css".to_string()])
            .build();
        assert_eq!(cfg.model_for("code"), "openai/gpt-4o");
        assert_eq!(cfg.ui.custom_css, "body { color: red; }");
        assert_eq!(cfg.ui.custom_css_files, vec!["./theme.css"]);
    }

    #[test]
    fn sanitize_truncates_css() {
        let long = "x".repeat(MAX_CUSTOM_CSS_LEN + 10);
        let out = UiConfig::sanitize_css(&long);
        assert_eq!(out.len(), MAX_CUSTOM_CSS_LEN);
    }
}
