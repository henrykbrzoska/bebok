//! Provider catalog route.
//!
//! `GET /providers/catalog` serves the machine-readable descriptor of every
//! built-in provider (`ProviderUiSpec`): what it is called, which protocol and
//! credential style it uses, which environment variable holds its key, its
//! default base URL and its provider-specific extra fields. The Settings ->
//! Providers screen renders its form from this instead of hard-coding one flat
//! `kind/endpoint/api_key` row per provider — in particular it can hide the
//! API-key field entirely when `auth` is `none` (Ollama).
//!
//! The catalog is static (no directory/session context, no credentials): it
//! describes the *shape* of a provider's configuration, never its values.

use axum::Json;

/// `GET /providers/catalog` -> `{ "providers": [ProviderUiSpec, ...] }`.
///
/// Each entry: `{id, label, kind, auth, envVar, baseUrlDefault, extraFields[]}`.
pub async fn provider_catalog() -> Json<serde_json::Value> {
    let providers = bebok_llm::provider_catalog();
    Json(serde_json::json!({ "providers": providers }))
}

/// `GET /providers/cli` -> `{ "agents": [CliProbe, ...] }`: the agent CLIs
/// the engine can drive (1.8), each with whether it is installed here, its
/// version, the provider name it would register as and its default models.
/// Probing runs `<cli> --version` for every known CLI in parallel.
pub async fn cli_agents() -> Json<serde_json::Value> {
    let probes = futures::future::join_all(
        bebok_core::provider_cli::CliAgent::ALL
            .iter()
            .map(|agent| agent.probe(None)),
    )
    .await;
    Json(serde_json::json!({ "agents": probes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cli_agents_lists_every_known_cli_with_install_state() {
        let Json(body) = cli_agents().await;
        let agents = body["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 6);
        for a in agents {
            for key in [
                "id",
                "label",
                "command",
                "installed",
                "provider",
                "defaultModels",
            ] {
                assert!(a.get(key).is_some(), "missing {key}");
            }
            assert!(a["provider"].as_str().unwrap().starts_with("cli-"));
        }
    }

    /// The endpoint returns every built-in provider in the documented shape.
    #[tokio::test]
    async fn catalog_endpoint_returns_all_builtin_providers() {
        let Json(body) = provider_catalog().await;
        let providers = body["providers"].as_array().expect("providers array");
        assert_eq!(providers.len(), 11, "11 built-in providers today");

        for p in providers {
            for key in [
                "id",
                "label",
                "kind",
                "auth",
                "envVar",
                "baseUrlDefault",
                "extraFields",
            ] {
                assert!(p.get(key).is_some(), "{key} missing from {p}");
            }
            assert!(p["extraFields"].is_array());
            let kind = p["kind"].as_str().unwrap();
            assert!(kind == "openai" || kind == "anthropic", "kind: {kind}");
            let auth = p["auth"].as_str().unwrap();
            assert!(
                matches!(auth, "bearer" | "x-api-key" | "none"),
                "auth: {auth}"
            );
        }

        let ids: Vec<&str> = providers
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![
                "zai",
                "openai",
                "anthropic",
                "xai",
                "deepseek",
                "google",
                "mistralai",
                "groq",
                "qwen",
                "openrouter",
                "ollama",
            ]
        );
    }

    #[tokio::test]
    async fn catalog_endpoint_describes_openai_fully() {
        let Json(body) = provider_catalog().await;
        let providers = body["providers"].as_array().unwrap();
        let openai = providers.iter().find(|p| p["id"] == "openai").unwrap();
        assert_eq!(openai["label"], "OpenAI");
        assert_eq!(openai["kind"], "openai");
        assert_eq!(openai["auth"], "bearer");
        assert_eq!(openai["envVar"], "OPENAI_API_KEY");
        assert_eq!(openai["baseUrlDefault"], "https://api.openai.com/v1");
        let fields = openai["extraFields"].as_array().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["key"], "organization");
        assert_eq!(fields[0]["type"], "text");

        let anthropic = providers.iter().find(|p| p["id"] == "anthropic").unwrap();
        assert_eq!(anthropic["auth"], "x-api-key");
        assert_eq!(anthropic["kind"], "anthropic");
    }

    /// The payload must never leak credentials: it is a form descriptor.
    #[tokio::test]
    async fn catalog_endpoint_carries_no_secrets() {
        let Json(body) = provider_catalog().await;
        let raw = body.to_string();
        assert!(!raw.contains("api_key"));
        assert!(!raw.contains("apiKey"));
    }
}
