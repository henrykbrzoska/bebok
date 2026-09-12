//! Provider registry: named provider specs (endpoint, kind, API key, models)
//! plus built-in defaults for the popular providers. A "provider" here is the
//! transport + credential for a family of models; the model prefix selects it
//! (`openai/gpt-4o` -> the `openai` spec).

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        },
        ProviderSpec {
            name: "xai".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.x.ai/v1".into()),
            api_key: None,
            models: vec!["grok-4.6".into(), "grok-4.3".into()],
        },
        ProviderSpec {
            name: "deepseek".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.deepseek.com/v1".into()),
            api_key: None,
            models: vec!["deepseek-chat".into(), "deepseek-reasoner".into()],
        },
        ProviderSpec {
            name: "google".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://generativelanguage.googleapis.com/v1beta/openai".into()),
            api_key: None,
            models: vec!["gemini-2.5-pro".into(), "gemini-2.5-flash".into()],
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
        },
        ProviderSpec {
            name: "groq".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://api.groq.com/openai/v1".into()),
            api_key: None,
            models: vec!["llama-3.3-70b-versatile".into()],
        },
        ProviderSpec {
            name: "qwen".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into()),
            api_key: None,
            models: vec!["qwen-plus".into(), "qwen3-235b-a22b".into()],
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
        },
        ProviderSpec {
            name: "ollama".into(),
            kind: ProviderKind::Openai,
            endpoint: Some("http://localhost:11434/v1".into()),
            api_key: None,
            models: Vec::new(),
        },
    ]
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

    match spec.kind {
        ProviderKind::Openai => {
            if let Some(key) = resolve_api_key(spec) {
                req = req.header("authorization", format!("Bearer {key}"));
            }
        }
        ProviderKind::Anthropic => {
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
