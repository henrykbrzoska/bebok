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
