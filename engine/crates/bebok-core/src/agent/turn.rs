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
use std::time::Duration;

use bebok_llm::{Provider, StreamEvent};
use bebok_tools::ToolRegistry;
use futures::StreamExt;
use futures::future::join_all;
use tokio_util::sync::CancellationToken;

use super::exec::{ExecCtx, ToolOutcome, exec_gated_call, fail_tool};
use super::gate::{GateCtx, resolve_permission};
use super::observe::{emit_message, emit_part, emit_session};
use super::preset::Agent;
use super::request::RequestBuilder;
use crate::error::Result;
use crate::event::{Event, EventBus};
use crate::llm_trace::{begin_llm_call, complete_llm_call};
use crate::permission::{CompiledLayer, PermissionEngine};
use crate::plugin::{Hook, PluginHost, TurnHook};
use crate::session::{Message, Role};
use crate::store::SessionState;

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
    #[allow(clippy::too_many_arguments)]
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

            // ── LLM trace: publish the request immediately so the in-flight
            // (last) call is visible in GET /debug/log while streaming ──
            let trace_request: serde_json::Value = serde_json::to_value(&req)
                .unwrap_or_else(|_| serde_json::json!({"_serialize_error": true}));
            let trace_id = begin_llm_call(model.clone(), trace_request);

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
            // Transient provider failures (429, 5xx, dropped connection) used
            // to kill the whole turn on the first try. Retry the stream setup
            // with exponential backoff before giving up.
            let stream_started = stream_with_retry(
                &provider,
                &req,
                RetryPolicy::default(),
                &abort,
                |attempt, err, delay| {
                    bus.publish(
                        Event::new("debug.log", state.directory(), &state.id().to_string())
                            .with_properties(serde_json::json!({
                                "source": "llm",
                                "kind": "retry",
                                "title": format!("llm {model}"),
                                "detail": format!(
                                    "attempt {attempt} failed ({}): retrying in {} ms - {}",
                                    err.kind(),
                                    delay.as_millis(),
                                    err
                                ),
                            })),
                    );
                },
            )
            .await;
            let mut stream = match stream_started {
                Ok(s) => s,
                Err(err) => {
                    // ── LLM trace: finish the in-flight call with the error ──
                    complete_llm_call(
                        trace_id,
                        serde_json::json!({ "model": model, "error": err.to_string() }),
                    );

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
            let mut stream_err: Option<String> = None;
            while let Some(ev) = stream.next().await {
                if abort.is_cancelled() {
                    break;
                }
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(err) => {
                        stream_err = Some(err.to_string());
                        break;
                    }
                };
                match ev {
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
                        // Context meter (F6-3): the provider re-sends the whole
                        // transcript on every call, so the latest call's input
                        // size *is* the live context usage. Fall back to the
                        // chars/4 estimate when the provider reported nothing.
                        let context_used = crate::context::context_used_from_usage(&usage)
                            .unwrap_or_else(|| {
                                crate::context::estimate_chat(&req.messages, &req.system) as u64
                            });
                        state.set_context_used(context_used, &model).await;
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

            // ── LLM trace: finish the in-flight call with the response ──
            // (runs on every exit path — done, abort, stream error — so the
            // last request/response pair is never missing from the trace).
            {
                let messages = state.messages.read().await;
                if let Some(err) = stream_err {
                    complete_llm_call(
                        trace_id,
                        serde_json::json!({ "model": model, "error": err }),
                    );
                    bus.publish(
                        Event::new("debug.log", state.directory(), &state.id().to_string())
                            .with_properties(serde_json::json!({
                                "source": "llm",
                                "kind": "error",
                                "title": format!("llm {model}"),
                                "detail": err,
                            })),
                    );
                } else if let Some(assistant_msg) = messages.get(assistant_idx) {
                    complete_llm_call(
                        trace_id,
                        serde_json::json!({
                            "model": model,
                            "message": assistant_msg,
                        }),
                    );
                } else {
                    complete_llm_call(
                        trace_id,
                        serde_json::json!({ "model": model, "error": "no assistant message" }),
                    );
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

            // Independent read-only calls run concurrently (F3-11); everything
            // else stays strictly sequential and in model order.
            let batches = schedule_tool_calls(pending, &tools, MAX_PARALLEL_TOOL_CALLS);

            let mut stop = false;
            for batch in batches {
                if abort.is_cancelled() {
                    break;
                }

                // 1. Permission gate (M2): allow / deny / ask — always run one
                //    call at a time, in model order, so `ask` prompts never
                //    interleave and the session decision cache still collapses
                //    duplicates. `ask` may suspend the loop until a client
                //    resolves the request (oneshot, no polling).
                let mut runnable: Vec<PendingCall> = Vec::with_capacity(batch.len());
                for (call_id, tool_name, input) in batch {
                    if abort.is_cancelled() {
                        stop = true;
                        break;
                    }
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
                            stop = true;
                            break;
                        }
                        ToolOutcome::Run => runnable.push((call_id, tool_name, input)),
                    }
                }

                // 2. Execute. A one-element batch keeps the original inline
                //    path; a parallel batch dispatches all of its calls at once
                //    and awaits them together, so one slow read does not hold
                //    up the others. A failing call only fails its own tool part.
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
                if runnable.len() == 1 {
                    let (call_id, tool_name, input) = &runnable[0];
                    if !exec_gated_call(&exec, call_id, tool_name, input).await {
                        stop = true;
                    }
                } else if !runnable.is_empty() {
                    let results = join_all(
                        runnable
                            .iter()
                            .map(|(id, name, input)| exec_gated_call(&exec, id, name, input)),
                    )
                    .await;
                    if results.iter().any(|kept_going| !kept_going) {
                        stop = true;
                    }
                }

                if stop {
                    // Same as before F3-11: stop feeding tool calls and let the
                    // outer loop decide (an abort is handled at its top).
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

/// Upper bound on how many tool calls run concurrently inside one batch.
///
/// Deliberately small: the point is to overlap I/O latency (three `read_file`
/// calls on the same turn), not to saturate the machine. Every call still holds
/// the session lock briefly when it persists, so a large fan-out would only
/// move the contention.
pub const MAX_PARALLEL_TOOL_CALLS: usize = 4;

/// Prefix of every MCP-provided tool name (`mcp__<server>__<tool>`).
const MCP_TOOL_PREFIX: &str = "mcp__";

/// Tools that report `is_read_only() == true` but are *not* side-effect free.
///
/// `task` and `fleet` only *spawn* sub-agents — they are marked read-only so the
/// delegation itself is not an extra approval prompt, but what the sub-agent
/// then does can write files and run commands. Running two of them concurrently
/// from one batch would reorder those effects, so they stay sequential.
const NEVER_PARALLEL: &[&str] = &["task", "fleet"];

/// One pending tool call: `(call_id, tool_name, input)`.
pub type PendingCall = (String, String, serde_json::Value);

/// Whether one pending call is safe to run concurrently with its neighbours.
///
/// Conservative by design (safety over speed): a call qualifies only when the
/// tool is registered, classifies *this* invocation as read-only
/// ([`bebok_tools::Tool::is_read_only_for`], so `fetch` qualifies for GET but
/// not for POST), is not one of [`NEVER_PARALLEL`], and is not provided by an
/// MCP server — an MCP `readOnlyHint` is self-declared by a third-party process
/// and is not a strong enough guarantee to reorder calls on.
pub fn is_parallel_safe(tools: &ToolRegistry, tool_name: &str, input: &serde_json::Value) -> bool {
    if NEVER_PARALLEL.contains(&tool_name) || tool_name.starts_with(MCP_TOOL_PREFIX) {
        return false;
    }
    tools
        .get(tool_name)
        .map(|t| t.is_read_only_for(input))
        .unwrap_or(false)
}

/// Split one turn's pending tool calls into ordered execution batches.
///
/// A run of adjacent parallel-safe calls becomes one batch of at most
/// `max_parallel` entries; every other call becomes a batch of its own. Batches
/// run one after another, so a mutating call never overtakes (or is overtaken
/// by) a call the model emitted before it: only calls that neither write nor
/// execute anything ever share a batch.
pub fn schedule_tool_calls(
    pending: Vec<PendingCall>,
    tools: &ToolRegistry,
    max_parallel: usize,
) -> Vec<Vec<PendingCall>> {
    let cap = max_parallel.max(1);
    let mut batches: Vec<Vec<PendingCall>> = Vec::new();
    for call in pending {
        let parallel = cap > 1 && is_parallel_safe(tools, &call.1, &call.2);
        let extend = parallel
            && batches
                .last()
                .is_some_and(|b| b.len() < cap && is_parallel_safe(tools, &b[0].1, &b[0].2));
        if extend {
            // `extend` is only true when a last batch exists.
            batches.last_mut().expect("checked above").push(call);
        } else {
            batches.push(vec![call]);
        }
    }
    batches
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

/// How the turn retries a failed `provider.stream()` call.
///
/// Only the *setup* of the stream is retried: once bytes are flowing the reply
/// is partially rendered, so re-issuing the request would duplicate output.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts, including the first one (1 = no retries).
    pub max_attempts: u32,
    /// Delay after the first failure; doubles per attempt.
    pub base_delay: Duration,
    /// Ceiling for the computed (pre-`retry_after`) delay.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Delay before attempt `attempt + 1` (1-based `attempt` = the one that
    /// just failed): exponential backoff with equal jitter, capped at
    /// `max_delay`, and never shorter than a `retry_after` the provider sent.
    ///
    /// Equal jitter (half fixed, half random) keeps the backoff monotonic while
    /// still de-synchronising parallel agents that got rate-limited together.
    pub fn delay_for(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16))
            .min(self.max_delay);
        let half = exp / 2;
        let jittered = half + Duration::from_nanos(jitter_nanos(half.as_nanos() as u64));
        match retry_after {
            // The provider's hint is a floor, not a replacement: a server that
            // says "1s" while we already backed off 8s gets the 8s.
            Some(hint) => jittered.max(hint),
            None => jittered,
        }
    }
}

/// Pseudo-random value in `0..=span` nanoseconds, seeded from the clock.
///
/// Deliberately not a `rand` dependency: this only has to de-correlate retries
/// between processes, not be statistically sound.
fn jitter_nanos(span: u64) -> u64 {
    if span == 0 {
        return 0;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    // xorshift so nearby nanosecond values do not produce nearby jitter.
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x % (span + 1)
}

/// Run `op`, retrying transient provider failures with backoff.
///
/// Retryable is decided by [`bebok_llm::ProviderErrorKind::is_transient`] -
/// rate limits, 5xx and transport errors - so an auth error or an unknown model
/// fails immediately instead of burning three attempts on a certain failure.
/// A cancelled `abort` ends the wait (and the retrying) at once.
pub async fn with_retry<T, F, Fut, R>(
    policy: RetryPolicy,
    abort: &CancellationToken,
    mut op: F,
    mut on_retry: R,
) -> std::result::Result<T, bebok_llm::LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, bebok_llm::LlmError>>,
    R: FnMut(u32, &bebok_llm::LlmError, Duration),
{
    let attempts = policy.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= attempts || !err.is_transient() || abort.is_cancelled() {
                    return Err(err);
                }
                let delay = policy.delay_for(attempt, err.retry_after());
                on_retry(attempt, &err, delay);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = abort.cancelled() => return Err(err),
                }
                attempt += 1;
            }
        }
    }
}

/// Open the provider stream for `req`, retrying transient failures.
pub async fn stream_with_retry<R>(
    provider: &Arc<dyn Provider>,
    req: &bebok_llm::ChatRequest,
    policy: RetryPolicy,
    abort: &CancellationToken,
    on_retry: R,
) -> std::result::Result<
    futures::stream::BoxStream<'static, bebok_llm::StreamResult<bebok_llm::StreamEvent>>,
    bebok_llm::LlmError,
>
where
    R: FnMut(u32, &bebok_llm::LlmError, Duration),
{
    with_retry(
        policy,
        abort,
        || {
            let provider = Arc::clone(provider);
            let req = req.clone();
            async move { provider.stream(req).await }
        },
        on_retry,
    )
    .await
}

/// Compat shim: the old 8-argument entry point delegates to `TurnRunner`.
#[allow(clippy::too_many_arguments)]
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

#[cfg(test)]
mod parallel_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use async_trait::async_trait;
    use bebok_llm::{ChatRequest, ToolCall, Usage};
    use bebok_tools::{Tool, ToolCtx, ToolOutput, builtin_tools};
    use futures::stream::BoxStream;
    use serde_json::{Value, json};

    use super::*;
    use crate::agent::preset::Agent;
    use crate::store::InstanceStore;

    // ── scheduling ────────────────────────────────────────────────────

    fn registry() -> ToolRegistry {
        ToolRegistry::new(builtin_tools())
    }

    fn call(id: &str, name: &str, input: Value) -> PendingCall {
        (id.to_string(), name.to_string(), input)
    }

    fn read(id: &str, path: &str) -> PendingCall {
        call(id, "read_file", json!({ "path": path }))
    }

    fn shape(batches: &[Vec<PendingCall>]) -> Vec<Vec<&str>> {
        batches
            .iter()
            .map(|b| b.iter().map(|c| c.0.as_str()).collect())
            .collect()
    }

    /// The headline case: three independent reads become one batch.
    #[test]
    fn read_only_calls_share_one_batch() {
        let tools = registry();
        let batches = schedule_tool_calls(
            vec![read("a", "1.txt"), read("b", "2.txt"), read("c", "3.txt")],
            &tools,
            MAX_PARALLEL_TOOL_CALLS,
        );
        assert_eq!(shape(&batches), vec![vec!["a", "b", "c"]]);
    }

    /// Anything that writes or executes is a batch of its own, and the model's
    /// order across batches is preserved exactly.
    #[test]
    fn mutating_calls_stay_sequential_and_in_order() {
        let tools = registry();
        let batches = schedule_tool_calls(
            vec![
                read("r1", "a.txt"),
                read("r2", "b.txt"),
                call(
                    "w1",
                    "write_file",
                    json!({ "path": "a.txt", "content": "x" }),
                ),
                read("r3", "a.txt"),
                call("b1", "bash", json!({ "command": "echo hi" })),
                call("b2", "bash", json!({ "command": "echo ho" })),
                read("r4", "a.txt"),
            ],
            &tools,
            MAX_PARALLEL_TOOL_CALLS,
        );
        assert_eq!(
            shape(&batches),
            vec![
                vec!["r1", "r2"],
                vec!["w1"],
                vec!["r3"],
                vec!["b1"],
                vec!["b2"],
                vec!["r4"],
            ],
            "a write must never be reordered against reads around it"
        );
        // Every call survives exactly once, in the original order.
        let flat: Vec<&str> = batches
            .iter()
            .flat_map(|b| b.iter().map(|c| c.0.as_str()))
            .collect();
        assert_eq!(flat, vec!["r1", "r2", "w1", "r3", "b1", "b2", "r4"]);
    }

    /// Concurrency is bounded: a long run of reads is chopped into batches.
    #[test]
    fn batches_are_bounded_by_max_parallel() {
        let tools = registry();
        let pending: Vec<PendingCall> = (0..9).map(|i| read(&format!("r{i}"), "a.txt")).collect();
        let batches = schedule_tool_calls(pending, &tools, 4);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches[1].len(), 4);
        assert_eq!(batches[2].len(), 1);

        // max_parallel = 1 degrades to the pre-F3-11 fully sequential loop.
        let seq = schedule_tool_calls(vec![read("a", "x"), read("b", "y")], &tools, 1);
        assert_eq!(shape(&seq), vec![vec!["a"], vec!["b"]]);
    }

    /// Per-call classification wins over the tool-wide flag.
    #[test]
    fn fetch_is_parallel_for_get_but_not_for_post() {
        let tools = registry();
        assert!(is_parallel_safe(
            &tools,
            "fetch",
            &json!({ "url": "http://x" })
        ));
        assert!(!is_parallel_safe(
            &tools,
            "fetch",
            &json!({ "url": "http://x", "method": "POST" })
        ));
    }

    /// Tools we cannot vouch for are never parallelised, even when they claim
    /// to be read-only: unknown names, sub-agent spawners, MCP tools.
    #[test]
    fn doubtful_tools_are_never_parallelised() {
        let tools = registry();
        assert!(!is_parallel_safe(&tools, "no_such_tool", &json!({})));
        assert!(!is_parallel_safe(&tools, "task", &json!({})));
        assert!(!is_parallel_safe(&tools, "fleet", &json!({})));
        assert!(!is_parallel_safe(&tools, "mcp__srv__lookup", &json!({})));

        let batches = schedule_tool_calls(
            vec![
                read("r1", "a.txt"),
                call("m1", "mcp__srv__lookup", json!({})),
                read("r2", "a.txt"),
            ],
            &tools,
            MAX_PARALLEL_TOOL_CALLS,
        );
        assert_eq!(shape(&batches), vec![vec!["r1"], vec!["m1"], vec!["r2"]]);
    }

    // ── end-to-end: a turn with three parallel reads ──────────────────

    /// A read-only tool that sleeps, records how many copies of itself were
    /// running at the same time, and optionally reports a failure.
    struct SlowRead {
        name: &'static str,
        delay: Duration,
        fails: bool,
        live: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SlowRead {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "slow read-only probe"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": { "path": { "type": "string" } } })
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: ToolCtx, args: Value) -> ToolOutput {
            let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(live, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.live.fetch_sub(1, Ordering::SeqCst);
            let path = args.get("path").and_then(Value::as_str).unwrap_or("?");
            if self.fails {
                ToolOutput::new(format!("error: boom reading {path}"), self.name)
            } else {
                ToolOutput::new(format!("contents of {path}"), self.name)
            }
        }
    }

    /// A provider that streams a scripted group of events per request.
    struct ScriptProvider {
        calls: AtomicUsize,
        script: Vec<Vec<StreamEvent>>,
    }

    #[async_trait]
    impl Provider for ScriptProvider {
        fn name(&self) -> &str {
            "script"
        }
        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> bebok_llm::StreamResult<BoxStream<'static, bebok_llm::StreamResult<StreamEvent>>>
        {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let group = self.script.get(n).cloned().unwrap_or_else(|| {
                vec![
                    StreamEvent::Text("done".to_string()),
                    StreamEvent::Done(Usage::default()),
                ]
            });
            Ok(Box::pin(futures::stream::iter(
                group.into_iter().map(Ok).collect::<Vec<_>>(),
            )))
        }
    }

    /// F6-3: the last call's provider usage (input + cache buckets) lands in
    /// `Session::context_used`, tagged with the model, and the context window
    /// resolves from the catalog for that model.
    #[tokio::test]
    async fn turn_records_last_call_context_used() {
        let base = std::env::temp_dir().join(format!("bebok-ctx-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("hello").await.unwrap();
        assert_eq!(session.meta_snapshot().await.context_used, None);

        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider {
            calls: AtomicUsize::new(0),
            script: vec![vec![
                StreamEvent::Text("hi".to_string()),
                StreamEvent::Done(Usage {
                    input_tokens: 1_200,
                    output_tokens: 10,
                    cost: None,
                    cache_read_input_tokens: Some(40_000),
                    cache_creation_input_tokens: Some(800),
                }),
            ]],
        });
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            Arc::new(registry()),
            provider,
            permission,
            store.bus(),
            CancellationToken::new(),
            "openai/gpt-4.1",
        )
        .await
        .unwrap();

        let meta = session.meta_snapshot().await;
        assert_eq!(meta.context_used, Some(42_000));
        assert_eq!(meta.context_model.as_deref(), Some("openai/gpt-4.1"));
        assert_eq!(
            crate::context::context_window_for(meta.context_model.as_deref()),
            1_048_576
        );
        // Cumulative usage is untouched by the gauge.
        assert_eq!(meta.usage.input_tokens, 1_200);
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    /// A provider that omits usage entirely still yields a (estimated) gauge.
    #[tokio::test]
    async fn turn_estimates_context_when_provider_omits_usage() {
        let base = std::env::temp_dir().join(format!("bebok-ctx-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("hello").await.unwrap();
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider {
            calls: AtomicUsize::new(0),
            script: vec![vec![
                StreamEvent::Text("hi".to_string()),
                StreamEvent::Done(Usage::default()),
            ]],
        });
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            Arc::new(registry()),
            provider,
            permission,
            store.bus(),
            CancellationToken::new(),
            "mock/model",
        )
        .await
        .unwrap();
        let meta = session.meta_snapshot().await;
        // System prompt + "hello" is never empty: the estimate is positive.
        assert!(meta.context_used.unwrap_or(0) > 0);
        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    fn tool_call(id: &str, name: &str, path: &str) -> StreamEvent {
        StreamEvent::ToolCall(ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            input: json!({ "path": path }),
        })
    }

    /// Run one turn whose single assistant message asks for `calls`, using a
    /// registry built from `tools`. Returns the elapsed time and the tool parts.
    async fn run_probe_turn(
        tools: Vec<Arc<dyn Tool>>,
        calls: Vec<StreamEvent>,
    ) -> (Duration, Vec<(String, crate::session::ToolState)>) {
        let base = std::env::temp_dir().join(format!("bebok-loop-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(project.join(".bebok"))
            .await
            .unwrap();
        // The probes are test doubles, not real tools: allow them outright so
        // the mutating one does not suspend the turn on a consent prompt.
        tokio::fs::write(
            project.join(".bebok").join("config.json"),
            r#"{ "permission": { "rules": [ { "pattern": "slow_write(*)", "action": "allow" } ] } }"#,
        )
        .await
        .unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("read three files")
            .await
            .unwrap();

        let mut first = calls;
        first.push(StreamEvent::Done(Usage::default()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider {
            calls: AtomicUsize::new(0),
            script: vec![first],
        });
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));

        let started = Instant::now();
        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            Arc::new(ToolRegistry::new(tools)),
            provider,
            permission,
            store.bus(),
            CancellationToken::new(),
            "mock/model",
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();

        let messages = session.messages_snapshot().await;
        let parts = messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|p| match p {
                crate::session::Part::Tool { id, state, .. } => Some((id.clone(), state.clone())),
                _ => None,
            })
            .collect();
        let _ = tokio::fs::remove_dir_all(&base).await;
        (elapsed, parts)
    }

    /// Three independent read-only calls in one model turn must be dispatched
    /// concurrently: all three run at the same time, and the whole turn takes
    /// far less than the sum of their delays.
    #[tokio::test]
    async fn three_read_only_calls_run_concurrently() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let delay = Duration::from_millis(200);
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(SlowRead {
            name: "slow_read",
            delay,
            fails: false,
            live: live.clone(),
            peak: peak.clone(),
        })];

        let (elapsed, parts) = run_probe_turn(
            tools,
            vec![
                tool_call("t1", "slow_read", "a.txt"),
                tool_call("t2", "slow_read", "b.txt"),
                tool_call("t3", "slow_read", "c.txt"),
            ],
        )
        .await;

        assert_eq!(
            peak.load(Ordering::SeqCst),
            3,
            "all three reads must be in flight at once"
        );
        assert_eq!(parts.len(), 3);
        // Sequential would need >= 600ms; generous margin for a loaded CI box.
        assert!(
            elapsed < delay * 3,
            "turn took {elapsed:?}, i.e. it did not overlap the reads"
        );
        for (id, state) in &parts {
            assert!(
                matches!(state, crate::session::ToolState::Completed { .. }),
                "{id} did not complete: {state:?}"
            );
        }
        // Model order is preserved in the transcript.
        let ids: Vec<&str> = parts.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }

    /// One failing call inside a parallel batch must not take its siblings
    /// down: the other two still complete, and the order is unchanged.
    #[tokio::test]
    async fn a_failing_call_does_not_abort_its_batch() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mk = |name: &'static str, fails: bool| -> Arc<dyn Tool> {
            Arc::new(SlowRead {
                name,
                delay: Duration::from_millis(50),
                fails,
                live: live.clone(),
                peak: peak.clone(),
            })
        };

        let (_, parts) = run_probe_turn(
            vec![mk("slow_read", false), mk("boom_read", true)],
            vec![
                tool_call("t1", "slow_read", "a.txt"),
                tool_call("t2", "boom_read", "b.txt"),
                tool_call("t3", "slow_read", "c.txt"),
            ],
        )
        .await;

        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert_eq!(parts.len(), 3);
        let ids: Vec<&str> = parts.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
        for (id, state) in &parts {
            match state {
                crate::session::ToolState::Completed { output, .. } => {
                    if id == "t2" {
                        assert!(output.contains("boom"), "t2 should carry its failure");
                    } else {
                        assert!(output.contains("contents of"), "{id} lost its output");
                    }
                }
                other => panic!("{id} did not complete: {other:?}"),
            }
        }
    }

    /// A write between two reads must not be overlapped with either of them.
    #[tokio::test]
    async fn a_mutating_call_is_never_overlapped() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mk = |name: &'static str| -> Arc<dyn Tool> {
            Arc::new(SlowRead {
                name,
                delay: Duration::from_millis(50),
                fails: false,
                live: live.clone(),
                peak: peak.clone(),
            })
        };

        /// A mutating probe that shares the same concurrency counters.
        struct SlowWrite {
            live: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl Tool for SlowWrite {
            fn name(&self) -> &str {
                "slow_write"
            }
            fn description(&self) -> &str {
                "slow mutating probe"
            }
            fn parameters_schema(&self) -> Value {
                json!({ "type": "object", "properties": { "path": { "type": "string" } } })
            }
            async fn execute(&self, _ctx: ToolCtx, _args: Value) -> ToolOutput {
                let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(live, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.live.fetch_sub(1, Ordering::SeqCst);
                ToolOutput::new("written", "slow_write")
            }
        }

        let (_, parts) = run_probe_turn(
            vec![
                mk("slow_read"),
                Arc::new(SlowWrite {
                    live: live.clone(),
                    peak: peak.clone(),
                }),
            ],
            vec![
                tool_call("t1", "slow_read", "a.txt"),
                tool_call("t2", "slow_write", "a.txt"),
                tool_call("t3", "slow_read", "a.txt"),
            ],
        )
        .await;

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "a write must run alone, and the reads around it must not join it"
        );
        assert_eq!(parts.len(), 3);
    }
}

#[cfg(test)]
mod retry_tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use bebok_llm::{
        ChatMessage, ChatRequest, LlmError, Provider, StreamEvent, StreamResult, Thinking, Usage,
    };
    use futures::stream::{self, BoxStream};

    use super::*;

    /// A provider that fails a scripted number of times before succeeding.
    struct FlakyProvider {
        /// Errors to return, in order; once exhausted the stream succeeds.
        script: Mutex<Vec<LlmError>>,
        calls: AtomicU32,
    }

    impl FlakyProvider {
        fn new(script: Vec<LlmError>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into_iter().rev().collect()),
                calls: AtomicU32::new(0),
            })
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for FlakyProvider {
        fn name(&self) -> &str {
            "flaky"
        }

        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(err) = self.script.lock().unwrap().pop() {
                return Err(err);
            }
            let events = vec![
                Ok(StreamEvent::Text("ok".to_string())),
                Ok(StreamEvent::Done(Usage::default())),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn http(status: u16, body: &str, retry_after: Option<u64>) -> LlmError {
        LlmError::Http {
            status,
            body: body.to_string(),
            retry_after,
        }
    }

    fn rate_limited(retry_after: Option<u64>) -> LlmError {
        http(
            429,
            "{\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limit reached\"}}",
            retry_after,
        )
    }

    fn fast_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(4),
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "openai/gpt-4.1".to_string(),
            system: "sys".to_string(),
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            max_tokens: 128,
            thinking: Thinking::Off,
        }
    }

    async fn collect(stream: BoxStream<'static, StreamResult<StreamEvent>>) -> Vec<StreamEvent> {
        stream
            .filter_map(|ev| async move { ev.ok() })
            .collect()
            .await
    }

    /// The headline case: one 429, then success. The turn must get its stream
    /// instead of failing, having called the provider exactly twice.
    #[tokio::test]
    async fn retries_once_after_a_429_then_succeeds() {
        let provider = FlakyProvider::new(vec![rate_limited(None)]);
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        let abort = CancellationToken::new();
        let mut retries = Vec::new();

        let stream = stream_with_retry(
            &dyn_provider,
            &req(),
            fast_policy(3),
            &abort,
            |attempt, err, delay| retries.push((attempt, err.kind(), delay)),
        )
        .await
        .expect("retry should recover from a single 429");

        assert_eq!(provider.calls(), 2, "one failed call plus one success");
        assert_eq!(retries.len(), 1, "exactly one retry");
        assert_eq!(retries[0].0, 1);
        assert_eq!(retries[0].1, bebok_llm::ProviderErrorKind::RateLimited);

        let events = collect(stream).await;
        assert_eq!(events.len(), 2);
        assert!(matches!(events[1], StreamEvent::Done(_)));
    }

    /// A transient 5xx is retried the same way as a 429.
    #[tokio::test]
    async fn retries_server_errors() {
        let provider = FlakyProvider::new(vec![
            http(503, "upstream unavailable", None),
            http(500, "{\"error\":{\"message\":\"internal\"}}", None),
        ]);
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        let abort = CancellationToken::new();

        let opened =
            stream_with_retry(&dyn_provider, &req(), fast_policy(3), &abort, |_, _, _| {}).await;
        assert!(
            opened.is_ok(),
            "two server errors are within the attempt budget"
        );
        assert_eq!(provider.calls(), 3);
    }

    /// Permanent failures must not burn the attempt budget.
    #[tokio::test]
    async fn does_not_retry_auth_or_model_errors() {
        for err in [
            http(401, "{\"error\":{\"message\":\"invalid api key\"}}", None),
            http(404, "{\"error\":{\"code\":\"model_not_found\"}}", None),
            http(
                400,
                "{\"error\":{\"message\":\"max_tokens too large\"}}",
                None,
            ),
        ] {
            let provider = FlakyProvider::new(vec![err]);
            let dyn_provider: Arc<dyn Provider> = provider.clone();
            let abort = CancellationToken::new();
            let mut retries = 0;
            let out =
                stream_with_retry(&dyn_provider, &req(), fast_policy(3), &abort, |_, _, _| {
                    retries += 1
                })
                .await;
            assert!(out.is_err());
            assert_eq!(provider.calls(), 1, "permanent error must fail fast");
            assert_eq!(retries, 0);
        }
    }

    /// The budget is finite: a provider that is down stays down.
    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let provider = FlakyProvider::new(vec![
            rate_limited(None),
            rate_limited(None),
            rate_limited(None),
            rate_limited(None),
        ]);
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        let abort = CancellationToken::new();

        let out =
            stream_with_retry(&dyn_provider, &req(), fast_policy(3), &abort, |_, _, _| {}).await;
        let err = match out {
            Ok(_) => panic!("still failing after the budget"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), bebok_llm::ProviderErrorKind::RateLimited);
        assert_eq!(provider.calls(), 3, "max_attempts calls, no more");
    }

    /// An aborted turn must not sit in a backoff sleep.
    #[tokio::test]
    async fn abort_stops_retrying() {
        let provider = FlakyProvider::new(vec![rate_limited(Some(30))]);
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        let abort = CancellationToken::new();
        abort.cancel();

        let out = stream_with_retry(
            &dyn_provider,
            &req(),
            RetryPolicy::default(),
            &abort,
            |_, _, _| {},
        )
        .await;
        assert!(out.is_err());
        assert_eq!(provider.calls(), 1);
    }

    /// `max_attempts: 1` disables retrying entirely.
    #[tokio::test]
    async fn single_attempt_policy_never_retries() {
        let provider = FlakyProvider::new(vec![rate_limited(None)]);
        let dyn_provider: Arc<dyn Provider> = provider.clone();
        let abort = CancellationToken::new();
        let out =
            stream_with_retry(&dyn_provider, &req(), fast_policy(1), &abort, |_, _, _| {}).await;
        assert!(out.is_err());
        assert_eq!(provider.calls(), 1);
    }

    /// Backoff doubles per attempt, stays within [half, full] of the window
    /// thanks to equal jitter, and is capped.
    #[test]
    fn backoff_is_exponential_jittered_and_capped() {
        let policy = RetryPolicy {
            max_attempts: 6,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1000),
        };
        for (attempt, window_ms) in [(1u32, 100u64), (2, 200), (3, 400), (4, 800)] {
            for _ in 0..50 {
                let d = policy.delay_for(attempt, None).as_millis() as u64;
                assert!(
                    (window_ms / 2..=window_ms).contains(&d),
                    "attempt {attempt}: {d}ms outside [{}, {window_ms}]",
                    window_ms / 2
                );
            }
        }
        // Capped: attempt 5 would be 1600ms, the cap is 1000ms.
        for _ in 0..50 {
            let d = policy.delay_for(5, None).as_millis() as u64;
            assert!((500..=1000).contains(&d), "cap not applied: {d}ms");
        }
        // And it does not overflow for an absurd attempt number.
        assert!(policy.delay_for(u32::MAX, None) <= policy.max_delay);
    }

    /// `retry_after` is a floor: honoured when longer than the computed
    /// backoff, ignored when the backoff already exceeds it.
    #[test]
    fn retry_after_is_a_floor_for_the_backoff() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
        };
        let d = policy.delay_for(1, Some(Duration::from_secs(5)));
        assert_eq!(d, Duration::from_secs(5), "provider hint wins when longer");

        let d = policy.delay_for(4, Some(Duration::from_millis(1)));
        assert!(
            d >= Duration::from_millis(400),
            "computed backoff wins when longer: {d:?}"
        );
    }

    /// The hint travels from the HTTP header through the error into the policy.
    #[test]
    fn retry_after_hint_survives_error_classification() {
        let err = rate_limited(Some(12));
        assert_eq!(err.retry_after(), Some(Duration::from_secs(12)));
        assert!(err.is_transient());
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay_for(1, err.retry_after()),
            Duration::from_secs(12)
        );
    }

    /// Transport-level failures (dropped connection) are transient too.
    #[test]
    fn stream_errors_are_classified_transient() {
        assert!(LlmError::Stream("connection reset by peer".into()).is_transient());
        assert!(!LlmError::Parse("bad SSE data".into()).is_transient());
    }
}
