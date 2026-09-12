//! Thin vendor configuration layered over protocol adapters.

mod deepseek;
mod groq;
mod ollama;
mod openrouter;
mod qwen;
mod xai;

use serde_json::Value;

use crate::ProviderSpec;
use crate::provider::AdapterConfig;

pub fn adapter_config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    match spec.name.as_str() {
        "xai" => xai::config(spec, api_key),
        "deepseek" => deepseek::config(spec, api_key),
        "groq" => groq::config(spec, api_key),
        "qwen" => qwen::config(spec, api_key),
        "ollama" => ollama::config(spec, api_key),
        "openrouter" => openrouter::config(spec, api_key),
        _ => generic_config(spec, api_key),
    }
}

fn generic_config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    let mut config = AdapterConfig {
        provider_name: spec.name.clone(),
        env_var: spec.env_var(),
        endpoint: spec.chat_url(),
        api_key,
        headers: Vec::new(),
    };
    if spec.name == "openai" {
        add_header(&mut config, spec, "organization", "OpenAI-Organization");
        add_header(&mut config, spec, "project", "OpenAI-Project");
    }
    config
}

fn openai_config(spec: &ProviderSpec, api_key: Option<String>) -> AdapterConfig {
    generic_config(spec, api_key)
}

fn add_header(config: &mut AdapterConfig, spec: &ProviderSpec, key: &str, header: &str) {
    if let Some(Value::String(value)) = spec.extra.get(key)
        && !value.trim().is_empty()
    {
        config.headers.push((header.to_string(), value.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin_provider_specs;

    #[test]
    fn openai_compatible_vendors_share_one_adapter_type() {
        let specs = builtin_provider_specs();
        let names = ["xai", "deepseek", "groq", "qwen", "ollama", "openrouter"];
        let entries: Vec<_> = names
            .iter()
            .map(|name| {
                let spec = specs.iter().find(|spec| spec.name == *name).unwrap();
                (
                    crate::adapter_for(spec.kind).type_name(),
                    adapter_config(spec, Some(format!("{name}-key"))),
                )
            })
            .collect();

        assert!(
            entries
                .iter()
                .all(|(adapter, _)| *adapter == "OpenAiChatAdapter")
        );
        for ((_, config), name) in entries.iter().zip(names) {
            assert_eq!(config.provider_name, name);
        }
        let endpoints: std::collections::HashSet<_> =
            entries.iter().map(|(_, config)| &config.endpoint).collect();
        assert_eq!(endpoints.len(), names.len());
        assert_eq!(entries[4].1.api_key, None, "Ollama is keyless");
    }

    #[test]
    fn provider_extras_become_protocol_headers() {
        let mut openai = builtin_provider_specs()
            .into_iter()
            .find(|spec| spec.name == "openai")
            .unwrap();
        openai
            .extra
            .insert("organization".into(), Value::String("org-1".into()));
        let config = adapter_config(&openai, Some("key".into()));
        assert!(
            config
                .headers
                .contains(&("OpenAI-Organization".into(), "org-1".into()))
        );

        let mut openrouter = builtin_provider_specs()
            .into_iter()
            .find(|spec| spec.name == "openrouter")
            .unwrap();
        openrouter
            .extra
            .insert("x_title".into(), Value::String("Bebok Desktop".into()));
        let config = adapter_config(&openrouter, Some("key".into()));
        assert!(
            config
                .headers
                .contains(&("X-Title".into(), "Bebok Desktop".into()))
        );
    }
}
