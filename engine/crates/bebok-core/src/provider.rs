//! Provider factory (Factory): build a provider client for a `provider/model`
//! string. Lives in core (not the server) so core-side features — e.g. the
//! sub-agent `task` tool — can build providers without depending on the
//! server layer.
//!
//! Selection is by model prefix (`zai/...`, `openai/...`, `anthropic/...`,
//! `xai/...`, `deepseek/...`, `google/...`, `openrouter/...`, `ollama/...` or
//! any provider name from config). The API key comes from the provider spec
//! (`api_key` field or the provider env var) with a final fallback to the
//! resolved `config.api_key`.

use std::sync::Arc;

use bebok_llm::{AnthropicProvider, OpenAiProvider, Provider, ProviderKind};

use crate::config::{ResolvedConfig, provider_from_model};
use crate::error::CoreError;

/// Build the provider for a model (see module docs for selection rules).
pub fn build_provider(config: &ResolvedConfig, model: &str) -> Result<Arc<dyn Provider>, CoreError> {
    let name = provider_from_model(model);
    let spec = config
        .provider_spec(&name)
        .ok_or_else(|| CoreError::ProviderConfig(format!("unknown provider '{name}'")))?;

    let mut key = bebok_llm::resolve_api_key(&spec);
    if key.is_none() {
        key = config.api_key.clone().filter(|s| !s.trim().is_empty());
    }

    match spec.kind {
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(|| {
                CoreError::ProviderConfig(format!(
                    "no API key for provider '{name}': set its api_key or the {} env var",
                    spec.env_var()
                ))
            })?;
            Ok(Arc::new(
                AnthropicProvider::new(key).with_base_url(spec.chat_url()),
            ))
        }
        ProviderKind::Openai => Ok(Arc::new(OpenAiProvider::new(key, spec.chat_url()))),
    }
}
