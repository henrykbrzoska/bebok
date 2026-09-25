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

/// Default cap on the code-map prompt section (tokens).
pub const DEFAULT_CODE_MAP_MAX_TOKENS: usize = 400;
/// Default directory depth scanned for the code map (root children = 1).
pub const DEFAULT_CODE_MAP_MAX_DEPTH: usize = 3;

/// Default max files for the code graph index.
pub const DEFAULT_CODE_GRAPH_MAX_FILES: usize = 5000;

/// Default max files parsed per `code_ast` query.
pub const DEFAULT_AST_SEARCH_MAX_FILES: usize = 2000;

/// Code-graph configuration (`code_graph`).
///
/// When `enabled`, the engine indexes project source files and derives
/// structural summaries for faster retrieval and navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeGraphConfig {
    /// Master toggle: when false, no indexing happens (zero I/O).
    pub enabled: bool,
    /// Glob patterns to exclude from the index (e.g. `**/node_modules/**`).
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Maximum number of files to index.
    pub max_files: usize,
}

impl Default for CodeGraphConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ignore_patterns: Vec::new(),
            max_files: DEFAULT_CODE_GRAPH_MAX_FILES,
        }
    }
}

/// Pre-computed project code map configuration (`code_map`).
///
/// When `enabled`, the engine scans the project tree, derives a one-sentence
/// description per directory and injects the rendered map into the system
/// prompt (see `crate::agent::code_map_prompt`), so the model orients itself
/// without a `list_dir`/`tree`/`glob` walk at the start of every conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeMapConfig {
    /// Master toggle: when false, no map is generated or injected (zero I/O).
    pub enabled: bool,
    /// Max tokens the map section may occupy in the system prompt.
    pub max_tokens: usize,
    /// Max directory depth to scan (root children = 1).
    pub max_depth: usize,
    /// Manual per-path description overrides (key = project-relative path
    /// without trailing slash, value = description).
    #[serde(default)]
    pub overrides: std::collections::HashMap<String, String>,
}

impl Default for CodeMapConfig {
    fn default() -> Self {
        Self {
            // OFF by default — opt-in per project.
            enabled: false,
            max_tokens: DEFAULT_CODE_MAP_MAX_TOKENS,
            max_depth: DEFAULT_CODE_MAP_MAX_DEPTH,
            overrides: std::collections::HashMap::new(),
        }
    }
}

/// `verify.buildTest` policy — whether the agent runs builds/tests autonomously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildTestMode {
    /// Run builds and tests autonomously (default).
    #[default]
    Auto,
    /// Ask the user before running builds/tests.
    Ask,
    /// Never mention or run builds/tests.
    Off,
}

impl BuildTestMode {
    /// Parse the `verify` config section (`{ "buildTest": "auto" | "ask" | "off" }`).
    /// Unknown/missing values resolve to [`BuildTestMode::Auto`].
    pub fn from_config(section: &serde_json::Value) -> Self {
        section
            .get("buildTest")
            .and_then(serde_json::Value::as_str)
            .and_then(Self::parse)
            .unwrap_or_default()
    }

    /// Parse one policy word (case-insensitive, trimmed).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ask" => Some(Self::Ask),
            "off" | "none" | "never" => Some(Self::Off),
            _ => None,
        }
    }

    /// The canonical config word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }
}

/// AST-aware structural search config (`ast_search`).
///
/// When `enabled`, the agent can use the `code_ast` tool to query the AST shape
/// of source files (impl blocks, structs with derives, functions returning a
/// type, annotated items, test functions, etc.). Off by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AstSearchConfig {
    /// Master toggle: when false, the `code_ast` tool is not registered (zero I/O).
    pub enabled: bool,
    /// Max files to parse per query (safety cap).
    pub max_files: usize,
    /// File extensions to include (without dot).
    #[serde(default = "default_ast_languages")]
    pub languages: Vec<String>,
}

fn default_ast_languages() -> Vec<String> {
    vec!["rs".into(), "ts".into(), "tsx".into()]
}

impl Default for AstSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_files: DEFAULT_AST_SEARCH_MAX_FILES,
            languages: default_ast_languages(),
        }
    }
}

/// WP-DELEGATION: concurrency cap for sub-agent fan-out. Sub-agents are only
/// ever spawned from the configured fleet list; this is the only remaining
/// `delegation` knob. Legacy keys (`mode`, `model_policy`/`modelPolicy`) are
/// ignored on load (with a warning).
pub const DEFAULT_DELEGATION_MAX_CONCURRENT: usize = 3;
/// Hard ceiling for `delegation.max_concurrent` (guards against typos).
pub const MAX_DELEGATION_MAX_CONCURRENT: usize = 16;
/// Default cap on watchdog-triggered restarts per child.
pub const DEFAULT_DELEGATION_MAX_RESTARTS: u32 = 2;
/// Default watchdog tick (seconds) for child supervision.
pub const DEFAULT_DELEGATION_WATCHDOG_SECS: u64 = 60;

/// `delegation` config section: concurrency cap + watchdog knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    /// Upper bound on concurrently *running* children; extra ones queue.
    pub max_concurrent: usize,
    /// Max watchdog-triggered restarts per child before the orchestrator
    /// gets an error and takes over itself.
    pub max_restarts: u32,
    /// Watchdog supervision tick in seconds.
    pub watchdog_secs: u64,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_DELEGATION_MAX_CONCURRENT,
            max_restarts: DEFAULT_DELEGATION_MAX_RESTARTS,
            watchdog_secs: DEFAULT_DELEGATION_WATCHDOG_SECS,
        }
    }
}

impl DelegationConfig {
    /// `max_concurrent` clamped to `1..=MAX_DELEGATION_MAX_CONCURRENT`.
    pub fn effective_max_concurrent(&self) -> usize {
        self.max_concurrent.clamp(1, MAX_DELEGATION_MAX_CONCURRENT)
    }
}

/// Parse a `Sampling` from a config JSON value (`{ "temperature": 0.5,
/// "top_p": 0.9, "frequency_penalty": 0.0, "presence_penalty": 0.0,
/// "seed": 42, "top_k": 40 }`). Unknown/mistyped fields are ignored.
fn sampling_from_value(v: &Value) -> bebok_llm::Sampling {
    let get_f = |k: &str| v.get(k).and_then(Value::as_f64).map(|f| f as f32);
    bebok_llm::Sampling {
        temperature: get_f("temperature"),
        top_p: get_f("top_p"),
        frequency_penalty: get_f("frequency_penalty"),
        presence_penalty: get_f("presence_penalty"),
        seed: v.get("seed").and_then(Value::as_i64),
        top_k: v.get("top_k").and_then(Value::as_u64).map(|k| k as u32),
    }
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
    /// Sampling overrides: global fields (`{ "temperature": 0.5, ... }`)
    /// and/or per-agent sections (`{ "code": { "temperature": 0.2 } }`).
    /// Merged per key (global, then project overrides).
    #[serde(default)]
    pub sampling: Value,
    /// Provider registry (merged with built-ins; see `resolve_providers`).
    pub providers: Vec<ProviderSpec>,
    /// Context budget (tokens) before pruning/compaction.
    pub context_budget: usize,
    /// Per-tool-output truncation cap (bytes).
    pub tool_output_cap: usize,
    /// YOLO mode: auto-allow every tool call without asking (dangerous).
    pub yolo: bool,
    /// Global allowlist of absolute directories agents may operate in
    /// (hub workspaces). Parsed + round-tripped only for now — not yet
    /// enforced by the permission engine.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    pub permission: Value,
    pub mcp: Value,
    pub skills: Value,
    pub terminal: Value,
    /// Executable paths for language runtimes (python/python3/node/php/docker).
    pub runtimes: Value,
    /// WP-BROWSER2 (F7-6): `browser.display` = `headed` | `viewer` | `drawer`
    /// (+ optional `windowPosition`), parsed by `bebok_tools::browser::BrowserSettings`.
    pub browser: Value,
    /// WP-AUTOVERIFY (F8-1): `verify.frontend` = `auto` | `ask` | `off`,
    /// parsed by [`super::verify::FrontendVerify`].
    pub verify: Value,
    /// Client-only UI overrides (custom CSS, plain text).
    #[serde(default)]
    pub ui: UiConfig,
    /// Toggleable parallel-agents fleet.
    #[serde(default)]
    pub fleet: FleetConfig,
    /// WP-CHAT4 (F7-7): explicit per-tool safety categories,
    /// `{ "<tool name or glob>": "safe" | "caution" | "dangerous" | "uncategorized" }`.
    /// Merged per key (global, then project overrides). Informational only -
    /// parsed by `crate::tool_safety`, never consulted by the permission engine.
    pub tool_safety: Value,
    /// WP-DELEGATION (F8-2): sub-agent delegation policy + concurrency cap.
    #[serde(default)]
    pub delegation: DelegationConfig,
    /// Pre-computed project code map (off by default; opt-in per project).
    #[serde(default)]
    pub code_map: CodeMapConfig,
    /// Code-graph indexing config (off by default; opt-in per project).
    #[serde(default)]
    pub code_graph: CodeGraphConfig,
    /// AST-aware structural search (off by default; opt-in per project).
    #[serde(default)]
    pub ast_search: AstSearchConfig,
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
            sampling: Value::Object(serde_json::Map::new()),
            providers: Vec::new(),
            context_budget: DEFAULT_CONTEXT_BUDGET,
            tool_output_cap: DEFAULT_TOOL_OUTPUT_CAP,
            yolo: false,
            allowed_paths: Vec::new(),
            permission: Value::Object(serde_json::Map::new()),
            mcp: Value::Object(serde_json::Map::new()),
            skills: Value::Object(serde_json::Map::new()),
            terminal: Value::Object(serde_json::Map::new()),
            runtimes: Value::Object(serde_json::Map::new()),
            browser: Value::Object(serde_json::Map::new()),
            verify: Value::Object(serde_json::Map::new()),
            ui: UiConfig::default(),
            fleet: FleetConfig::default(),
            tool_safety: Value::Object(serde_json::Map::new()),
            delegation: DelegationConfig::default(),
            code_map: CodeMapConfig::default(),
            code_graph: CodeGraphConfig::default(),
            ast_search: AstSearchConfig::default(),
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

    /// The effective sampling params for an agent type. Precedence (each    /// layer wins per-field via `Sampling::merge`):
    /// hardcoded per-agent default → global `sampling` → `sampling.<agent>`.
    /// (An explicit `task`-call override merges on top at the call site.)
    pub fn sampling_for(&self, agent_type: &str) -> bebok_llm::Sampling {
        let mut out = super::super::agent::sampling_defaults::default_sampling(agent_type);
        out = out.merge(sampling_from_value(&self.sampling));
        if let Some(per_agent) = self.sampling.get(agent_type) {
            out = out.merge(sampling_from_value(per_agent));
        }
        out
    }

    /// Whether the parallel-agents fleet is enabled. Capability gate: the
    /// orchestrator prefers fan-out whenever this is on and members are
    /// configured (`FleetContext::is_usable`). The legacy per-prompt
    /// `fleet: true` flag (`FleetContext::requested`) is recorded for
    /// back-compat but no longer gates anything.
    pub fn is_fleet_enabled(&self) -> bool {
        self.fleet.enabled
    }

    /// Fleet members (empty when the fleet is disabled/unconfigured).
    pub fn fleet_members(&self) -> &[FleetMember] {
        &self.fleet.members
    }

    /// The effective `verify.frontend` policy (WP-AUTOVERIFY / F8-1).
    pub fn frontend_verify(&self) -> super::verify::FrontendVerify {
        super::verify::FrontendVerify::from_config(&self.verify)
    }

    /// The effective `verify.buildTest` policy.
    pub fn build_test_mode(&self) -> BuildTestMode {
        BuildTestMode::from_config(&self.verify)
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

    pub fn allowed_paths(mut self, paths: Vec<String>) -> Self {
        self.inner.allowed_paths = paths;
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

    pub fn tool_safety(mut self, v: Value) -> Self {
        self.inner.tool_safety = v;
        self
    }

    pub fn verify(mut self, v: Value) -> Self {
        self.inner.verify = v;
        self
    }

    pub fn delegation(mut self, d: DelegationConfig) -> Self {
        self.inner.delegation = d;
        self
    }

    pub fn code_map(mut self, code_map: CodeMapConfig) -> Self {
        self.inner.code_map = code_map;
        self
    }

    pub fn code_graph(mut self, code_graph: CodeGraphConfig) -> Self {
        self.inner.code_graph = code_graph;
        self
    }

    pub fn ast_search(mut self, ast_search: AstSearchConfig) -> Self {
        self.inner.ast_search = ast_search;
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

    #[test]
    fn build_test_mode_default_is_auto() {
        assert_eq!(BuildTestMode::default(), BuildTestMode::Auto);
        assert_eq!(
            BuildTestMode::from_config(&serde_json::json!({})),
            BuildTestMode::Auto
        );
        assert_eq!(
            BuildTestMode::from_config(&serde_json::Value::Null),
            BuildTestMode::Auto
        );
        // Missing `buildTest` key (e.g. only `frontend` set).
        assert_eq!(
            BuildTestMode::from_config(&serde_json::json!({ "frontend": "off" })),
            BuildTestMode::Auto
        );
    }

    #[test]
    fn build_test_mode_parses_every_value_case_insensitively() {
        assert_eq!(BuildTestMode::parse("auto"), Some(BuildTestMode::Auto));
        assert_eq!(BuildTestMode::parse("Auto"), Some(BuildTestMode::Auto));
        assert_eq!(BuildTestMode::parse(" AUTO "), Some(BuildTestMode::Auto));
        assert_eq!(BuildTestMode::parse("ask"), Some(BuildTestMode::Ask));
        assert_eq!(BuildTestMode::parse("ASK"), Some(BuildTestMode::Ask));
        assert_eq!(BuildTestMode::parse("off"), Some(BuildTestMode::Off));
        assert_eq!(BuildTestMode::parse(" Off "), Some(BuildTestMode::Off));
        assert_eq!(BuildTestMode::parse("none"), Some(BuildTestMode::Off));
        assert_eq!(BuildTestMode::parse("never"), Some(BuildTestMode::Off));
    }

    #[test]
    fn build_test_mode_rejects_unknown_words() {
        assert_eq!(BuildTestMode::parse("sometimes"), None);
        assert_eq!(BuildTestMode::parse(""), None);
        assert_eq!(BuildTestMode::parse("on"), None);
        // Non-string config values fall back to the default.
        assert_eq!(
            BuildTestMode::from_config(&serde_json::json!({ "buildTest": 3 })),
            BuildTestMode::Auto
        );
        assert_eq!(
            BuildTestMode::from_config(&serde_json::json!({ "buildTest": true })),
            BuildTestMode::Auto
        );
    }

    #[test]
    fn build_test_mode_as_str_round_trips() {
        for mode in [BuildTestMode::Auto, BuildTestMode::Ask, BuildTestMode::Off] {
            assert_eq!(BuildTestMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(BuildTestMode::Auto.as_str(), "auto");
        assert_eq!(BuildTestMode::Ask.as_str(), "ask");
        assert_eq!(BuildTestMode::Off.as_str(), "off");
    }

    #[test]
    fn build_test_mode_follows_the_resolved_config() {
        let cfg = |mode: &str| {
            ResolvedConfig::builder()
                .verify(serde_json::json!({ "buildTest": mode }))
                .build()
        };
        assert_eq!(cfg("ask").build_test_mode(), BuildTestMode::Ask);
        assert_eq!(cfg("off").build_test_mode(), BuildTestMode::Off);
        // Default config (no `verify` section) is auto, and a config that
        // only sets `frontend` must not disturb `buildTest`.
        assert_eq!(
            ResolvedConfig::default().build_test_mode(),
            BuildTestMode::Auto
        );
        assert_eq!(
            ResolvedConfig::builder()
                .verify(serde_json::json!({ "frontend": "ask" }))
                .build()
                .build_test_mode(),
            BuildTestMode::Auto
        );
    }

    #[test]
    fn ast_search_config_defaults() {
        let cfg = AstSearchConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_files, DEFAULT_AST_SEARCH_MAX_FILES);
        assert_eq!(cfg.languages, vec!["rs", "ts", "tsx"]);
    }

    #[test]
    fn ast_search_config_serde_round_trip() {
        let cfg = AstSearchConfig {
            enabled: true,
            max_files: 200,
            languages: vec!["rs".into()],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: AstSearchConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn resolved_config_builder_ast_search() {
        let cfg = ResolvedConfig::builder()
            .ast_search(AstSearchConfig {
                enabled: true,
                max_files: 100,
                languages: vec!["rs".into()],
            })
            .build();
        assert!(cfg.ast_search.enabled);
        assert_eq!(cfg.ast_search.max_files, 100);
        assert_eq!(cfg.ast_search.languages, vec!["rs"]);
    }
}
