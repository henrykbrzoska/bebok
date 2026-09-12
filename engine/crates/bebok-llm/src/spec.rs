//! Provider registry: named provider specs (endpoint, kind, API key, models)
//! plus built-in defaults for the popular providers. A "provider" here is the
//! transport + credential for a family of models; the model prefix selects it
//! (`openai/gpt-4o` -> the `openai` spec).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::provider::LlmError;

/// How a provider speaks to its models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenAI Chat Completions compatible (OpenAI, xAI, DeepSeek, OpenRouter,
    /// Ollama, and most self-hosted OpenAI-compatible servers).
    Openai,
    /// Anthropic Messages API (Anthropic, Z.ai's Anthropic endpoint).
    Anthropic,
}

impl Default for ProviderKind {
    fn default() -> Self {
        Self::Openai
    }
}

/// One named provider. `api_key` is intentionally plain (empty = not given /
/// fall back to the provider-specific environment variable); the engine never
/// logs it and never sends it to clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    /// API base URL (without a trailing `/chat/completions` or `/messages`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// API key; empty means "not set".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Known model ids (used by the GUI's model picker; a subset, not
    /// authoritative - `list_models` can refresh it).
    #[serde(default)]
    pub models: Vec<String>,
    /// Provider-specific settings that do not deserve a column of their own:
    /// `organization`/`project` (OpenAI), `api-version` (Azure-style hosts),
    /// `http_referer`/`x_title` (OpenRouter), and whatever a future provider
    /// needs. The keys a given provider understands are declared by
    /// [`ProviderUiSpec::extra_fields`]; unknown keys are preserved verbatim, so
    /// a provider can grow options without a config-schema migration. Omitted
    /// from the serialised form when empty, so existing configs are unchanged.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl ProviderSpec {
    /// The full chat/messages URL for this provider.
    pub fn chat_url(&self) -> String {
        let base = self
            .endpoint
            .clone()
            .unwrap_or_else(|| default_endpoint(&self.name, self.kind).to_string());
        let base = base.trim_end_matches('/');
        match self.kind {
            ProviderKind::Openai => format!("{base}/chat/completions"),
            ProviderKind::Anthropic => format!("{base}/messages"),
        }
    }

    /// The models-list URL for this provider.
    pub fn models_url(&self) -> String {
        let base = self
            .endpoint
            .clone()
            .unwrap_or_else(|| default_endpoint(&self.name, self.kind).to_string());
        base.trim_end_matches('/').to_string()
    }

    /// Environment variable holding the provider's key (`openai` -> `OPENAI_API_KEY`).
    pub fn env_var(&self) -> String {
        format!(
            "{}_API_KEY",
            self.name.to_ascii_uppercase().replace('-', "_")
        )
    }

    /// True when an API key is resolvable (explicit key or env var).
    pub fn has_key(&self) -> bool {
        resolve_api_key(self).is_some()
    }

    /// Credential style of this provider (`bearer` / `x-api-key` / `none`).
    pub fn auth(&self) -> ProviderAuth {
        provider_auth(&self.name, self.kind)
    }

    /// False for keyless providers (Ollama and other local servers): no API key
    /// is needed, so a GUI must not render the field and a missing key is not a
    /// misconfiguration.
    pub fn needs_api_key(&self) -> bool {
        self.auth().needs_api_key()
    }

    /// True when this provider is usable as configured: either it needs no key,
    /// or a key is resolvable.
    pub fn is_configured(&self) -> bool {
        !self.needs_api_key() || self.has_key()
    }
}

fn default_endpoint(name: &str, kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => match name {
            "zai" => "https://api.z.ai/api/anthropic/v1",
            _ => "https://api.anthropic.com/v1",
        },
        ProviderKind::Openai => match name {
            "xai" => "https://api.x.ai/v1",
            "deepseek" => "https://api.deepseek.com/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "ollama" => "http://localhost:11434/v1",
            "google" => "https://generativelanguage.googleapis.com/v1beta/openai",
            "mistralai" => "https://api.mistral.ai/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "qwen" => "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "openai" => "https://api.openai.com/v1",
            _ => "https://api.openai.com/v1",
        },
    }
}

/// Built-in provider defaults (the GUI can override/extend them via config).
pub fn builtin_provider_specs() -> Vec<ProviderSpec> {
    vec![
        ProviderSpec {
            name: "zai".into(),
            kind: ProviderKind::Anthropic,
            endpoint: Some("https://api.z.ai/api/anthropic/v1".into()),
            api_key: None,
            models: vec![
                "glm-5.3-flash".into(),
                "glm-4.7-flash".into(),
                "glm-4.6".into(),
                "glm-4.5-air".into(),
            ],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "openai".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.openai.com/v1".into()),
            api_key: None,
            models: vec![
                "gpt-4.1".into(),
                "gpt-4.1-mini".into(),
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
                "o3-mini".into(),
            ],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "anthropic".into(),
            kind: ProviderKind::Anthropic,
            endpoint: Some("https://api.anthropic.com/v1".into()),
            api_key: None,
            models: vec![
                "claude-opus-4-5".into(),
                "claude-sonnet-4-5".into(),
                "claude-haiku-4-5".into(),
            ],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "xai".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.x.ai/v1".into()),
            api_key: None,
            models: vec!["grok-4.6".into(), "grok-4.3".into()],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "deepseek".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.deepseek.com/v1".into()),
            api_key: None,
            models: vec!["deepseek-chat".into(), "deepseek-reasoner".into()],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "google".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://generativelanguage.googleapis.com/v1beta/openai".into()),
            api_key: None,
            models: vec!["gemini-2.5-pro".into(), "gemini-2.5-flash".into()],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "mistralai".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.mistral.ai/v1".into()),
            api_key: None,
            models: vec![
                "mistral-large-latest".into(),
                "mistral-medium-latest".into(),
            ],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "groq".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.groq.com/openai/v1".into()),
            api_key: None,
            models: vec!["llama-3.3-70b-versatile".into()],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "qwen".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into()),
            api_key: None,
            models: vec!["qwen-plus".into(), "qwen3-235b-a22b".into()],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "openrouter".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://openrouter.ai/api/v1".into()),
            api_key: None,
            models: vec![
                "openai/gpt-4.1".into(),
                "anthropic/claude-sonnet-4-5".into(),
                "google/gemini-2.5-pro".into(),
                "z-ai/glm-5.3-flash".into(),
                "deepseek/deepseek-v4-flash".into(),
            ],
            extra: Map::new(),
        },
        ProviderSpec {
            name: "ollama".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("http://localhost:11434/v1".into()),
            api_key: None,
            models: Vec::new(),
            extra: Map::new(),
        },
    ]
}

/// How a provider authenticates. Separate from [`ProviderKind`] (the wire
/// protocol), because two providers speaking the same protocol can differ here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderAuth {
    /// `Authorization: Bearer <key>` (OpenAI and every OpenAI-compatible host).
    #[default]
    Bearer,
    /// `x-api-key: <key>` (Anthropic Messages API).
    #[serde(rename = "x-api-key")]
    XApiKey,
    /// No credentials at all (local servers such as Ollama). A GUI must not
    /// render an API-key field for these.
    None,
}

impl ProviderAuth {
    /// Lowercase wire name (`bearer` / `x-api-key` / `none`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderAuth::Bearer => "bearer",
            ProviderAuth::XApiKey => "x-api-key",
            ProviderAuth::None => "none",
        }
    }

    /// False for keyless providers: the GUI should hide the API-key field and
    /// the engine must not treat a missing key as a misconfiguration.
    pub fn needs_api_key(&self) -> bool {
        !matches!(self, ProviderAuth::None)
    }
}

/// Input type for a [`ProviderExtraField`] (a hint for the settings form).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderFieldType {
    Text,
    Password,
    Url,
    Number,
    Bool,
}

/// One provider-specific configuration field, declared by the engine and
/// rendered by the GUI. Values land in [`ProviderSpec::extra`] under `key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderExtraField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: ProviderFieldType,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

impl ProviderExtraField {
    fn text(key: &str, label: &str, placeholder: &str) -> Self {
        Self {
            key: key.to_string(),
            label: label.to_string(),
            field_type: ProviderFieldType::Text,
            required: false,
            placeholder: (!placeholder.is_empty()).then(|| placeholder.to_string()),
        }
    }
}

/// The machine-readable descriptor of a provider, for building a settings UI.
///
/// This is deliberately *not* [`ProviderSpec`]: the spec is runtime state
/// (endpoint, key, model list) that the user edits and the engine persists,
/// while this is the static shape of the form used to edit it — what the field
/// is called, whether a key is needed at all, which environment variable is
/// consulted, and which provider-specific extras exist. Served by
/// `GET /providers/catalog`; keys are camelCase for the TypeScript client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUiSpec {
    /// Provider id; matches [`ProviderSpec::name`] and the model prefix.
    pub id: String,
    /// Human-readable name for the settings list.
    pub label: String,
    /// Wire protocol (`openai` | `anthropic`).
    pub kind: ProviderKind,
    /// Credential style (`bearer` | `x-api-key` | `none`).
    pub auth: ProviderAuth,
    /// Environment variable consulted when no key is stored in the config.
    pub env_var: String,
    /// Default API base URL (what an empty `endpoint` falls back to).
    pub base_url_default: String,
    /// Provider-specific fields, stored in [`ProviderSpec::extra`].
    pub extra_fields: Vec<ProviderExtraField>,
}

/// Extra (provider-specific) fields a given built-in exposes.
fn builtin_extra_fields(name: &str) -> Vec<ProviderExtraField> {
    match name {
        "openai" => vec![
            ProviderExtraField::text("organization", "Organization ID", "org-..."),
            ProviderExtraField::text("project", "Project ID", "proj_..."),
        ],
        "openrouter" => vec![
            ProviderExtraField::text("http_referer", "HTTP-Referer", "https://your.app"),
            ProviderExtraField::text("x_title", "X-Title", "Bebok"),
        ],
        _ => Vec::new(),
    }
}

/// Display name for a built-in provider id (falls back to the id itself for
/// custom providers).
fn provider_label(name: &str) -> String {
    match name {
        "zai" => "Z.ai",
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        "xai" => "xAI",
        "deepseek" => "DeepSeek",
        "google" => "Google Gemini",
        "mistralai" => "Mistral AI",
        "groq" => "Groq",
        "qwen" => "Qwen (DashScope)",
        "openrouter" => "OpenRouter",
        "ollama" => "Ollama",
        other => return other.to_string(),
    }
    .to_string()
}

/// Credential style of a provider: keyless for local servers, otherwise the
/// protocol's header (`x-api-key` for Anthropic, bearer for OpenAI-compatible).
fn provider_auth(name: &str, kind: ProviderKind) -> ProviderAuth {
    match name {
        "ollama" => ProviderAuth::None,
        _ => match kind {
            ProviderKind::Anthropic => ProviderAuth::XApiKey,
            ProviderKind::Openai => ProviderAuth::Bearer,
        },
    }
}

/// Describe one provider spec for the settings UI.
pub fn provider_ui_spec(spec: &ProviderSpec) -> ProviderUiSpec {
    ProviderUiSpec {
        id: spec.name.clone(),
        label: provider_label(&spec.name),
        kind: spec.kind,
        auth: provider_auth(&spec.name, spec.kind),
        env_var: spec.env_var(),
        base_url_default: default_endpoint(&spec.name, spec.kind).to_string(),
        extra_fields: builtin_extra_fields(&spec.name),
    }
}

/// The provider catalog: one [`ProviderUiSpec`] per built-in provider, in the
/// order of [`builtin_provider_specs`]. Served by `GET /providers/catalog`.
pub fn provider_catalog() -> Vec<ProviderUiSpec> {
    builtin_provider_specs()
        .iter()
        .map(provider_ui_spec)
        .collect()
}

/// Resolve the effective provider specs: config entries override built-ins by
/// name; unknown names are appended (custom/self-hosted providers).
pub fn resolve_provider_specs(config_specs: &[ProviderSpec]) -> Vec<ProviderSpec> {
    let mut out = builtin_provider_specs();
    for spec in config_specs {
        match out.iter_mut().find(|b| b.name == spec.name) {
            Some(builtin) => {
                builtin.kind = spec.kind;
                if spec.endpoint.is_some() {
                    builtin.endpoint = spec.endpoint.clone();
                }
                if spec.api_key.is_some() {
                    builtin.api_key = spec.api_key.clone();
                }
                if !spec.models.is_empty() {
                    builtin.models = spec.models.clone();
                }
                // Extra fields merge key-by-key: a config that sets only
                // `organization` must not drop a `project` set elsewhere.
                for (k, v) in &spec.extra {
                    builtin.extra.insert(k.clone(), v.clone());
                }
            }
            None => out.push(spec.clone()),
        }
    }
    out
}

/// Look up one provider spec by name (falling back to built-ins).
pub fn find_provider_spec<'a>(specs: &'a [ProviderSpec], name: &str) -> Option<&'a ProviderSpec> {
    specs.iter().find(|s| s.name == name)
}

/// Resolve the API key for a spec: explicit key -> provider env var -> `None`.
pub fn resolve_api_key(spec: &ProviderSpec) -> Option<String> {
    if let Some(key) = spec.api_key.as_ref().filter(|s| !s.trim().is_empty()) {
        return Some(key.clone());
    }
    std::env::var(spec.env_var())
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// List available model ids for a provider by calling its `GET /models` (or
/// Ollama's `/v1/models`) endpoint. This is the engine-side implementation of
/// the GUI's "check available models" button.
pub async fn list_models(spec: &ProviderSpec) -> Result<Vec<String>, LlmError> {
    let client = reqwest::Client::new();
    let url = format!("{}/models", spec.models_url());
    let mut req = client.get(&url);

    // Keyless providers (Ollama) get no credential header at all, so a stray
    // `OLLAMA_API_KEY` in the environment cannot make a local server 401.
    match spec.auth() {
        ProviderAuth::None => {}
        ProviderAuth::Bearer => {
            if let Some(key) = resolve_api_key(spec) {
                req = req.header("authorization", format!("Bearer {key}"));
            }
        }
        ProviderAuth::XApiKey => {
            if let Some(key) = resolve_api_key(spec) {
                req = req
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
        }
    }

    let resp = req.send().await.map_err(LlmError::Request)?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let retry_after = crate::provider::retry_after_from_headers(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::Http {
            status,
            body,
            retry_after,
        });
    }

    let value: serde_json::Value = resp.json().await.map_err(LlmError::Request)?;
    parse_models(&value)
}

/// Parse a models-list response across provider shapes:
/// - OpenAI / Anthropic: `{ "data": [ { "id": "..." }, ... ] }`
/// - Ollama (native):    `{ "models": [ { "name": "..." }, ... ] }`
/// - Z.ai error:         `{ "code": ..., "msg": "...", "success": false }`
pub fn parse_models(value: &serde_json::Value) -> Result<Vec<String>, LlmError> {
    // Explicit provider-side error envelope.
    if value.get("success").and_then(|s| s.as_bool()) == Some(false) {
        let msg = value
            .get("msg")
            .or_else(|| value.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return Err(LlmError::Provider(msg));
    }
    if let Some(err) = value.get("error") {
        if !err.is_null() {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| err.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Err(LlmError::Provider(msg));
        }
    }

    if let Some(data) = value.get("data").and_then(|d| d.as_array()) {
        let ids: Vec<String> = data
            .iter()
            .filter_map(|m| m.get("id").and_then(|x| x.as_str()).map(str::to_string))
            .collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }

    if let Some(models) = value.get("models").and_then(|d| d.as_array()) {
        let ids: Vec<String> = models
            .iter()
            .filter_map(|m| {
                m.get("id")
                    .or_else(|| m.get("name"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .collect();
        return Ok(ids);
    }

    Ok(Vec::new())
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// Every built-in provider is described, in registry order.
    #[test]
    fn catalog_covers_every_builtin_provider() {
        let specs = builtin_provider_specs();
        let catalog = provider_catalog();
        assert_eq!(catalog.len(), specs.len());
        assert_eq!(catalog.len(), 11, "11 built-in providers today");
        let ids: Vec<&str> = catalog.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "zai",
                "openai",
                "anthropic",
                "xai",
                "deepseek",
                "google",
                "mistralai",
                "groq",
                "qwen",
                "openrouter",
                "ollama",
            ]
        );
    }

    /// The shape the settings screen consumes: no empty label/env var, and a
    /// default base URL that matches the spec's own endpoint default.
    #[test]
    fn catalog_entries_have_the_documented_shape() {
        for entry in provider_catalog() {
            assert!(!entry.id.is_empty());
            assert!(!entry.label.is_empty(), "{} has no label", entry.id);
            assert!(!entry.env_var.is_empty());
            assert!(
                entry.base_url_default.starts_with("http"),
                "{}: {}",
                entry.id,
                entry.base_url_default
            );
        }
    }

    #[test]
    fn env_var_and_base_url_match_the_runtime_spec() {
        let specs = builtin_provider_specs();
        for entry in provider_catalog() {
            let spec = find_provider_spec(&specs, &entry.id).expect("built-in spec");
            assert_eq!(entry.env_var, spec.env_var());
            assert_eq!(entry.kind, spec.kind);
            assert_eq!(entry.base_url_default, spec.models_url());
        }
    }

    #[test]
    fn auth_follows_the_protocol_for_keyed_providers() {
        let catalog = provider_catalog();
        let by_id = |id: &str| catalog.iter().find(|p| p.id == id).unwrap().auth;
        assert_eq!(by_id("openai"), ProviderAuth::Bearer);
        assert_eq!(by_id("groq"), ProviderAuth::Bearer);
        assert_eq!(by_id("anthropic"), ProviderAuth::XApiKey);
        assert_eq!(by_id("zai"), ProviderAuth::XApiKey);
    }

    #[test]
    fn extra_fields_are_declared_where_the_provider_has_them() {
        let catalog = provider_catalog();
        let openai = catalog.iter().find(|p| p.id == "openai").unwrap();
        let keys: Vec<&str> = openai.extra_fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["organization", "project"]);
        assert!(openai.extra_fields.iter().all(|f| !f.required));

        let openrouter = catalog.iter().find(|p| p.id == "openrouter").unwrap();
        let keys: Vec<&str> = openrouter
            .extra_fields
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(keys, vec!["http_referer", "x_title"]);

        let anthropic = catalog.iter().find(|p| p.id == "anthropic").unwrap();
        assert!(anthropic.extra_fields.is_empty());
    }

    /// The catalog is serialised for a TypeScript client: camelCase keys, the
    /// field type under `type`.
    #[test]
    fn catalog_serialises_with_camel_case_keys() {
        let entry = provider_catalog()
            .into_iter()
            .find(|p| p.id == "openai")
            .unwrap();
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["id"], "openai");
        assert_eq!(v["label"], "OpenAI");
        assert_eq!(v["kind"], "openai");
        assert_eq!(v["auth"], "bearer");
        assert_eq!(v["envVar"], "OPENAI_API_KEY");
        assert_eq!(v["baseUrlDefault"], "https://api.openai.com/v1");
        assert_eq!(v["extraFields"][0]["key"], "organization");
        assert_eq!(v["extraFields"][0]["label"], "Organization ID");
        assert_eq!(v["extraFields"][0]["type"], "text");
        assert_eq!(v["extraFields"][0]["required"], false);
        assert_eq!(v["extraFields"][0]["placeholder"], "org-...");
        // No stray snake_case keys leaked into the payload.
        assert!(v.get("env_var").is_none());
        assert!(v.get("base_url_default").is_none());
        assert!(v.get("extra_fields").is_none());
    }

    /// A custom (non-built-in) provider is describable too: the id doubles as
    /// the label and the protocol decides the auth header.
    #[test]
    fn custom_provider_is_described_from_its_spec() {
        let spec = ProviderSpec {
            name: "my-llm".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("http://10.0.0.5:8000/v1".into()),
            api_key: None,
            models: vec!["local-7b".into()],
            extra: Map::new(),
        };
        let ui = provider_ui_spec(&spec);
        assert_eq!(ui.id, "my-llm");
        assert_eq!(ui.label, "my-llm");
        assert_eq!(ui.auth, ProviderAuth::Bearer);
        assert_eq!(ui.env_var, "MY_LLM_API_KEY");
        assert!(ui.extra_fields.is_empty());
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    /// Ollama is keyless: the catalog says so, so the settings UI can drop the
    /// API-key field entirely instead of showing an input nothing reads.
    #[test]
    fn catalog_reports_auth_none_for_ollama() {
        let ollama = provider_catalog()
            .into_iter()
            .find(|p| p.id == "ollama")
            .expect("ollama in catalog");
        assert_eq!(ollama.auth, ProviderAuth::None);
        assert!(!ollama.auth.needs_api_key());

        let v = serde_json::to_value(&ollama).unwrap();
        assert_eq!(v["auth"], "none");
        assert_eq!(v["baseUrlDefault"], "http://localhost:11434/v1");
    }

    /// Every other built-in still declares a credential style.
    #[test]
    fn every_other_builtin_needs_a_key() {
        for entry in provider_catalog() {
            if entry.id == "ollama" {
                continue;
            }
            assert!(
                entry.auth.needs_api_key(),
                "{} unexpectedly keyless",
                entry.id
            );
            assert_ne!(entry.auth, ProviderAuth::None);
        }
    }

    #[test]
    fn keyless_provider_is_configured_without_a_key() {
        let specs = builtin_provider_specs();
        let ollama = find_provider_spec(&specs, "ollama").unwrap();
        assert_eq!(ollama.auth(), ProviderAuth::None);
        assert!(!ollama.needs_api_key());
        assert!(ollama.is_configured(), "keyless provider is ready as-is");

        let openai = find_provider_spec(&specs, "openai").unwrap();
        assert!(openai.needs_api_key());
        assert_eq!(openai.is_configured(), openai.has_key());
    }

    #[test]
    fn auth_serde_round_trips() {
        for auth in [
            ProviderAuth::Bearer,
            ProviderAuth::XApiKey,
            ProviderAuth::None,
        ] {
            let s = serde_json::to_string(&auth).unwrap();
            assert_eq!(s, format!("\"{}\"", auth.as_str()));
            assert_eq!(serde_json::from_str::<ProviderAuth>(&s).unwrap(), auth);
        }
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    fn spec_with_extra() -> ProviderSpec {
        let mut extra = Map::new();
        extra.insert("organization".into(), Value::String("org-abc".into()));
        extra.insert("project".into(), Value::String("proj_1".into()));
        extra.insert("api-version".into(), Value::String("2024-10-21".into()));
        ProviderSpec {
            name: "openai".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.openai.com/v1".into()),
            api_key: Some("sk-test".into()),
            models: vec!["gpt-4.1".into()],
            extra,
        }
    }

    /// A provider persists extra fields without any change to the base schema.
    #[test]
    fn extra_fields_round_trip_through_serialization() {
        let spec = spec_with_extra();
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["extra"]["organization"], "org-abc");
        assert_eq!(json["extra"]["project"], "proj_1");
        assert_eq!(json["extra"]["api-version"], "2024-10-21");

        let back: ProviderSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.extra.len(), 3);
        assert_eq!(back.extra["organization"], Value::String("org-abc".into()));
        assert_eq!(
            back.extra["api-version"],
            Value::String("2024-10-21".into())
        );
        assert_eq!(back.name, "openai");
        assert_eq!(back.models, vec!["gpt-4.1".to_string()]);
    }

    /// Non-string values (numbers, booleans, nested objects) survive too - the
    /// map is opaque to the engine.
    #[test]
    fn extra_accepts_arbitrary_json_values() {
        let raw = serde_json::json!({
            "name": "custom",
            "kind": "openai",
            "extra": {
                "timeout_ms": 30000,
                "insecure": true,
                "headers": { "X-Title": "Bebok" }
            }
        });
        let spec: ProviderSpec = serde_json::from_value(raw).unwrap();
        assert_eq!(spec.extra["timeout_ms"], 30000);
        assert_eq!(spec.extra["insecure"], true);
        assert_eq!(spec.extra["headers"]["X-Title"], "Bebok");

        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(back["extra"]["headers"]["X-Title"], "Bebok");
    }

    /// Existing configs (no `extra`) still parse, and an empty map is omitted
    /// from the output so no config file grows a noise key.
    #[test]
    fn missing_extra_defaults_to_empty_and_is_not_serialized() {
        let spec: ProviderSpec =
            serde_json::from_value(serde_json::json!({ "name": "groq" })).unwrap();
        assert!(spec.extra.is_empty());

        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("extra").is_none(), "empty extra must be omitted");
    }

    /// Config layers merge extras key-by-key instead of replacing the map.
    #[test]
    fn resolve_merges_extra_into_the_builtin() {
        let mut extra = Map::new();
        extra.insert("organization".into(), Value::String("org-1".into()));
        let override_spec = ProviderSpec {
            name: "openai".into(),
            kind: ProviderKind::Openai,
            extra,
            ..Default::default()
        };
        let resolved = resolve_provider_specs(&[override_spec]);
        let openai = find_provider_spec(&resolved, "openai").unwrap();
        assert_eq!(openai.extra["organization"], Value::String("org-1".into()));
        // Untouched fields of the built-in survive the merge.
        assert_eq!(
            openai.endpoint.as_deref(),
            Some("https://api.openai.com/v1")
        );

        let mut extra = Map::new();
        extra.insert("project".into(), Value::String("proj-2".into()));
        let second = ProviderSpec {
            name: "openai".into(),
            kind: ProviderKind::Openai,
            extra,
            ..Default::default()
        };
        let resolved = resolve_provider_specs(&[
            ProviderSpec {
                name: "openai".into(),
                kind: ProviderKind::Openai,
                extra: openai.extra.clone(),
                ..Default::default()
            },
            second,
        ]);
        let openai = find_provider_spec(&resolved, "openai").unwrap();
        assert_eq!(openai.extra["organization"], Value::String("org-1".into()));
        assert_eq!(openai.extra["project"], Value::String("proj-2".into()));
    }

    /// A custom provider carrying extras is appended whole.
    #[test]
    fn custom_provider_keeps_its_extra() {
        let mut extra = Map::new();
        extra.insert("deployment".into(), Value::String("gpt4o-eu".into()));
        let custom = ProviderSpec {
            name: "azure".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://x.openai.azure.com/openai".into()),
            extra,
            ..Default::default()
        };
        let resolved = resolve_provider_specs(&[custom]);
        let azure = find_provider_spec(&resolved, "azure").unwrap();
        assert_eq!(azure.extra["deployment"], Value::String("gpt4o-eu".into()));
    }

    /// The catalog declares which keys a provider understands; the values live
    /// in `ProviderSpec::extra` under exactly those keys.
    #[test]
    fn declared_extra_fields_match_the_extra_map_keys() {
        let ui = provider_catalog()
            .into_iter()
            .find(|p| p.id == "openai")
            .unwrap();
        let spec = spec_with_extra();
        for field in &ui.extra_fields {
            assert!(
                spec.extra.contains_key(&field.key),
                "declared field {} has no home in extra",
                field.key
            );
        }
    }
}
