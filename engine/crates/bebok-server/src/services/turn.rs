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

use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_core::agent::TurnRunner;
use bebok_core::config::provider_from_model;
use bebok_core::error::CoreError;
use bebok_core::store::SessionState;
use bebok_llm::ProviderKind;

use super::provider_factory::{ProviderCtx, build_provider_for};
use crate::error::ApiError;
use crate::routes::session::PromptBody;
use crate::state::AppState;

/// RAII guard that owns the turn slot.
///
/// Constructed via [`TurnSlot::new`] (returns `None` if the session is
/// already busy). When dropped, the guard calls [`SessionState::end_turn`]
/// — so early `?` returns after the claim can never leak the slot.
/// Call [`TurnSlot::disarm`] to transfer ownership of the release to a
/// long-lived task (e.g. the `tokio::spawn`ed turn).
struct TurnSlot {
    session: Arc<SessionState>,
    armed: bool,
}

impl TurnSlot {
    /// Try to claim the turn slot. Returns `None` if busy.
    fn new(session: Arc<SessionState>) -> Option<Self> {
        if session.try_begin_turn() {
            Some(Self {
                session,
                armed: true,
            })
        } else {
            None
        }
    }

    /// Prevent the Drop impl from releasing the slot — ownership is
    /// transferred to the caller (typically the spawned background task).
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TurnSlot {
    fn drop(&mut self) {
        if self.armed {
            self.session.end_turn();
        }
    }
}

/// Best-effort drop guard held across `run_turn` inside the spawned task.
///
/// If `run_turn` panics this guard fires and releases the turn slot so the
/// session is not permanently wedged. On the normal (non-panic) path the
/// explicit `end_turn` + `clear_abort` run first and the guard is already
/// disarmed by being dropped *after* (or just a no-op second release).
///
/// `clear_abort` is async and cannot be called from a `Drop` impl, so we
/// only release the sync slot here — the normal path handles `clear_abort`.
struct ReleaseOnDrop {
    session: Arc<SessionState>,
    active: bool,
}

impl ReleaseOnDrop {
    fn new(session: Arc<SessionState>) -> Self {
        Self {
            session,
            active: true,
        }
    }

    /// Cancel the guard without releasing the slot (used on the happy path
    /// right before the explicit `end_turn` call).
    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if self.active {
            self.session.end_turn();
        }
    }
}

/// Orchestrate one prompt turn; returns the `202 running` payload.
pub async fn prompt_turn(
    state: &AppState,
    id: Uuid,
    body: PromptBody,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // A prompt sent while a CLI turn runs goes straight to the CLI when it can
    // take input (text only), instead of waiting for the turn to end.
    if body.images.is_empty() && body.files.is_empty() {
        let running = state.store.open_session(id).await.map_err(ApiError::from)?;
        if running.inject_cli_message(&body.message) {
            let idx = running
                .append_user_message(&body.message)
                .await
                .map_err(ApiError::from)?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "sessionID": id.to_string(),
                    "messageIndex": idx,
                    "status": "injected",
                })),
            ));
        }
        // A native turn takes it at its next step boundary (after the tool
        // calls in flight finish) — the loop appends the user message itself.
        if running.push_steer(&body.message) {
            return Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "sessionID": id.to_string(),
                    "status": "injected",
                })),
            ));
        }
    }
    // Validate image attachments before claiming the turn slot.
    let image_parts = validate_images(body.images)?;
    let mut attachment_parts = image_parts.clone();
    attachment_parts.extend(
        bebok_core::agent::validate_files(&body.files)
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
    );
    let session = state.store.open_session(id).await.map_err(ApiError::from)?;

    // One turn per session: synchronously claim the slot -> 409 otherwise.
    // TurnSlot is an RAII guard: if any of the fallible steps below return
    // early the slot is automatically released.
    let mut slot = TurnSlot::new(session.clone())
        .ok_or_else(|| ApiError::conflict("session busy: a turn is already running"))?;

    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(ApiError::from)?;
    let cfg = instance.config_snapshot();
    let meta = session.meta_snapshot().await;

    // The user may switch the agent mid-chat: an explicit `agent` on the prompt
    // wins and is persisted for subsequent turns.
    if let Some(agent) = body.agent.as_deref().filter(|a| !a.trim().is_empty())
        && agent != meta.agent
    {
        session.set_agent(agent).await;
    }
    let effective_agent = body
        .agent
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .unwrap_or(&meta.agent);

    // Resolve the agent preset and the effective model (agent override ->
    // session override -> per-agent-type config -> resolved config).
    let mut agent = instance.resolve_agent(effective_agent);
    let prompt_model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    // F9-9: a toolbar pick is persisted on the session so `effective_model`
    // and later turns agree with what the user sees.
    if let Some(m) = prompt_model
        && meta.model.as_deref() != Some(m)
    {
        session.set_model(Some(m)).await;
    }
    let model = prompt_model.map(str::to_string).unwrap_or_else(|| {
        agent
            .model
            .clone()
            .or_else(|| meta.model.clone())
            .unwrap_or_else(|| cfg.model_for(&agent.name))
    });

    // Pre-flight vision capability check: a model known to reject image input
    // fails fast with a 400 *before* the user message is appended, so the
    // transcript never ends up with an orphaned, unanswerable attachment.
    // Unknown models are assumed capable (conservative deny-list).
    if !image_parts.is_empty() && !bebok_core::agent::model_supports_images(&model) {
        return Err(ApiError::bad_request(format!(
            "model '{model}' does not support image input: remove the attachment or switch to a vision-capable model"
        )));
    }

    // Batch models (:batch suffix) never touch chat/completions: submit the
    // prompt as a batch job (Option A fallback) and return 202 with the
    // batch id. The job runs alongside the session — the turn slot is
    // released by Drop (`slot` is never disarmed here), so the session
    // stays responsive while the batch runs for minutes/hours. Progress
    // and results surface via `batch.*` SSE events + the Batch panel.
    if bebok_llm::is_batch_model(&model) {
        // The user message is appended first so the transcript keeps the
        // prompt; then a marker assistant message points at the batch job.
        session
            .append_user_message_with_attachments(&body.message, attachment_parts)
            .await
            .map_err(ApiError::from)?;
        if session.set_title_if_empty(&body.message).await {
            session.touch().await;
        }
        let batch_id = super::batch::submit_batch_job(
            state,
            session.directory(),
            id,
            &model,
            vec![crate::routes::batch::BatchItemRequest {
                messages: vec![serde_json::json!({
                    "role": "user",
                    "content": body.message,
                })],
            }],
        )
        .await?;
        let marker_idx = session
            .append_assistant_text(
                effective_agent,
                &model,
                &format!(
                    "Sent as batch job {batch_id} — track progress and read the result in the Batch panel (batch.completed will arrive when the provider finishes)."
                ),
            )
            .await;
        session.touch().await;
        let bus = state.store.bus();
        bus.publish(
            bebok_core::event::Event::new("message.updated", session.directory(), &id.to_string())
                .with_properties(serde_json::json!({ "index": marker_idx })),
        );
        return Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "sessionID": id.to_string(),
                "status": "batch",
                "batchID": batch_id,
            })),
        ));
    }

    // CLI providers run inside the project, so they need its root as their cwd.
    //
    // A CLI provider's tool use happens in a child process, outside Bebok's
    // permission engine. The managed profile runs `bebok-server cli-hook <event>`
    // before/after every tool call, so the child needs this turn's hook
    // endpoint plus a token scoped to this session — issued here, revoked in the
    // turn's teardown.
    let hook = match cfg
        .provider_spec(&provider_from_model(&model))
        .map(|spec| spec.kind)
    {
        Some(ProviderKind::Cli) => crate::routes::cli_hook::issue_hook_env(&id.to_string()),
        _ => None,
    };
    let hook_token = hook.as_ref().map(|(token, _)| token.clone());
    let provider_ctx = match hook {
        Some((_, env)) => ProviderCtx::root(&instance.root).with_hook_env(env),
        None => ProviderCtx::root(&instance.root),
    };
    // Where a CLI reports its conversation key when the turn ends (see the
    // CLI session bookkeeping after `TurnRunner::run` below).
    let cli_sink: bebok_cli_agents::CliSessionSink =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let is_cli = matches!(
        cfg.provider_spec(&provider_from_model(&model))
            .map(|spec| spec.kind),
        Some(ProviderKind::Cli)
    );
    let provider_ctx = if is_cli {
        provider_ctx
            .with_session_sink(cli_sink.clone())
            .with_input_slot(session.cli_input_slot())
            .with_cli_allow_write(agent.cli_allow_write())
    } else {
        provider_ctx
    };
    let mut provider = build_provider_for(&cfg, &model, &provider_ctx).map_err(ApiError::from)?;

    // Extension point (Tasks 2/5/6): prompt assembly is isolated here so
    // config/plugin editable prompts (and future sidebar/topbar or custom-CSS
    // context) can override the system text without touching the handler. It
    // needs the built provider: a CLI transport changes which prompt sections
    // make sense (phase 03/07).
    assemble_prompt(&instance, &mut agent, &cfg, provider.transport());

    // Whether this turn carried image attachments (drives the error hint below).
    let images_attached = !image_parts.is_empty();

    // Append the user message, set the title, persist, emit.
    let user_idx = session
        .append_user_message_with_attachments(&body.message, attachment_parts)
        .await
        .map_err(ApiError::from)?;
    if session.set_title_if_empty(&body.message).await {
        session.touch().await;
    }

    // A CLI provider continues its own conversation when the record still
    // matches this turn (same provider, model, system prompt and profile, and
    // the transcript before the record is plain). The record is read after the
    // user message is appended, so the tail it selects includes this message.
    let system_hash = bebok_core::provider::system_hash(&agent.prompt);
    if is_cli {
        let messages = session.messages_snapshot().await;
        let hint = bebok_core::provider::cli_resume_hint(
            session.cli_session().await.as_ref(),
            &provider_from_model(&model),
            &model,
            &system_hash,
            &messages,
        );
        if hint.is_some() {
            let resumed_ctx = provider_ctx.clone().with_resume(hint);
            provider = build_provider_for(&cfg, &model, &resumed_ctx).map_err(ApiError::from)?;
        }
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

    // Transfer ownership of the turn slot to the background task. After
    // disarm, the Drop impl on `slot` becomes a no-op.
    slot.disarm();

    let task_state = session.clone();
    let store = state.store.clone();
    tokio::spawn(async move {
        // Hold the turn mutex for the whole turn (flag already claimed above).
        let _guard = task_state.turn.lock().await;

        // Safety net: if run_turn panics the turn slot is still released.
        let mut release = ReleaseOnDrop::new(task_state.clone());
        let provider_name = provider_from_model(&model);
        let turn_model = model.clone();
        let result = TurnRunner::new(
            task_state.clone(),
            agent,
            instance.tools.clone(),
            provider,
            instance.permission.clone(),
            store.bus(),
            abort.clone(),
            model.clone(),
        )
        .run()
        .await;

        // Remember the CLI conversation only for a turn that ended cleanly: an
        // error or abort may have left the CLI ahead of the transcript, and the
        // next turn then starts over with the full transcript.
        if is_cli {
            let report = cli_sink.lock().ok().and_then(|mut slot| slot.take());
            let record = match (&result, abort.is_cancelled(), report) {
                (
                    Ok(()),
                    false,
                    Some(bebok_cli_agents::CliSessionReport {
                        key: Some(key),
                        profile_hash,
                    }),
                ) => Some(bebok_core::session::CliSessionMeta {
                    key,
                    provider: provider_name.clone(),
                    model: turn_model.clone(),
                    system_hash: system_hash.clone(),
                    msg_index: task_state.messages_snapshot().await.len(),
                    profile_hash: profile_hash.unwrap_or_default(),
                }),
                _ => None,
            };
            task_state.set_cli_session(record).await;
        }

        // Happy path: disarm the panic guard, then do the normal teardown
        // (clear_abort is async so it can't live in Drop).
        release.disarm();
        task_state.clear_abort().await;
        task_state.end_turn();
        // The CLI is gone (the provider stream owns the child and kills it on
        // drop), so its hook token has no further use.
        if let Some(token) = &hook_token {
            crate::routes::cli_hook::revoke_hook_token(token);
        }
        // Always publish an explicit idle transition. Clients treat bare
        // `session.updated` events as unrelated metadata changes and may still
        // see `running=true` if teardown is the only completion signal.
        bus.publish(
            bebok_core::event::Event::new(
                "session.updated",
                task_state.directory(),
                &id.to_string(),
            )
            .with_properties(serde_json::json!({
                "running": false,
                "aborted": abort.is_cancelled(),
            })),
        );

        if let Err(e) = result {
            if abort.is_cancelled() {
                tracing::info!("turn aborted for session {id}: {e}");
            } else {
                tracing::error!("turn failed for session {id}: {e}");
                // A turn that carried images can fail because the model has no
                // vision support; say so explicitly instead of surfacing only
                // the raw provider error.
                let surfaced = if images_attached {
                    format!(
                        "{e} — the user message included image attachments; model '{model}' \
                         may not support image input. Retry without the attachment or switch \
                         to a vision-capable model."
                    )
                } else {
                    format!("{e}")
                };
                // Unstick GUI clients: the normal end-of-turn `session.updated`
                // never fires on this path (SPEC §3.11 fan-out).
                bus.publish(
                    bebok_core::event::Event::new(
                        "session.updated",
                        task_state.directory(),
                        &id.to_string(),
                    )
                    .with_properties(serde_json::json!({ "error": surfaced })),
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
///
fn assemble_prompt(
    instance: &bebok_core::Instance,
    agent: &mut bebok_core::agent::Agent,
    cfg: &bebok_core::config::ResolvedConfig,
    transport: bebok_llm::Transport,
) {
    let is_cli_without_bebok = matches!(
        transport,
        bebok_llm::Transport::AgenticCli {
            bebok_tools: false,
            ..
        }
    );
    let is_cli = transport.is_cli();

    // AGENTS.md / skills — via profile for CLI without bebok_tools (faza 07).
    if !is_cli_without_bebok {
        let mut discovered = bebok_core::skills::discover(&instance.root);
        bebok_core::skills::apply_toggles(&mut discovered, Some(&cfg.skills));
        let instructions = bebok_core::skills::assemble_prompt(&discovered);
        if !instructions.is_empty() {
            agent.prompt = format!("{}\n\n{instructions}", agent.prompt);
        }
    }

    // Surface mid-chat environment changes (MCP/skill/yolo toggles) to the model.
    let notes = instance.take_context_notes();
    if !notes.is_empty() {
        let mut block = "Recent environment changes during this conversation:\n".to_string();
        for note in &notes {
            block.push_str(&format!("- {note}\n"));
        }
        agent.prompt = format!("{}\n\n{block}", agent.prompt);
    }

    // Host OS / bash — omitted for CLI without bebok_tools.
    if !is_cli_without_bebok {
        agent.prompt = format!(
            "{}\n\n{}",
            agent.prompt,
            bebok_core::agent::host_os_note_for_transport(transport)
        );
    }

    // WP-AUTOVERIFY — omitted for CLI without bebok_tools.
    if !is_cli_without_bebok
        && let Some(section) =
            bebok_core::agent::verification_section_for_transport(cfg, agent, transport)
    {
        tracing::debug!(
            agent = %agent.name,
            mode = cfg.frontend_verify().as_str(),
            "verification capabilities section added to the system prompt"
        );
        agent.prompt = format!("{}\n\n{section}", agent.prompt);
    }

    // `verify.buildTest` policy — ALWAYS (policy, not tool).
    if let Some(section) = bebok_core::agent::build_test_section(cfg) {
        tracing::debug!(
            mode = cfg.build_test_mode().as_str(),
            "build & test policy section added to the system prompt"
        );
        agent.prompt = format!("{}\n\n{section}", agent.prompt);
    }

    // Pre-computed code map — omitted for CLI without bebok_tools.
    if !is_cli_without_bebok
        && let Some(section) =
            bebok_core::agent::code_map_section_for_transport(&instance.root, cfg, transport)
    {
        tracing::debug!("code map section added to the system prompt");
        agent.prompt = format!("{}\n\n{section}", agent.prompt);
    }

    // Module dependency graph — omitted for CLI without bebok_tools.
    if !is_cli_without_bebok
        && let Some(section) =
            bebok_core::agent::code_graph_section_for_transport(&instance.root, cfg, transport)
    {
        tracing::debug!("code graph section added to the system prompt");
        agent.prompt = format!("{}\n\n{section}", agent.prompt);
    }

    // Fleet-only delegation: never for CLI (no fleet/task on CLI).
    if is_cli {
        return;
    }
    // Fleet-only delegation: the orchestrator note (fleet roster, spawn rule,
    // cap, supervision tools), main-thread sessions only; the text lives in
    // `bebok_core::agent::delegation_policy`, and the `Fleet:` paragraph is
    // rendered from the *resolved config* so the model sees the members this
    // project actually has (or that the fleet is off) rather than a generic
    // description it would have to guess names from. The roster is only
    // advertised when the `fleet` tool is actually registered — otherwise the
    // prompt would promise fan-out the model cannot call.
    let fleet = bebok_core::agent::FleetContext::from_config_and_tools(
        cfg,
        &agent.name,
        false,
        instance.tools.get("fleet").is_some(),
    );
    if let Some(policy) = bebok_core::agent::delegation_policy_note(&cfg.delegation, &fleet) {
        agent.prompt = format!("{}\n\n{policy}", agent.prompt);
    }
}

// Keep the error import used in both cfg paths (avoids unused warnings where
// CoreError only flows through `ApiError::from`).
#[allow(dead_code)]
fn _keep_core_error(_: &CoreError) {}

/// Image limits live in `bebok_core::agent::images` (single source of truth,
/// shared with the `task`/`fleet` tools); this module only adapts them to
/// the HTTP layer (`ImageInput` -> `AgentImageInput`, `String` -> `ApiError`).
/// Validate prompt image attachments -> session `Part::Image` list.
///
/// Delegates to the shared `bebok_core::agent::images` validator (single
/// source of truth with the `task`/`fleet` tools); maps the error to 400.
pub fn validate_images(
    images: Vec<crate::routes::session::ImageInput>,
) -> Result<Vec<bebok_core::session::Part>, ApiError> {
    let adapted = images
        .into_iter()
        .map(|img| bebok_core::agent::images::AgentImageInput {
            media_type: img.media_type,
            data: img.data,
            name: img.name,
        })
        .collect();
    bebok_core::agent::images::validate_agent_images(adapted).map_err(ApiError::bad_request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::session::ImageInput;

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    fn img(media_type: &str, data: &str) -> ImageInput {
        ImageInput {
            media_type: media_type.to_string(),
            data: data.to_string(),
            name: None,
        }
    }

    fn msg(err: ApiError) -> String {
        match err {
            ApiError::BadRequest(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Forbidden(m)
            | ApiError::Gone(m)
            | ApiError::BadGateway(m)
            | ApiError::ServiceUnavailable(m)
            | ApiError::Internal(m)
            | ApiError::Unauthorized(m) => m,
        }
    }

    #[test]
    fn five_images_ok_sixth_is_400() {
        let five: Vec<ImageInput> = (0..5).map(|_| img("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=")).collect();
        assert_eq!(
            validate_images(five)
                .unwrap_or_else(|e| panic!("{}", msg(e)))
                .len(),
            5
        );
        let six: Vec<ImageInput> = (0..6).map(|_| img("image/png", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=")).collect();
        let err = match validate_images(six) {
            Ok(_) => panic!("expected too-many-images error"),
            Err(e) => msg(e),
        };
        assert!(err.contains("max 5 images"), "{err}");
    }

    #[test]
    fn bad_mime_is_400() {
        let err = match validate_images(vec![img(
            "image/bmp",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
        )]) {
            Ok(_) => panic!("expected bad-mime error"),
            Err(e) => msg(e),
        };
        assert!(err.contains("unsupported image media_type"), "{err}");
    }

    #[test]
    fn data_url_prefix_stripped() {
        let raw = ImageInput {
            media_type: String::new(),
            data: format!("data:image/png;base64,{PNG_1X1}"),
            name: None,
        };
        let out = validate_images(vec![raw]).unwrap_or_else(|e| panic!("{}", msg(e)));
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            bebok_core::session::Part::Image { media_type, data, .. }
            if media_type == "image/png" && data == PNG_1X1
        ));
    }

    /// The `Fleet:` roster the main agent sees is rendered from the *resolved
    /// config*, not from the static preset text (which cannot know the member
    /// labels), and an agent that does not carry the `fleet` tool never sees
    /// the section at all. This is the `assemble_prompt` wiring guard.
    /// Fleet-first: the same config yields the roster + prefer-fleet spawn
    /// route whether or not the prompt carried the legacy `fleet: true` flag.
    #[tokio::test]
    async fn assembled_prompt_carries_the_configured_fleet_roster() {
        let base = std::env::temp_dir().join(format!("bebok-fleet-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = bebok_core::InstanceStore::with_data_dir(base.join("data"));
        bebok_core::config::store::write_delta_at(
            &store.config_db_path(),
            &bebok_core::config::project_scope(&project),
            &bebok_core::config::jsonc::parse(r#"{
                 "delegation": { "mode": "auto" },
                 "fleet": { "enabled": true, "members": [
                     { "name": "ask-openai-gpt-4.1-nano", "agent": "ask", "model": "openai/gpt-4.1-nano" },
                     { "name": "code-zai-glm-5.3-flash", "agent": "code", "model": "zai/glm-5.3-flash" }
                 ]}
               }"#).unwrap(),
        )
        .unwrap();
        let dir = project.to_string_lossy().to_string();
        let instance = store.get_or_create_instance(&dir).await.unwrap();
        let cfg = instance.config_snapshot();

        // Run as fleet: roster + prefer-fleet spawn route.
        let mut orchestrator = bebok_core::agent::Agent::orchestrator();
        assemble_prompt(
            &instance,
            &mut orchestrator,
            &cfg,
            bebok_llm::Transport::Http,
        );
        let prompt = orchestrator.prompt.clone();
        assert!(prompt.contains("## Delegation (fleet only,"), "{prompt}");
        assert!(prompt.contains("Fleet: enabled here with"), "{prompt}");
        assert!(
            prompt
                .contains("- `ask-openai-gpt-4.1-nano` - agent `ask`, model `openai/gpt-4.1-nano`"),
            "{prompt}"
        );
        assert!(prompt.contains("- `code-zai-glm-5.3-flash`"), "{prompt}");
        // The spawn bullet only offers the fleet route when it was requested
        // for this prompt and can run.
        assert!(prompt.contains("ONLY through `fleet`"), "{prompt}");

        // Same project, legacy flag off: the roster + prefer-fleet route are
        // still shown (fleet-first: usability alone decides).
        let mut solo = bebok_core::agent::Agent::orchestrator();
        assemble_prompt(&instance, &mut solo, &cfg, bebok_llm::Transport::Http);
        assert!(
            solo.prompt.contains("Fleet: enabled here with"),
            "{}",
            solo.prompt
        );
        assert!(
            solo.prompt.contains("ONLY through `fleet`"),
            "{}",
            solo.prompt
        );

        // `code` is a main-thread agent as well, but it has no `fleet` tool.
        let mut code = bebok_core::agent::Agent::code();
        assemble_prompt(&instance, &mut code, &cfg, bebok_llm::Transport::Http);
        assert!(!code.prompt.contains("fleet"), "{}", code.prompt);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Assembled prompt section order: preset → skills → context notes →
    /// Host OS → Verification capabilities → Build & test policy →
    /// code map → code graph → Delegation. The "Code navigation" section
    /// is appended later by `request.rs::apply_request_hook`, so it must
    /// NOT be present yet at this stage.
    #[tokio::test]
    async fn assembled_prompt_section_order() {
        let base = std::env::temp_dir().join(format!("bebok-order-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = bebok_core::InstanceStore::with_data_dir(base.join("data"));
        bebok_core::config::store::write_delta_at(
            &store.config_db_path(),
            &bebok_core::config::project_scope(&project),
            &bebok_core::config::jsonc::parse(
                r#"{
                 "verify": { "frontend": "auto", "buildTest": "auto" },
                 "fleet": { "enabled": true, "members": [
                     { "name": "ask-a", "agent": "ask", "model": "openai/gpt-4.1-nano" }
                 ]}
               }"#,
            )
            .unwrap(),
        )
        .unwrap();
        let dir = project.to_string_lossy().to_string();
        let instance = store.get_or_create_instance(&dir).await.unwrap();
        let cfg = instance.config_snapshot();

        let mut orchestrator = bebok_core::agent::Agent::orchestrator();
        assemble_prompt(
            &instance,
            &mut orchestrator,
            &cfg,
            bebok_llm::Transport::Http,
        );
        let prompt = orchestrator.prompt.clone();

        let host = prompt.find("Host OS: ").expect("Host OS note");
        let verification = prompt
            .find("Verification capabilities")
            .expect("verification section");
        let build_test = prompt
            .find("Build & test policy")
            .expect("build & test section");
        let delegation = prompt.find("## Delegation").expect("delegation note");
        assert!(
            host < verification,
            "Host OS must precede Verification capabilities"
        );
        assert!(
            verification < build_test,
            "Verification capabilities must precede Build & test policy"
        );
        assert!(
            build_test < delegation,
            "Build & test policy must precede Delegation"
        );
        assert!(
            !prompt.contains("Code navigation"),
            "Code navigation is appended by apply_request_hook, not assemble_prompt"
        );
        // Manual-preview guard (krok 6): exactly one of each section, and no
        // hard-coded absolute project root anywhere in the assembled prompt.
        assert_eq!(prompt.matches("Host OS: ").count(), 1, "one Host OS note");
        assert_eq!(
            prompt.matches("Verification capabilities").count(),
            1,
            "one verification section"
        );
        assert_eq!(
            prompt.matches("Build & test policy").count(),
            1,
            "one build & test section"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A legacy `fleet: true` prompt flag is ignored (unknown fields are
    /// accepted but have no effect): sub-agents are chosen only from the
    /// fleet list for the orchestrator.
    #[test]
    fn prompt_body_ignores_legacy_fleet_flag() {
        let plain: crate::routes::session::PromptBody =
            serde_json::from_str(r#"{"message":"hi"}"#).unwrap();
        assert_eq!(plain.message, "hi");
        let legacy: crate::routes::session::PromptBody =
            serde_json::from_str(r#"{"message":"hi","fleet":true}"#).unwrap();
        assert_eq!(legacy.message, "hi");
    }
}
