//! Provider-model persistence: upsert fetched models into the `providers`
//! list of the `global` or a project config scope in `bebok.db`.

use std::path::Path;

use bebok_llm::ProviderKind;

use super::store::{self as config_store, GLOBAL_SCOPE, project_scope};
use super::writer::{write_global_delta, write_project_delta};

/// Persist a provider's known models into the **global** config scope
/// (`providers` list, upsert by name, preserving kind/endpoint/api_key).
pub fn save_provider_models_global(
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(GLOBAL_SCOPE, name, kind, models)?;
    write_global_delta(&delta)
}

/// Persist a provider's known models into a **project's** config scope
/// (upsert by name, preserving kind/endpoint/api_key). This is what the GUI's
/// "check available models" button calls for an instance/directory: the
/// project layer is authoritative, so the fetched models surface in
/// `GET /config` (global-only writes would be masked when a project
/// re-declares the provider).
pub fn save_provider_models(
    directory: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models(&project_scope(directory), name, kind, models)?;
    write_project_delta(directory, &delta)
}

/// [`save_provider_models_global`] against an explicit config database.
pub fn save_provider_models_global_at(
    db_path: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let delta = upsert_provider_models_in(db_path, GLOBAL_SCOPE, name, kind, models)?;
    config_store::write_delta_at(db_path, GLOBAL_SCOPE, &delta)
}

/// [`save_provider_models`] against an explicit config database.
pub fn save_provider_models_at(
    db_path: &Path,
    directory: &Path,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<(), String> {
    let scope = project_scope(directory);
    let delta = upsert_provider_models_in(db_path, &scope, name, kind, models)?;
    config_store::write_delta_at(db_path, &scope, &delta)
}

/// Read the `providers` array of `scope` and upsert `models` into the matching
/// provider (or append it, preserving kind/endpoint/api_key). Returns a config
/// `{ "providers": [...] }` delta.
fn upsert_provider_models_in(
    db_path: &Path,
    scope: &str,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<serde_json::Value, String> {
    let existing = config_store::read_scope_at(db_path, scope).unwrap_or_default();
    Ok(upsert_providers_value(
        existing.get("providers"),
        name,
        kind,
        models,
    ))
}

/// Same as [`upsert_provider_models_in`] against the default database.
fn upsert_provider_models(
    scope: &str,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> Result<serde_json::Value, String> {
    let existing = config_store::read_scope_db(scope).unwrap_or_default();
    Ok(upsert_providers_value(
        existing.get("providers"),
        name,
        kind,
        models,
    ))
}

/// Pure upsert over the current `providers` value (no I/O).
fn upsert_providers_value(
    current: Option<&serde_json::Value>,
    name: &str,
    kind: ProviderKind,
    models: &[String],
) -> serde_json::Value {
    use bebok_llm::ProviderSpec;

    let mut providers: Vec<ProviderSpec> = current
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| serde_json::from_value(x.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    match providers.iter_mut().find(|p| p.name == name) {
        Some(existing) => {
            existing.models = models.to_vec();
        }
        // A new entry carries no credential: the key comes from the config
        // entry the user already has, an env var or the top-level api_key.
        None => providers.push(ProviderSpec {
            name: name.to_string(),
            kind,
            endpoint: None,
            api_key: None,
            models: models.to_vec(),
            ..Default::default()
        }),
    }

    serde_json::json!({ "providers": providers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("bebok-prov-{tag}-{}", uuid::Uuid::new_v4()));
        let dir = base.join("project");
        std::fs::create_dir_all(&dir).unwrap();
        let db = base.join("bebok.db");
        (base, dir, db)
    }

    #[test]
    fn global_upsert_appends_a_provider_without_a_key() {
        let (base, _dir, db) = tmp("global");
        save_provider_models_global_at(&db, "zai", ProviderKind::Openai, &["glm-5".into()])
            .unwrap();
        let obj = config_store::read_scope_at(&db, GLOBAL_SCOPE).unwrap();
        let providers = obj["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0]["name"], "zai");
        assert_eq!(providers[0]["models"][0], "glm-5");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn project_upsert_preserves_kind_endpoint_and_key_of_an_existing_entry() {
        let (base, dir, db) = tmp("project");
        let scope = project_scope(&dir);
        config_store::write_delta_at(
            &db,
            &scope,
            &json!({ "providers": [
                { "name": "zai", "kind": "openai", "endpoint": "https://x/v1",
                  "api_key": "real-key", "models": ["old"] }
            ] }),
        )
        .unwrap();

        save_provider_models_at(&db, &dir, "zai", ProviderKind::Openai, &["new".into()]).unwrap();

        let obj = config_store::read_scope_at(&db, &scope).unwrap();
        let providers = obj["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 1, "matching provider is updated in place");
        assert_eq!(providers[0]["models"][0], "new");
        assert_eq!(providers[0]["endpoint"], "https://x/v1");
        assert!(
            providers[0]["api_key"].is_string(),
            "existing credential survives"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn other_config_keys_are_untouched() {
        let (base, dir, db) = tmp("keep");
        let scope = project_scope(&dir);
        config_store::write_delta_at(&db, &scope, &json!({ "model": "zai/glm-5" })).unwrap();
        save_provider_models_at(&db, &dir, "zai", ProviderKind::Openai, &["glm-5".into()]).unwrap();
        let obj = config_store::read_scope_at(&db, &scope).unwrap();
        assert_eq!(obj.get("model").unwrap(), "zai/glm-5");
        assert!(obj.contains_key("providers"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
