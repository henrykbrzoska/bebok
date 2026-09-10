//! Turn runner (Orchestrator / state machine): `TurnRunner` replaces the
//! 8-argument `run_turn(...)` with a struct holding
//! state/agent/tools/provider/permission/bus/abort/model.
//!
//! Loop: `build request -> before.request hook -> stream SSE -> append parts
//! -> permission gate (Strategy) -> execute (exec.rs) -> events (Observer)`.
//! The `run_turn` free function is kept as a thin delegating shim so existing
//! callers (`services/turn.rs`, tests) compile unchanged.

use std::path::Path;
use std::sync::Arc;

use bebok_llm::{Provider, StreamEvent};
use bebok_tools::ToolRegistry;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::exec::{ExecCtx, ToolOutcome, exec_gated_call, fail_tool};
use super::gate::{GateCtx, resolve_permission};
use super::observe::{emit_message, emit_part, emit_session};
use super::preset::Agent;
use super::request::RequestBuilder;
use crate::error::Result;
use crate::event::{Event, EventBus};
use crate::llm_trace::{LLM_TRACE, LlmCall, push_llm_call};
use crate::permission::{CompiledLayer, PermissionEngine};
use crate::plugin::{Hook, PluginHost, TurnHook};
use crate::session::{Message, Role};
use crate::store::SessionState;
use crate::util::now_ms;

/// Orchestrates one full turn (owns everything `run_turn` took as args).
pub struct TurnRunner {
    pub state: Arc<SessionState>,
    pub agent: Agent,
    pub tools: Arc<ToolRegistry>,
    pub provider: Arc<dyn Provider>,
    pub permission: Arc<PermissionEngine>,
    pub bus: EventBus,
    pub abort: CancellationToken,
    pub model: String,
}

impl TurnRunner {
    pub fn new(
        state: Arc<SessionState>,
        agent: Agent,
        tools: Arc<ToolRegistry>,
        provider: Arc<dyn Provider>,
        permission: Arc<PermissionEngine>,
        bus: EventBus,
        abort: CancellationToken,
        model: &str,
    ) -> Self {
        Self {
            state,
            agent,
            tools,
            provider,
            permission,
            bus,
            abort,
            model: model.to_string(),
        }
    }

    /// Run a full turn: build request -> stream SSE -> append parts -> pending
    /// tool calls -> permission gate (allow/ask/deny, M2) -> execute -> events.
    ///
    /// The turn lock must already be held by the caller (409 otherwise).
    pub async fn run(self) -> Result<()> {
        let Self {
            state,
            agent,
            tools,
            provider,
            permission,
            bus,
            abort,
            model,
        } = self;
        let config = state.config_snapshot();
        let hooks = PluginHost::global();

        // Agent permission overrides are evaluated before project/global rules.
        let agent_layer = if agent.permissions.is_empty() {
            None
        } else {
            Some(CompiledLayer::compile(&agent.permissions))
        };

        loop {
            if abort.is_cancelled() {
                // Persist a clear abort message so the transcript is informative.
                persist_abort_message(&state, &bus, &agent.name, &model).await;
                break;
            }

            let builder = RequestBuilder::new(
                &state,
                &agent,
                &tools,
                &model,
                config.max_tokens,
                config.thinking,
            );
            let mut req = builder.build().await?;
            // Plugin hook: inspect / mutate the request before it is sent.
            builder.apply_request_hook(&mut req).await;

            // ── LLM trace: capture the full request body before it is consumed ──
            let trace_ts = now_ms();
            let trace_model = model.clone();
            let trace_request: serde_json::Value = serde_json::to_value(&req)
                .unwrap_or_else(|_| serde_json::json!({"_serialize_error": true}));

            bus.publish(
                Event::new("debug.log", state.directory(), &state.id().to_string())
                    .with_properties(serde_json::json!({
                        "source": "llm",
                        "kind": "request",
                        "title": format!("llm {model}"),
                        "detail": format!(
                            "messages={} tools={} system_chars={} max_tokens={} thinking={}",
                            req.messages.len(),
                            req.tools.len(),
                            req.system.chars().count(),
                            req.max_tokens,
                            req.thinking.as_str()
                        ),
                    })),
            );
            let mut stream = match provider.stream(req).await {
                Ok(s) => s,
                Err(err) => {
                    // ── LLM trace: record the failed call ──
                    push_llm_call(LlmCall {
                        id: LLM_TRACE.next_id(),
                        ts: trace_ts,
                        model: trace_model,
                        request: trace_request,
                        response: serde_json::json!({ "error": err.to_string() }),
                    });

                    // A provider-level failure (e.g. a model that refuses tool
                    // use, invalid request, 5xx): note it in the project config so
                    // repeat offenders can be handled/filtered later.
                    crate::config::record_llm_error(
                        Path::new(state.directory()),
                        &model,
                        &err.to_string(),
                    );
                    bus.publish(
                        Event::new("debug.log", state.directory(), &state.id().to_string())
                            .with_properties(serde_json::json!({
                                "source": "llm",
                                "kind": "error",
                                "title": format!("llm {model}"),
                                "detail": err.to_string(),
                            })),
                    );
                    return Err(err.into());
                }
            };

            // Stream into a new assistant message (kept in the session so GET
            // /message reflects live state; persisted on part completion).
            let (assistant_idx, _assistant_id) = {
                let mut messages = state.messages.write().await;
                let msg = Message::assistant_with(&agent.name, &model);
                let id = msg.id;
                messages.push(msg);
                (messages.len() - 1, id)
            };
            state.note_message_index(assistant_idx);

            let mut saw_any = false;
            while let Some(ev) = stream.next().await {
                if abort.is_cancelled() {
                    break;
                }
                match ev? {
                    StreamEvent::Text(delta) => {
                        saw_any = true;
                        state
                            .append_to_part(assistant_idx, |m| m.append_text(&delta))
                            .await;
                        emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                    }
                    StreamEvent::Thinking(delta) => {
                        saw_any = true;
                        state
                            .append_to_part(assistant_idx, |m| m.append_thinking(&delta))
                            .await;
                        emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                    }
                    StreamEvent::ToolCall(call) => {
                        saw_any = true;
                        state
                            .append_to_part(assistant_idx, |m| {
                                m.add_tool_call(
                                    call.id.clone(),
                                    call.name.clone(),
                                    call.input.clone(),
                                )
                            })
                            .await;
                        emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                    }
                    StreamEvent::Done(usage) => {
                        let cost = bebok_llm::compute_cost(
                            &model,
                            usage.input_tokens,
                            usage.output_tokens,
                            usage.cache_read_input_tokens.unwrap_or(0),
                            usage.cache_creation_input_tokens.unwrap_or(0),
                        )
                        .or(usage.cost);
                        let cache_read = usage.cache_read_input_tokens;
                        let cache_write = usage.cache_creation_input_tokens;
                        state
                            .append_to_part(assistant_idx, |m| {
                                m.set_usage(crate::session::UsageTotals {
                                    input_tokens: usage.input_tokens,
                                    output_tokens: usage.output_tokens,
                                    cost,
                                    cache_read_input_tokens: cache_read,
                                    cache_creation_input_tokens: cache_write,
                                })
                            })
                            .await;
                        state
                            .add_usage(
                                usage.input_tokens,
                                usage.output_tokens,
                                cost,
                                cache_read,
                                cache_write,
                            )
                            .await;
                        emit_part(&bus, &state, "message.part.updated", assistant_idx).await;
                        bus.publish(
                            Event::new("debug.log", state.directory(), &state.id().to_string())
                                .with_properties(serde_json::json!({
                                    "source": "llm",
                                    "kind": "response",
                                    "title": format!("llm {model}"),
                                    "detail": format!(
                                        "in={} out={} cache_read={} cache_write={}",
                                        usage.input_tokens,
                                        usage.output_tokens,
                                        cache_read.unwrap_or(0),
                                        cache_write.unwrap_or(0)
                                    ),
                                })),
                        );
                    }
                }
                emit_message(&bus, &state, "message.updated", assistant_idx);
            }

            // Drop the stream so the HTTP connection closes promptly.
            drop(stream);

            // ── LLM trace: record the completed call ──
            {
                let messages = state.messages.read().await;
                if let Some(assistant_msg) = messages.get(assistant_idx) {
                    push_llm_call(LlmCall {
                        id: LLM_TRACE.next_id(),
                        ts: trace_ts,
                        model: trace_model,
                        request: trace_request,
                        response: serde_json::json!({
                            "model": model,
                            "message": assistant_msg,
                        }),
                    });
                }
            }

            if abort.is_cancelled() {
                // Persist whatever streamed so far (crash-safe transcript),
                // then add a clear abort marker.
                state.persist_message_at(assistant_idx).await;
                persist_abort_message(&state, &bus, &agent.name, &model).await;
                break;
            }

            if !saw_any {
                state.persist_message_at(assistant_idx).await;
                break;
            }

            // Persist the assistant message (parts completed -> flush to disk).
            state.persist_message_at(assistant_idx).await;
            emit_message(&bus, &state, "message.updated", assistant_idx);

            // Pending tool calls.
            let pending = {
                let messages = state.messages.read().await;
                messages
                    .get(assistant_idx)
                    .map(|m| m.pending_tool_calls())
                    .unwrap_or_default()
            };

            if pending.is_empty() {
                break; // final answer
            }

            for (call_id, tool_name, input) in pending {
                if abort.is_cancelled() {
                    break;
                }

                // Permission gate (M2): allow / deny / ask. `ask` may suspend the
                // loop until a client resolves the request (oneshot, no polling).
                let gate = GateCtx {
                    state: &state,
                    bus: &bus,
                    permission: &permission,
                    agent_layer: agent_layer.as_ref(),
                    tools: &tools,
                    abort: &abort,
                    agent_name: &agent.name,
                    assistant_idx,
                };
                match resolve_permission(&gate, &tool_name, &input).await {
                    ToolOutcome::Denied(message) => {
                        fail_tool(&state, &bus, assistant_idx, &call_id, message).await;
                        continue;
                    }
                    ToolOutcome::Aborted => {
                        fail_tool(&state, &bus, assistant_idx, &call_id, "aborted").await;
                        break;
                    }
                    ToolOutcome::Run => {}
                }

                let exec = ExecCtx {
                    state: &state,
                    bus: &bus,
                    tools: &tools,
                    permission: &permission,
                    provider: &provider,
                    agent: &agent,
                    abort: &abort,
                    assistant_idx,
                    tool_output_cap: config.tool_output_cap,
                };
                if !exec_gated_call(&exec, &call_id, &tool_name, &input).await {
                    break;
                }
            }
        }

        state.touch().await;

        // Plugin hook: turn finished successfully (errors return early above and
        // surface through `session.updated` events to the observers).
        if hooks.has_plugins().await {
            let mut payload = TurnHook {
                ok: !abort.is_cancelled(),
                messages: state.messages_snapshot().await.len(),
            };
            hooks.run_hook(Hook::TURN_END, &mut payload).await;
        }

        emit_session(&bus, &state, "session.updated");
        Ok(())
    }
}

/// Persist a clear "Turn aborted" assistant message so the transcript is
/// informative when a user (or child task abort) cancels the turn.
async fn persist_abort_message(
    state: &Arc<SessionState>,
    bus: &EventBus,
    agent_name: &str,
    model: &str,
) {
    // Only add an abort marker if there isn't already a recent assistant
    // message that ended with an abort indicator (avoid duplicates).
    let already_marked = {
        let messages = state.messages.read().await;
        messages
            .last()
            .map(|m| m.role == Role::Assistant && m.text_content().contains("[Turn aborted"))
            .unwrap_or(false)
    };
    if already_marked {
        return;
    }

    let idx = {
        let mut messages = state.messages.write().await;
        let mut msg = Message::assistant_with(agent_name, model);
        msg.append_text("[Turn aborted by user]");
        let idx = messages.len();
        messages.push(msg);
        idx
    };
    state.note_message_index(idx);
    state.persist_message_at(idx).await;
    emit_message(bus, state, "message.updated", idx);
}

/// Compat shim: the old 8-argument entry point delegates to `TurnRunner`.
pub async fn run_turn(
    state: Arc<SessionState>,
    agent: Agent,
    tools: Arc<ToolRegistry>,
    provider: Arc<dyn Provider>,
    permission: Arc<PermissionEngine>,
    bus: EventBus,
    abort: CancellationToken,
    model: &str,
) -> Result<()> {
    TurnRunner::new(state, agent, tools, provider, permission, bus, abort, model)
        .run()
        .await
}
