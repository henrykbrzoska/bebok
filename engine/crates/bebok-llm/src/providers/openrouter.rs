use crate::ProviderSpec;
use crate::provider::AdapterConfig;

pub(super) fn config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    let mut config = super::openai_config(spec, api_key);
    super::add_header(&mut config, spec, "http_referer", "HTTP-Referer");
    super::add_header(&mut config, spec, "x_title", "X-Title");
    config
}
