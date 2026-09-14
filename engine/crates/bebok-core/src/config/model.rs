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
/// Default port of the remote (tailnet/LAN) listener (WP-M1, F10-1).
pub const DEFAULT_REMOTE_PORT: u16 = 8790;

/// WP-M1 (F10-1): which high-volume event families the remote listener
/// republishes to paired phones (`remote.publish.*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemotePublishConfig {
    /// Forward `browser.frame` thumbnails (only when the stream asks for
    /// them with `?interest=browser`).
    pub browser_frames: bool,
    /// Forward `process.output` / `process.exited` (coalesced).
    pub processes: bool,
}

impl Default for RemotePublishConfig {
    fn default() -> Self {
        Self {
            browser_frames: true,
            processes: true,
        }
    }
}

/// WP-M1 (F10-1): push-notification settings, parsed here and consumed by
/// `push.rs` (WP-M7). `provider` is `none` (default) or `ntfy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemotePushConfig {
    pub provider: String,
    pub url: String,
}

impl Default for RemotePushConfig {
    fn default() -> Self {
        Self {
            provider: "none".to_string(),
            url: String::new(),
        }
    }
}

/// 1.8 (relay): the engine dials out to a `bebok-relay` worker (Cloudflare
/// Workers + Durable Objects, `relay/` in the repo) so paired phones reach it
/// from anywhere over HTTPS. `secret` is minted by the engine the first time
/// the relay is enabled and claims the tunnel id on the relay (TOFU);
/// `url` is the worker origin, e.g. `https://bebok-relay.example.workers.dev`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteRelayConfig {
    pub enabled: bool,
    pub url: String,
    pub secret: String,
}

/// WP-M1 (F10-1): the `remote` section of the **global** config — the
/// second listener that paired phones reach over Tailscale / LAN, plus the
/// 1.8 relay.
///
/// ```jsonc
/// "remote": {
///   "enabled": false, "port": 8790, "allow_lan": false,
///   "publish": { "browser_frames": true, "processes": true },
///   "push": { "provider": "none", "url": "" },
///   "relay": { "enabled": false, "url": "", "secret": "" }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    pub enabled: bool,
    pub port: u16,
    /// Also bind RFC1918 addresses (LAN without TLS - off by default, see
    /// the 1.6 threat model).
    pub allow_lan: bool,
    pub publish: RemotePublishConfig,
    pub push: RemotePushConfig,
    pub relay: RemoteRelayConfig,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_REMOTE_PORT,
            allow_lan: false,
            publish: RemotePublishConfig::default(),
            push: RemotePushConfig::default(),
            relay: RemoteRelayConfig::default(),
        }
    }
}

impl RemoteConfig {
    /// Parse a `remote` section on top of the defaults (see [`Self::apply`]).
    pub fn from_value(v: &Value) -> Self {
        let mut cfg = Self::default();
        cfg.apply(v);
        cfg
    }

    /// Apply one layer's `remote` section key by key; malformed values are
    /// ignored per key. Both `snake_case` and `camelCase` keys are accepted
    /// (`allow_lan` / `allowLan`, `browser_frames` / `browserFrames`).
    pub fn apply(&mut self, v: &Value) {
        let Some(obj) = v.as_object() else {
            return;
        };
        let pick = |a: &str, b: &str| obj.get(a).or_else(|| obj.get(b));
        if let Some(enabled) = obj.get("enabled").and_then(Value::as_bool) {
            self.enabled = enabled;
        }
        if let Some(port) = obj.get("port").and_then(Value::as_u64)
            && (1..=u64::from(u16::MAX)).contains(&port)
        {
            self.port = port as u16;
        }
        if let Some(lan) = pick("allow_lan", "allowLan").and_then(Value::as_bool) {
            self.allow_lan = lan;
        }
        if let Some(publish) = obj.get("publish").and_then(Value::as_object) {
            let pick = |a: &str, b: &str| publish.get(a).or_else(|| publish.get(b));
            if let Some(b) = pick("browser_frames", "browserFrames").and_then(Value::as_bool) {
                self.publish.browser_frames = b;
            }
            if let Some(b) = publish.get("processes").and_then(Value::as_bool) {
                self.publish.processes = b;
            }
        }
        if let Some(relay) = obj.get("relay").and_then(Value::as_object) {
            if let Some(b) = relay.get("enabled").and_then(Value::as_bool) {
                self.relay.enabled = b;
            }
            if let Some(u) = relay.get("url").and_then(Value::as_str) {
                self.relay.url = u.trim().trim_end_matches('/').to_string();
            }
            if let Some(sec) = relay.get("secret").and_then(Value::as_str) {
                self.relay.secret = sec.trim().to_string();
            }
        }
        if let Some(push) = obj.get("push").and_then(Value::as_object) {
            if let Some(p) = push.get("provider").and_then(Value::as_str) {
                let p = p.trim().to_ascii_lowercase();
                self.push.provider = if p.is_empty() { "none".to_string() } else { p };
            }
            if let Some(u) = push.get("url").and_then(Value::as_str) {
                self.push.url = u.trim().to_string();
            }
        }
    }

    /// The relay is usable: enabled with a worker URL.
    pub fn relay_enabled(&self) -> bool {
        self.relay.enabled && !self.relay.url.trim().is_empty()
    }

    /// True when a push provider other than `none` is configured.
    pub fn push_enabled(&self) -> bool {
        self.push.provider != "none" && !self.push.provider.is_empty()
    }
}

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

/// WP-DELEGATION (F8-2): when the main agent should hand work to sub-agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DelegationMode {
    /// No policy text; the `task`/`fleet` tools stay available.
    Off,
    /// Decompose when the task has independent parts / spans areas (default).
    #[default]
    Auto,
    /// Decompose every non-trivial task.
    Always,
}

impl DelegationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DelegationMode::Off => "off",
            DelegationMode::Auto => "auto",
            DelegationMode::Always => "always",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(DelegationMode::Off),
            "auto" => Some(DelegationMode::Auto),
            "always" => Some(DelegationMode::Always),
            _ => None,
        }
    }
}

/// Default number of sub-agents that may run at the same time per session.
pub const DEFAULT_DELEGATION_MAX_CONCURRENT: usize = 3;
/// Hard ceiling for `delegation.max_concurrent` (guards against typos).
pub const MAX_DELEGATION_MAX_CONCURRENT: usize = 16;

/// F9-10: `delegation.model_policy` — which model sub-agents run on.
/// Serialised as a plain string: `"inherit"`, `"cheaper"` or an explicit
/// `provider/model` id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DelegationModelPolicy {
    /// The parent's model.
    Inherit,
    /// A lighter sibling of the parent's model (same provider, via the model
    /// catalog); inherit when there is none.
    #[default]
    Cheaper,
    /// Always this model.
    Explicit(String),
}

impl DelegationModelPolicy {
    /// Parse the config string (`inherit` / `cheaper` / anything else = an
    /// explicit model id; empty = the default `cheaper`).
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "" | "cheaper" => DelegationModelPolicy::Cheaper,
            "inherit" => DelegationModelPolicy::Inherit,
            _ => DelegationModelPolicy::Explicit(t.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            DelegationModelPolicy::Inherit => "inherit",
            DelegationModelPolicy::Cheaper => "cheaper",
            DelegationModelPolicy::Explicit(m) => m.as_str(),
        }
    }
}

impl Serialize for DelegationModelPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DelegationModelPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(DelegationModelPolicy::parse(&s))
    }
}

/// WP-DELEGATION (F8-2): `delegation` config section. Global config with a
/// per-key project override (a project that sets only `mode` keeps the global
/// `max_concurrent` / `model_policy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    pub mode: DelegationMode,
    /// Upper bound on concurrently *running* children; extra ones queue.
    pub max_concurrent: usize,
    /// Legacy (pre F9-10) explicit model override for every sub-agent
    /// (`provider/model`). Still honoured: a non-empty value with no
    /// `model_policy` means `Explicit(model)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// F9-10: `inherit` | `cheaper` (default) | explicit `provider/model`.
    pub model_policy: DelegationModelPolicy,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            mode: DelegationMode::Auto,
            max_concurrent: DEFAULT_DELEGATION_MAX_CONCURRENT,
            model: None,
            model_policy: DelegationModelPolicy::Cheaper,
        }
    }
}

impl DelegationConfig {
    /// `max_concurrent` clamped to `1..=MAX_DELEGATION_MAX_CONCURRENT`.
    pub fn effective_max_concurrent(&self) -> usize {
        self.max_concurrent.clamp(1, MAX_DELEGATION_MAX_CONCURRENT)
    }

    /// The explicit sub-agent model, if the effective policy names one
    /// (legacy `model` key or an explicit `model_policy`).
    pub fn model_override(&self) -> Option<String> {
        match self.effective_model_policy() {
            DelegationModelPolicy::Explicit(m) => Some(m),
            _ => None,
        }
    }

    /// The policy in force: `model_policy`, except that a legacy non-empty
    /// `model` with the default policy means "explicit that model".
    pub fn effective_model_policy(&self) -> DelegationModelPolicy {
        if self.model_policy == DelegationModelPolicy::Cheaper
            && let Some(m) = self
                .model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
        {
            return DelegationModelPolicy::Explicit(m.to_string());
        }
        self.model_policy.clone()
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
    /// WP-M1 (F10-1): remote listener + device pairing settings (global).
    #[serde(default)]
    pub remote: RemoteConfig,
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
            browser: Value::Object(serde_json::Map::new()),
            verify: Value::Object(serde_json::Map::new()),
            ui: UiConfig::default(),
            fleet: FleetConfig::default(),
            tool_safety: Value::Object(serde_json::Map::new()),
            delegation: DelegationConfig::default(),
            remote: RemoteConfig::default(),
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

    pub fn remote(mut self, remote: RemoteConfig) -> Self {
        self.inner.remote = remote;
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

    /// WP-M1 (F10-1): `remote` defaults are off/8790/no-LAN, publish both,
    /// push none; the section round-trips through serde with those keys.
    #[test]
    fn remote_config_defaults_and_serde_round_trip() {
        let cfg = RemoteConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.port, DEFAULT_REMOTE_PORT);
        assert!(!cfg.allow_lan);
        assert!(cfg.publish.browser_frames);
        assert!(cfg.publish.processes);
        assert_eq!(cfg.push.provider, "none");
        assert!(cfg.push.url.is_empty());
        assert!(!cfg.push_enabled());

        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["port"], 8790);
        assert_eq!(json["publish"]["browser_frames"], true);
        assert_eq!(json["push"]["provider"], "none");
        let back: RemoteConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, cfg);

        // Partial JSON keeps the other defaults.
        let partial: RemoteConfig =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.port, DEFAULT_REMOTE_PORT);
    }

    #[test]
    fn remote_config_apply_is_per_key_and_tolerant() {
        let mut cfg = RemoteConfig::default();
        cfg.apply(&serde_json::json!({
            "enabled": true, "port": 9000, "allowLan": true,
            "publish": { "browserFrames": false },
            "push": { "provider": "NTFY", "url": " https://ntfy.sh " }
        }));
        assert!(cfg.enabled);
        assert_eq!(cfg.port, 9000);
        assert!(cfg.allow_lan);
        assert!(!cfg.publish.browser_frames);
        assert!(cfg.publish.processes, "untouched key keeps its default");
        assert_eq!(cfg.push.provider, "ntfy");
        assert_eq!(cfg.push.url, "https://ntfy.sh");
        assert!(cfg.push_enabled());

        // Malformed values are ignored key by key; port 0 / > 65535 rejected.
        cfg.apply(&serde_json::json!({ "enabled": "yes", "port": 0, "allow_lan": 1 }));
        assert!(cfg.enabled);
        assert_eq!(cfg.port, 9000);
        assert!(cfg.allow_lan);
        cfg.apply(&serde_json::json!({ "port": 70000, "push": { "provider": "" } }));
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.push.provider, "none");
        // Non-object section: no-op.
        cfg.apply(&serde_json::json!("off"));
        assert!(cfg.enabled);
    }

    #[test]
    fn sanitize_truncates_css() {
        let long = "x".repeat(MAX_CUSTOM_CSS_LEN + 10);
        let out = UiConfig::sanitize_css(&long);
        assert_eq!(out.len(), MAX_CUSTOM_CSS_LEN);
    }
}
