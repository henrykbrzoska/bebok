//! Provider-model persistence: upsert fetched models into the
//! `providers` list of the global or project config file.

use std::path::Path;

use bebok_llm::ProviderKind;

use super::jsonc;
use super::loader::{global_config_path, project_config_path};
use super::writer::{write_global_delta, write_project_delta};

/// Persist a provider's known models into the **global** config `providers`
/// list (upsert by name, preserving kind/endpoint/api_key).
pub fn save_provider_models_global(
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(&global_config_path(), name, kind, models)?;
    write_global_delta(&delta)
}

/// Persist a provider's known models into the **project** config `providers`
/// list (upsert by name, preserving kind/endpoint/api_key). This is what the
/// GUI's "check available models" button calls for an instance/directory: the
/// project layer is authoritative, so the fetched models surface in `GET /config`
/// (global-only writes would be masked when a project re-declares the provider).
pub fn save_provider_models(
    directory: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(&project_config_path(directory), name, kind, models)?;
    write_project_delta(directory, &delta)
}

/// Read the `providers` array at `path` and upsert `models` into the matching
/// provider (or append it, preserving kind/endpoint/api_key). Returns a config
/// `{ "providers": [...] }` delta — the caller persists it to disk.
fn upsert_provider_models(
    path: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<serde_json::Value, String> {
    use bebok_llm::ProviderSpec;

    let mut providers: Vec<ProviderSpec> = match std::fs::read_to_string(path) {
        Ok(text) => match jsonc::parse(&text) {
            Ok(v) => v
                .get("providers")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| serde_json::from_value(x.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };

    match providers.iter_mut().find(|p| p.name == name) {
        Some(existing) => {
            existing.models = models.to_vec();
        }
        None => providers.push(ProviderSpec {
            name: name.to_string(),
            kind,
            endpoint: None,
            api_key: None,
            models: models.to_vec(),
        }),
    }

    Ok(serde_json::json!({ "providers": providers }))
}
