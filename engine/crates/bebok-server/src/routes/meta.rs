//! Meta routes: agents, plugins, docker probe, provider models.
//!
//! Future extension point for Task 5 (sidebar/topbar): new introspection
//! endpoints register here without touching the route table shape in
//! `routes/mod.rs`.

use axum::extract::{Query, State};
use axum::Json;
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::config;
use bebok_core::error::CoreError;
use bebok_core::{Runtimes, check_docker};

use crate::error::{ApiError, err_response};
use crate::routes::common::DirectoryQuery;
use crate::state::AppState;

/// `GET /models?directory=&provider=` query.
#[derive(Deserialize)]
pub struct ModelsQuery {
    pub directory: String,
    pub provider: String,
}

/// `GET /agent?directory=` -> agent presets (built-ins + file presets).
pub async fn list_agents(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let agents = instance.agent_infos();
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// `GET /plugins` -> registered plugins + exposed hook points (introspection
/// for the plugin system: shows what is plugged in and where it can hook).
pub async fn list_plugins() -> Json<serde_json::Value> {
    let host = bebok_core::PluginHost::global();
    Json(serde_json::json!({
        "plugins": host.names().await,
        "hooks": bebok_core::hook_names(),
        "attached": true,
    }))
}

/// `GET /docker?directory=` -> Docker access probe (resolves `runtimes.docker`).
pub async fn check_docker_endpoint(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let runtimes = Runtimes::from_config(&instance.config_snapshot().runtimes);
    let status = check_docker(&runtimes.docker).await;
    Ok(Json(serde_json::json!({ "docker": status })))
}

/// `GET /models?directory=&provider=` -> list a provider's available models and
/// persist them into the **project** config `.bebok/config.json` for the
/// instance (`<directory>`), so `GET /config` and the GUI's model selects see
/// them (the project layer is authoritative over the global config). This is
/// the GUI's "check available models" button.
pub async fn list_models(
    State(state): State<AppState>,
    Query(q): Query<ModelsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let cfg = instance.config_snapshot();
    let spec = cfg.provider_spec(&q.provider).ok_or_else(|| {
        ApiError::not_found(format!("unknown provider '{}'", q.provider)).into_response()
    })?;

    let models = bebok_llm::list_models(&spec).await.map_err(|e| {
        ApiError::bad_gateway(format!(
            "failed to list models for '{}': {e}",
            q.provider
        ))
        .into_response()
    })?;

    // Persist the fetched models into the instance's project config providers
    // list, then reload so `GET /config` reflects them for the model selects.
    config::save_provider_models(&instance.root, &q.provider, spec.kind, &models)
        .map_err(|e| err_response(&CoreError::Other(e)))?;
    state
        .store
        .reload_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))
        .map(|_| ())?;

    Ok(Json(serde_json::json!({ "provider": q.provider, "models": models })))
}
