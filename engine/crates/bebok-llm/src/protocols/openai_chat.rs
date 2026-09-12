use std::sync::Arc;

use crate::provider::{AdapterConfig, ProviderAdapter};
use crate::{OpenAiProvider, Provider};

#[derive(Debug)]
pub struct OpenAiChatAdapter;

impl ProviderAdapter for OpenAiChatAdapter {
    fn type_name(&self) -> &'static str {
        "OpenAiChatAdapter"
    }

    fn build(&self, config: AdapterConfig) -> Result<Arc<dyn Provider>, String> {
        Ok(Arc::new(
            OpenAiProvider::new(config.api_key, config.endpoint).with_headers(config.headers),
        ))
    }
}
