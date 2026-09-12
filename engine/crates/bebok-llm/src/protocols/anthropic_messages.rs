use std::sync::Arc;

use crate::provider::{AdapterConfig, ProviderAdapter};
use crate::{AnthropicProvider, Provider};

#[derive(Debug)]
pub struct AnthropicMessagesAdapter;

impl ProviderAdapter for AnthropicMessagesAdapter {
    fn type_name(&self) -> &'static str {
        "AnthropicMessagesAdapter"
    }

    fn build(&self, config: AdapterConfig) -> Result<Arc<dyn Provider>, String> {
        let key = config.api_key.ok_or_else(|| {
            format!(
                "no API key for provider '{}': set its api_key or the {} env var",
                config.provider_name, config.env_var
            )
        })?;
        Ok(Arc::new(
            AnthropicProvider::new(key).with_base_url(config.endpoint),
        ))
    }
}
