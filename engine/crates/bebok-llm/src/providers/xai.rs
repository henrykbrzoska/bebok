use crate::ProviderSpec;
use crate::provider::AdapterConfig;

pub(super) fn config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    super::openai_config(spec, api_key)
}
