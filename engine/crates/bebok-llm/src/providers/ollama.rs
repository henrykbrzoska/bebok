use crate::ProviderSpec;
use crate::provider::AdapterConfig;

pub(super) fn config(spec: &ProviderSpec, _api_key: Option<String>) -> AdapterConfig {
    super::openai_config(spec, None)
}
