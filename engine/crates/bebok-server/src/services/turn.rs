//! Turn service: prompt orchestration (the `POST /session/{id}/prompt`
//! body moved out of the route handler).
//!
//! Flow: open session -> claim turn slot (409 when busy) -> resolve agent +
//! model -> assemble prompt (skills + context notes) -> build provider ->
//! append user message -> spawn the background turn task -> return `202`.
//!
//! Prompt-text assembly stays exactly as before; this module is the future
//! seam for config/plugin editable prompts (Task 2): overrides hook in at
//! `assemble_prompt` without touching the handler.

use axum::http::StatusCode;
use axum::Json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_core::agent::run_turn;
use bebok_core::error::CoreError;

use super::provider_factory::build_provider;
use crate::error::ApiError;
use crate::routes::session::PromptBody;
use crate::state::AppState;

/// Orchestrate one prompt turn; returns the `202 running` payload.
pub async fn prompt_turn(
    state: &AppState,
    id: Uuid,
    body: PromptBody,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(ApiError::from)?;

    // One turn per session: synchronously claim the slot -> 409 otherwise.
    if !session.try_begin_turn() {
        return Err(ApiError::conflict("session busy: a turn is already running"));
    }

    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(ApiError::from)?;
    let cfg = instance.config_snapshot();
    let meta = session.meta_snapshot().await;

    // The user may switch the agent mid-chat: an explicit `agent` on the prompt
    // wins and is persisted for subsequent turns.
    if let Some(agent) = body.agent.as_deref().filter(|a| !a.trim().is_empty()) {
        if agent != meta.agent {
            session.set_agent(agent).await;
        }
    }
    let effective_agent = body
        .agent
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .unwrap_or(&meta.agent);

    // Resolve the agent preset and the effective model (agent override ->
    // session override -> per-agent-type config -> resolved config).
    let mut agent = instance.resolve_agent(effective_agent);
    let model = body
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            agent
                .model
                .clone()
                .or_else(|| meta.model.clone())
                .unwrap_or_else(|| cfg.model_for(&agent.name))
        });

    // Extension point (Tasks 2/5/6): prompt assembly is isolated here so
    // config/plugin editable prompts (and future sidebar/topbar or custom-CSS
    // context) can override the system text without touching the handler.
    assemble_prompt(&instance, &mut agent, &cfg);

    let provider = build_provider(&cfg, &model).map_err(ApiError::from)?;

    // Append the user message, set the title, persist, emit.
    let user_idx = session
        .append_user_message(&body.message)
        .await
        .map_err(ApiError::from)?;
    if session.set_title_if_empty(&body.message).await {
        session.touch().await;
    }

    let bus = state.store.bus();
    let abort = CancellationToken::new();
    session.set_abort(abort.clone()).await;

    // Announce the turn start so clients can show a global "working" indicator
    // even when the chat view is in the background.
    bus.publish(
        bebok_core::event::Event::new("session.updated", session.directory(), &id.to_string())
            .with_properties(serde_json::json!({ "running": true })),
    );

    let task_state = session.clone();
    let store = state.store.clone();
    tokio::spawn(async move {
        // Hold the turn mutex for the whole turn (flag already claimed above).
        let _guard = task_state.turn.lock().await;
        let result = run_turn(
            task_state.clone(),
            agent,
            instance.tools.clone(),
            provider,
            instance.permission.clone(),
            bus.clone(),
            abort.clone(),
            &model,
        )
        .await;
        task_state.clear_abort().await;
        task_state.end_turn();
        if let Err(e) = result {
            if abort.is_cancelled() {
                tracing::info!("turn aborted for session {id}: {e}");
            } else {
                tracing::error!("turn failed for session {id}: {e}");
                // Unstick GUI clients: the normal end-of-turn `session.updated`
                // never fires on this path (SPEC §3.11 fan-out).
                let _ = bus.publish(
                    bebok_core::event::Event::new(
                        "session.updated",
                        task_state.directory(),
                        &id.to_string(),
                    )
                    .with_properties(serde_json::json!({ "error": format!("{e}") })),
                );
            }
        }
        let _ = store; // keep the store alive for the turn
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "sessionID": id.to_string(),
            "messageIndex": user_idx,
            "status": "running",
        })),
    ))
}

/// Assemble AGENTS.md + enabled skills + mid-chat environment notes into the
/// agent's system prompt (verbatim move of the previous inline block).
fn assemble_prompt(
    instance: &bebok_core::Instance,
    agent: &mut bebok_core::agent::Agent,
    cfg: &bebok_core::config::ResolvedConfig,
) {
    // Assemble AGENTS.md + enabled skills into the system prompt.
    let mut discovered = bebok_core::skills::discover(&instance.root);
    bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
    let instructions = bebok_core::skills::assemble_prompt(&discovered);
    if !instructions.is_empty() {
        agent.prompt = format!("{}\n\n{instructions}", agent.prompt);
    }

    // Surface mid-chat environment changes (MCP/skill/yolo toggles) to the model.
    let notes = instance.take_context_notes();
    if !notes.is_empty() {
        let mut block =
            "Recent environment changes during this conversation:\n".to_string();
        for note in &notes {
            block.push_str(&format!("- {note}\n"));
        }
        agent.prompt = format!("{}\n\n{block}", agent.prompt);
    }
}

// Keep the error import used in both cfg paths (avoids unused warnings where
// CoreError only flows through `ApiError::from`).
#[allow(dead_code)]
fn _keep_core_error(_: &CoreError) {}
