//! [`CliProvider`]: the `bebok_llm::Provider` impl over a CLI backend.
//!
//! This is the seam between two very different worlds. The agent loop speaks
//! [`bebok_llm::ChatRequest`] / [`bebok_llm::StreamEvent`]; a CLI speaks argv
//! and stdout lines. The translation rules that matter:
//!
//! - **The prompt is flattened.** A CLI takes one `-p` argument, so the whole
//!   conversation is rendered as text ([`crate::transcript`]) instead of being
//!   sent as a message array.
//! - **`req.tools` is dropped.** The CLI has its own tool registry; handing it
//!   Bebok's would describe tools it can never call.
//! - **Tool activity becomes thinking.** [`bebok_llm::StreamEvent::ToolCall`]
//!   would make the agent loop execute the call through Bebok's registry (see
//!   [`crate::backend`]), so activity is emitted as `Thinking` in v1.
//! - **Exactly one `Done`, last.** The agent loop treats `Done` as end-of-turn
//!   and records usage from it. Even a failed-but-billed CLI turn must still
//!   emit it before the error, or the cost disappears from the stats.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bebok_llm::{ChatRequest, LlmError, Provider, StreamEvent, StreamResult, Usage};
use futures::stream::{BoxStream, StreamExt};

use crate::backend::{CliBackend, CliError, CliEvent, CliRequest};
use crate::runner;
use crate::spec::CliSpec;

/// Text the CLI prints when asked to resume a conversation it no longer has.
const RESUME_LOST_MARKER: &str = "No conversation found";

/// Continue a CLI-side conversation from an earlier turn instead of re-sending
/// the whole transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResume {
    /// The CLI's own conversation id (`--resume <key>`).
    pub key: String,
    /// Index into the chat messages of the first one the CLI has not seen.
    /// Only the messages from here on are sent.
    pub from_msg: usize,
    /// Profile digest the conversation was started with. A different profile
    /// means different settings and hooks, so the turn starts over.
    pub profile_hash: String,
}

/// What a finished turn tells its caller, so the conversation can be resumed
/// next time. `key: None` means nothing is resumable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliSessionReport {
    pub key: Option<String>,
    pub profile_hash: Option<String>,
}

/// Where a provider leaves its [`CliSessionReport`] when a turn ends.
pub type CliSessionSink = Arc<Mutex<Option<CliSessionReport>>>;

pub use crate::runner::{CliInputSlot, LiveInput};

/// Publish a run's live input (or clear a stale one) into the session slot.
fn publish_input(slot: &Option<CliInputSlot>, input: Option<LiveInput>) {
    if let Some(slot) = slot
        && let Ok(mut guard) = slot.lock()
    {
        *guard = input;
    }
}

/// A provider that drives an agentic CLI.
pub struct CliProvider {
    backend: Arc<dyn CliBackend>,
    spec: CliSpec,
    cwd: PathBuf,
    /// Environment every turn of this provider adds to the child process: the
    /// hook endpoint + its turn-scoped token, and (in `isolated` profile mode)
    /// the config dir override. Assembled by the engine for one turn.
    extra_env: Vec<(String, String)>,
    /// Conversation to continue, when the caller has one for this session.
    resume: Option<CliResume>,
    /// Receives the conversation key and profile digest when the turn ends.
    sink: Option<CliSessionSink>,
    /// Receives the live-input handle of the running turn.
    input_slot: Option<CliInputSlot>,
}

impl CliProvider {
    pub fn new(backend: Arc<dyn CliBackend>, spec: CliSpec, cwd: PathBuf) -> Self {
        Self {
            backend,
            spec,
            cwd,
            extra_env: Vec::new(),
            resume: None,
            sink: None,
            input_slot: None,
        }
    }

    /// Publish the running turn's [`LiveInput`] into `slot`, so a prompt sent
    /// mid-turn can be written to the CLI instead of aborting it.
    pub fn with_input_slot(mut self, slot: Option<CliInputSlot>) -> Self {
        self.input_slot = slot;
        self
    }

    /// Continue an earlier conversation when the CLI still has it (see
    /// [`CliResume`]). Falls back to the full transcript when it cannot.
    pub fn with_resume(mut self, resume: Option<CliResume>) -> Self {
        self.resume = resume;
        self
    }

    /// Report the conversation key and profile digest to `sink` at turn end.
    pub fn with_session_sink(mut self, sink: Option<CliSessionSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Add environment variables every turn of this provider passes to the CLI
    /// (see `bebok_core::cli_turn_token` for the hook token that motivates it).
    pub fn with_extra_env(mut self, env: Vec<(String, String)>) -> Self {
        self.extra_env = env;
        self
    }

    /// The backend this provider drives (used by `/cli/backends` reporting and
    /// tests).
    pub fn backend(&self) -> &Arc<dyn CliBackend> {
        &self.backend
    }

    /// The effective spec.
    pub fn spec(&self) -> &CliSpec {
        &self.spec
    }

    /// The working directory every turn runs in.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }
}

impl std::fmt::Debug for CliProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliProvider")
            .field("backend", &self.backend.id())
            .field("cwd", &self.cwd)
            .finish()
    }
}

/// The CLI-side model id for a Bebok `provider/model` string.
///
/// Only the FIRST segment is Bebok's provider name; the rest belongs to the
/// CLI and must be sent verbatim (`claude-cli/vendor/model` -> `vendor/model`).
/// Splitting on the last `/` would mangle ids that themselves contain slashes.
fn cli_model_id(model: &str) -> &str {
    match model.split_once('/') {
        Some((_, rest)) => rest,
        None => model,
    }
}

/// Accumulate a `Usage` report from the CLI into the turn total.
///
/// The CLI streams one `result` event per turn today, but summing keeps this
/// correct if a backend emits several. `cost` is reported per turn, not per
/// chunk, so the last value wins instead of being summed.
fn merge_usage(total: &mut Usage, next: Usage) {
    total.input_tokens += next.input_tokens;
    total.output_tokens += next.output_tokens;
    if next.cost.is_some() {
        total.cost = next.cost;
    }
    if let Some(v) = next.cache_read_input_tokens {
        *total.cache_read_input_tokens.get_or_insert(0) += v;
    }
    if let Some(v) = next.cache_creation_input_tokens {
        *total.cache_creation_input_tokens.get_or_insert(0) += v;
    }
    // The prompt size is a snapshot of the latest API call, not a total.
    if next.context_tokens.is_some() {
        total.context_tokens = next.context_tokens;
    }
}

/// How a CLI event is presented to the agent loop.
///
/// Returns `None` for events that are bookkeeping rather than output (the
/// session key is remembered by the caller, not shown).
fn present(ev: CliEvent) -> Option<StreamEvent> {
    match ev {
        CliEvent::Text(t) => Some(StreamEvent::Text(t)),
        CliEvent::Thinking(t) => Some(StreamEvent::Thinking(t)),
        CliEvent::ToolActivity { name, detail } => {
            Some(StreamEvent::Thinking(format!("⚙ {name}: {detail}")))
        }
        CliEvent::ExternalTool {
            id,
            name,
            input,
            output,
            is_error,
        } => Some(StreamEvent::ExternalTool {
            id,
            name,
            input,
            output,
            is_error,
            source: bebok_llm::ExternalSource::Cli,
        }),
        CliEvent::Usage(_) | CliEvent::SessionKey(_) => None,
    }
}

/// Everything needed to start the same turn again with the full transcript.
struct RetryPlan {
    backend: Arc<dyn CliBackend>,
    spec: CliSpec,
    full: CliRequest,
}

/// Streaming state for one turn.
struct RunState {
    inner: BoxStream<'static, Result<CliEvent, CliError>>,
    exit: Option<tokio::task::JoinHandle<Result<Option<i32>, CliError>>>,
    /// Accumulated usage; emitted once, in the single final `Done`.
    usage: Usage,
    saw_usage: bool,
    /// Items still to hand to the consumer before the stream ends. Filled once,
    /// when the inner stream is exhausted (a turn can end with `Done` followed
    /// by an error).
    queue: VecDeque<StreamResult<StreamEvent>>,
    /// True once the inner stream ended and the child was reaped.
    finalised: bool,
    /// The current attempt continues a conversation (`--resume`).
    resumed: bool,
    /// Set when the current attempt produced something the consumer saw.
    emitted: bool,
    /// Present while a resumed attempt can still fall back to the full
    /// transcript. Taken (so at most one retry happens) when it is used.
    retry: Option<RetryPlan>,
    /// The resumed conversation was lost before any output: start over.
    want_retry: bool,
    /// Conversation key announced by the current attempt.
    session_key: Option<String>,
    /// Profile digest of the current attempt.
    profile_hash: Option<String>,
    sink: Option<CliSessionSink>,
    input_slot: Option<CliInputSlot>,
}

impl RunState {
    /// Start the same turn again with the full transcript, after the CLI lost
    /// the conversation it was asked to resume.
    async fn restart_full(&mut self) -> Result<(), CliError> {
        let plan = self
            .retry
            .take()
            .ok_or_else(|| CliError::Stream("no full transcript to fall back to".into()))?;
        tracing::info!("CLI lost the resumed conversation; retrying with the full transcript");
        let run = runner::spawn_cli(plan.backend, plan.spec, plan.full).await?;
        publish_input(&self.input_slot, run.input);
        self.inner = run.events;
        self.exit = Some(run.exit);
        self.profile_hash = run.profile_hash;
        self.session_key = None;
        self.resumed = false;
        self.finalised = false;
        Ok(())
    }

    /// Await the child, then decide what the tail of the stream looks like.
    async fn finalise(&mut self, stream_error: Option<CliError>) {
        let exit_result = match self.exit.take() {
            Some(handle) => handle.await,
            // Already reaped (or never created): nothing to add.
            None => Ok(Ok(None)),
        };

        let mut cli_error: Option<CliError> = stream_error;
        let mut error: Option<LlmError> = None;
        if cli_error.is_none() {
            match exit_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => cli_error = Some(e),
                Err(join) => {
                    error = Some(LlmError::Provider(format!("CLI supervisor failed: {join}")));
                }
            }
        }
        if let Some(e) = cli_error {
            let e = classify_resume_lost(e);
            if matches!(e, CliError::ResumeLost(_))
                && self.resumed
                && !self.emitted
                && self.retry.is_some()
            {
                // The stream is over for this attempt; `next` restarts it.
                self.want_retry = true;
                self.finalised = true;
                return;
            }
            error = Some(LlmError::from(e));
        }

        if let Some(sink) = &self.sink
            && let Ok(mut slot) = sink.lock()
        {
            *slot = Some(CliSessionReport {
                key: self.session_key.clone(),
                profile_hash: self.profile_hash.clone(),
            });
        }

        // `Done` first: a billed turn must be accounted for even when the CLI
        // then reports a failure. A clean turn always ends with `Done`.
        let done_required = self.saw_usage || error.is_none();
        if done_required {
            let usage = if self.saw_usage {
                std::mem::take(&mut self.usage)
            } else {
                Usage::default()
            };
            self.queue.push_back(Ok(StreamEvent::Done(usage)));
        }
        if let Some(e) = error {
            self.queue.push_back(Err(e));
        }
        self.finalised = true;
    }
}

#[async_trait::async_trait]
impl Provider for CliProvider {
    fn name(&self) -> &str {
        self.backend.id()
    }

    fn transport(&self) -> bebok_llm::Transport {
        bebok_llm::Transport::AgenticCli {
            bebok_tools: false,
            native_tools: true,
        }
    }

    /// Stream one turn through the CLI.
    ///
    /// The child process is owned by the returned stream: dropping it (an
    /// aborted turn) kills the whole process tree, so an abort in the GUI never
    /// leaves a CLI running against the project. Nothing is spawned detached.
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let caps = self.backend.capabilities();

        // `Option::filter` is shadowed by `futures::StreamExt::filter` in this
        // module, hence the explicit form.
        let system = if caps.system_prompt && !req.system.trim().is_empty() {
            Some(req.system.clone())
        } else {
            None
        };
        let model_id = cli_model_id(&req.model);
        let model = if model_id.trim().is_empty() {
            None
        } else {
            Some(model_id.to_string())
        };

        // `allow_write` is the single opt-in for letting the CLI mutate the
        // tree; it bypasses Bebok's permission engine and change tracking,
        // so it stays false unless the user asked for it in config.
        let turn = |from: usize, resume_key: Option<String>| CliRequest {
            prompt: crate::transcript::render_prompt(&slice_from(&req, from)),
            blocks: crate::transcript::render_blocks(&slice_from(&req, from)),
            system: system.clone(),
            model: model.clone(),
            cwd: self.cwd.clone(),
            resume_key,
            write: self.spec.allow_write,
            max_turns: self.spec.max_turns,
            timeout: self.spec.timeout(),
            extra_env: self.extra_env.clone(),
        };
        let full = turn(0, None);

        // Continue the CLI's conversation only when the binary can, the
        // profile is unchanged and the tail is non-empty. Otherwise the whole
        // transcript goes out again.
        let (first, resumed) = match &self.resume {
            Some(hint) if hint.from_msg < req.messages.len() => {
                let tail = turn(hint.from_msg, Some(hint.key.clone()));
                let pre = runner::preflight(self.backend.as_ref(), &self.spec, &tail)
                    .await
                    .map_err(LlmError::from)?;
                let same_profile = pre.profile_hash.unwrap_or_default() == hint.profile_hash;
                if pre.resume_supported && same_profile {
                    (tail, true)
                } else {
                    (full.clone(), false)
                }
            }
            _ => (full.clone(), false),
        };

        let run = runner::spawn_cli(self.backend.clone(), self.spec.clone(), first)
            .await
            .map_err(LlmError::from)?;

        publish_input(&self.input_slot, run.input.clone());
        let retry = resumed.then(|| RetryPlan {
            backend: self.backend.clone(),
            spec: self.spec.clone(),
            full,
        });

        let state = RunState {
            inner: run.events,
            exit: Some(run.exit),
            usage: Usage::default(),
            saw_usage: false,
            queue: VecDeque::new(),
            finalised: false,
            resumed,
            emitted: false,
            retry,
            want_retry: false,
            session_key: None,
            profile_hash: run.profile_hash,
            sink: self.sink.clone(),
            input_slot: self.input_slot.clone(),
        };

        let stream = futures::stream::unfold(Some(state), |slot: Option<RunState>| async move {
            let mut st = slot?;

            loop {
                if let Some(item) = st.queue.pop_front() {
                    return Some((item, Some(st)));
                }
                if st.finalised {
                    if st.want_retry {
                        st.want_retry = false;
                        if let Err(e) = st.restart_full().await {
                            st.queue.push_back(Err(LlmError::from(e)));
                            st.finalised = true;
                        }
                        continue;
                    }
                    // Queue drained and nothing left to say: end the stream.
                    return None;
                }

                match st.inner.next().await {
                    Some(Ok(ev)) => {
                        if let CliEvent::SessionKey(key) = &ev {
                            st.session_key = Some(key.clone());
                        }
                        if let CliEvent::Usage(u) = ev {
                            merge_usage(&mut st.usage, u);
                            st.saw_usage = true;
                            continue;
                        }
                        if let Some(event) = present(ev) {
                            st.emitted = true;
                            return Some((Ok(event), Some(st)));
                        }
                        // Bookkeeping event: keep reading.
                        continue;
                    }
                    Some(Err(e)) => {
                        st.finalise(Some(e)).await;
                        continue;
                    }
                    None => {
                        st.finalise(None).await;
                        continue;
                    }
                }
            }
        });

        Ok(Box::pin(stream))
    }
}

/// The chat request without its first `from` messages, for a resumed turn.
fn slice_from(req: &ChatRequest, from: usize) -> ChatRequest {
    ChatRequest {
        messages: req.messages[from.min(req.messages.len())..].to_vec(),
        ..req.clone()
    }
}

/// Turn "the CLI does not know this conversation" into [`CliError::ResumeLost`],
/// so the caller can tell it apart from any other failure.
fn classify_resume_lost(err: CliError) -> CliError {
    let text = match &err {
        CliError::Exit { stderr, .. } => Some(stderr.clone()),
        CliError::RunFailed(message) => Some(message.clone()),
        _ => None,
    };
    match text {
        Some(text) if text.contains(RESUME_LOST_MARKER) => CliError::ResumeLost(text),
        _ => err,
    }
}

/// Build a provider for a `kind: "cli"` spec.
///
/// The project root is required: it becomes the CLI's `current_dir`, and a
/// guessed cwd would let the CLI read or edit the wrong tree.
pub fn build_provider(
    spec: &bebok_llm::ProviderSpec,
    project_root: Option<PathBuf>,
) -> Result<Arc<dyn Provider>, String> {
    build_provider_with_env(spec, project_root, Vec::new())
}

/// [`build_provider`] plus the extra environment the child must see this turn
/// (the hook endpoint and its turn-scoped token).
pub fn build_provider_with_env(
    spec: &bebok_llm::ProviderSpec,
    project_root: Option<PathBuf>,
    extra_env: Vec<(String, String)>,
) -> Result<Arc<dyn Provider>, String> {
    build_provider_for_turn(spec, project_root, extra_env, None, None, None, None)
}

/// [`build_provider_with_env`] plus the conversation to continue (if any) and
/// the sink that receives the conversation key when the turn ends.
pub fn build_provider_for_turn(
    spec: &bebok_llm::ProviderSpec,
    project_root: Option<PathBuf>,
    extra_env: Vec<(String, String)>,
    resume: Option<CliResume>,
    sink: Option<CliSessionSink>,
    input_slot: Option<CliInputSlot>,
    allow_write: Option<bool>,
) -> Result<Arc<dyn Provider>, String> {
    let mut cli_spec = CliSpec::from_extra(&spec.extra).map_err(|e| e.to_string())?;
    // A per-turn override from the agent preset (see `ProviderCtx`): a preset
    // with full tool access forces write on, a read-only preset forces it off.
    // `None` keeps the config value.
    if let Some(allow) = allow_write {
        cli_spec.allow_write = allow;
    }

    let backend = crate::backends::backend_for(&cli_spec.backend).ok_or_else(|| {
        let known: Vec<&str> = crate::backends::all_backends()
            .iter()
            .map(|b| b.id())
            .collect();
        format!(
            "{} (known backends: {})",
            CliError::UnknownBackend(cli_spec.backend.clone()),
            known.join(", ")
        )
    })?;

    let root = project_root.ok_or_else(|| {
        format!(
            "provider '{}' is a CLI provider ('{}') but no project root was supplied; \
             the CLI needs a working directory",
            spec.name, cli_spec.backend
        )
    })?;

    Ok(Arc::new(
        CliProvider::new(backend, cli_spec, root)
            .with_extra_env(extra_env)
            .with_resume(resume)
            .with_session_sink(sink)
            .with_input_slot(input_slot),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Result::unwrap_err` needs `T: Debug`, and `Arc<dyn Provider>` is not.
    fn expect_err(result: Result<Arc<dyn Provider>, String>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    fn spec_with(extra: serde_json::Value) -> bebok_llm::ProviderSpec {
        let mut spec = bebok_llm::ProviderSpec {
            name: "claude-cli".into(),
            kind: bebok_llm::ProviderKind::Cli,
            ..Default::default()
        };
        if let serde_json::Value::Object(map) = extra {
            spec.extra = map;
        }
        spec
    }

    #[test]
    fn model_id_takes_the_part_after_the_first_slash() {
        assert_eq!(cli_model_id("claude-cli/sonnet"), "sonnet");
        assert_eq!(cli_model_id("claude-cli/vendor/model"), "vendor/model");
        assert_eq!(cli_model_id("sonnet"), "sonnet");
    }

    #[test]
    fn missing_backend_is_reported() {
        let err = expect_err(build_provider(
            &spec_with(serde_json::json!({})),
            Some("/tmp".into()),
        ));
        assert!(err.contains("extra.backend"), "{err}");
    }

    #[test]
    fn unknown_backend_lists_the_known_ones() {
        let err = expect_err(build_provider(
            &spec_with(serde_json::json!({ "backend": "nope" })),
            Some("/tmp".into()),
        ));
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("claude"), "{err}");
    }

    #[test]
    fn missing_project_root_is_reported() {
        let err = expect_err(build_provider(
            &spec_with(serde_json::json!({ "backend": "claude" })),
            None,
        ));
        assert!(err.contains("project root"), "{err}");
    }

    #[test]
    fn valid_spec_builds_a_provider_in_the_project_root() {
        let provider = build_provider(
            &spec_with(serde_json::json!({ "backend": "claude" })),
            Some("/tmp/project".into()),
        )
        .expect("builds");
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn per_turn_allow_write_override_wins_over_config() {
        // The override is applied to the spec before the backend is resolved:
        // `None` keeps the config value, `Some` forces it either way.
        let base = CliSpec::from_extra(
            serde_json::json!({ "backend": "claude" })
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert!(!base.allow_write, "writing stays opt-in by default");
        let mut forced_on = base.clone();
        forced_on.allow_write = true;
        assert!(forced_on.allow_write);
        let writing = CliSpec::from_extra(
            serde_json::json!({ "backend": "claude", "allow_write": true })
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert!(writing.allow_write);
        let mut forced_off = writing.clone();
        forced_off.allow_write = false;
        assert!(!forced_off.allow_write);
        // And the builder accepts the extra argument in every position.
        let spec = spec_with(serde_json::json!({ "backend": "claude" }));
        for allow in [None, Some(true), Some(false)] {
            build_provider_for_turn(
                &spec,
                Some("/tmp/project".into()),
                Vec::new(),
                None,
                None,
                None,
                allow,
            )
            .expect("builds");
        }
    }

    #[test]
    fn usage_is_summed_but_cost_is_taken_from_the_last_report() {
        let mut total = Usage::default();
        merge_usage(
            &mut total,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                cost: Some(0.01),
                cache_read_input_tokens: Some(3),
                cache_creation_input_tokens: None,
                context_tokens: None,
            },
        );
        merge_usage(
            &mut total,
            Usage {
                input_tokens: 1,
                output_tokens: 2,
                cost: Some(0.02),
                cache_read_input_tokens: Some(4),
                cache_creation_input_tokens: Some(7),
                context_tokens: None,
            },
        );
        assert_eq!(total.input_tokens, 11);
        assert_eq!(total.output_tokens, 7);
        assert_eq!(total.cost, Some(0.02));
        assert_eq!(total.cache_read_input_tokens, Some(7));
        assert_eq!(total.cache_creation_input_tokens, Some(7));
    }

    #[test]
    fn context_size_is_the_latest_snapshot_not_a_sum() {
        let mut total = Usage::default();
        for context in [Some(10_000), Some(25_000), None] {
            merge_usage(
                &mut total,
                Usage {
                    context_tokens: context,
                    ..Usage::default()
                },
            );
        }
        assert_eq!(total.context_tokens, Some(25_000));
    }

    #[test]
    fn tool_activity_is_presented_as_thinking_with_a_prefix() {
        let presented = present(CliEvent::ToolActivity {
            name: "Bash".into(),
            detail: "ls".into(),
        })
        .expect("presented");
        match presented {
            StreamEvent::Thinking(text) => assert_eq!(text, "⚙ Bash: ls"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn bookkeeping_events_are_not_presented() {
        assert!(present(CliEvent::SessionKey("abc".into())).is_none());
        assert!(present(CliEvent::Usage(Usage::default())).is_none());
    }

    /// End-to-end mapping through the real runner, using a shell script instead
    /// of a real CLI so CI needs no `claude` install.
    #[cfg(unix)]
    #[tokio::test]
    async fn streams_text_tool_activity_and_one_final_done() {
        use crate::backend::{CliCapabilities, CliStreamState};

        /// A backend that prints canned JSONL from a shell one-liner.
        struct FakeBackend;

        impl CliBackend for FakeBackend {
            fn id(&self) -> &'static str {
                "fake"
            }
            fn label(&self) -> &'static str {
                "Fake CLI"
            }
            fn capabilities(&self) -> CliCapabilities {
                CliCapabilities::default()
            }
            fn default_models(&self) -> Vec<String> {
                vec!["sonnet".into()]
            }
            fn binary_names(&self) -> &'static [&'static str] {
                &["sh"]
            }
            fn env_override(&self) -> &'static str {
                "BEBOK_FAKE_CLI_BIN"
            }
            fn build_command(
                &self,
                _spec: &CliSpec,
                _req: &CliRequest,
                _probe: &crate::backend::CliProbe,
                _scratch: &mut crate::backend::TurnScratch,
            ) -> Result<(String, Vec<String>), CliError> {
                let jsonl = concat!(
                    r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
                    "\n",
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#,
                    "\n",
                    r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
                    "\n",
                    r#"{"type":"result","subtype":"success","session_id":"s-1","usage":{"input_tokens":7,"output_tokens":3},"total_cost_usd":0.5}"#,
                    "\n",
                );
                Ok((
                    "sh".to_string(),
                    vec![
                        "-c".to_string(),
                        format!("printf '%s' \"{}\"", jsonl.replace('"', "\\\"")),
                    ],
                ))
            }
            fn parse_line(
                &self,
                line: &str,
                st: &mut CliStreamState,
            ) -> Result<Option<CliEvent>, CliError> {
                crate::backends::backend_for("claude")
                    .expect("claude backend")
                    .parse_line(line, st)
            }
        }

        let provider = CliProvider::new(
            Arc::new(FakeBackend),
            CliSpec {
                backend: "fake".into(),
                command: None,
                args: Vec::new(),
                mode: crate::backend::CliMode::Agent,
                allow_write: false,
                timeout_secs: 30,
                max_turns: None,
                profile: Default::default(),
                profile_overrides: None,
                system_prompt_mode: Default::default(),
            },
            PathBuf::from("/tmp"),
        );

        let events: Vec<_> = provider
            .stream(ChatRequest {
                model: "fake/sonnet".into(),
                system: String::new(),
                messages: vec![bebok_llm::ChatMessage::user("hi")],
                tools: Vec::new(),
                max_tokens: 128,
                thinking: bebok_llm::Thinking::Off,
                sampling: bebok_llm::Sampling::default(),
            })
            .await
            .expect("streams")
            .collect()
            .await;

        let mut texts = Vec::new();
        let mut dones = 0;
        for ev in &events {
            match ev {
                Ok(StreamEvent::Text(t)) => texts.push(t.clone()),
                Ok(StreamEvent::Done(u)) => {
                    dones += 1;
                    assert_eq!(u.input_tokens, 7);
                    assert_eq!(u.output_tokens, 3);
                    assert_eq!(u.cost, Some(0.5));
                }
                _ => {}
            }
        }
        assert_eq!(texts, vec!["hello".to_string()]);
        assert_eq!(dones, 1, "exactly one Done: {events:?}");
        assert!(
            matches!(events.last(), Some(Ok(StreamEvent::Done(_)))),
            "Done must be last: {events:?}"
        );
    }
}

/// Resume behaviour against a recording fake CLI (no real `claude` needed).
#[cfg(all(test, unix))]
mod resume_tests {
    use std::sync::Mutex;

    use super::*;
    use crate::backend::{CliCapabilities, CliProbe, CliStreamState, TurnScratch};
    use bebok_llm::ChatMessage;

    /// When the fake pretends to have lost the conversation.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Lose {
        Never,
        /// Only turns that try to resume fail.
        OnResume,
        /// Every turn fails, resumed or not.
        Always,
    }

    /// Each turn's `(resume key, prompt)`, in order.
    type Calls = Arc<Mutex<Vec<(Option<String>, String)>>>;

    struct RecordingBackend {
        calls: Calls,
        lose: Lose,
    }

    impl CliBackend for RecordingBackend {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn label(&self) -> &'static str {
            "Fake CLI"
        }
        fn capabilities(&self) -> CliCapabilities {
            CliCapabilities {
                resume: true,
                ..CliCapabilities::default()
            }
        }
        fn default_models(&self) -> Vec<String> {
            vec!["sonnet".into()]
        }
        fn binary_names(&self) -> &'static [&'static str] {
            &["sh"]
        }
        fn env_override(&self) -> &'static str {
            "BEBOK_FAKE_CLI_BIN"
        }
        fn resume_flag_available(&self, _probe: &CliProbe) -> bool {
            true
        }
        fn build_command(
            &self,
            _spec: &CliSpec,
            req: &CliRequest,
            _probe: &CliProbe,
            _scratch: &mut TurnScratch,
        ) -> Result<(String, Vec<String>), CliError> {
            self.calls
                .lock()
                .unwrap()
                .push((req.resume_key.clone(), req.prompt.clone()));
            let lost = match self.lose {
                Lose::Never => false,
                Lose::OnResume => req.resume_key.is_some(),
                Lose::Always => true,
            };
            let script = if lost {
                "echo 'No conversation found with session ID: s-old' >&2; exit 1".to_string()
            } else {
                // A resumed conversation keeps its id; a fresh one gets a new id.
                let key = req.resume_key.clone().unwrap_or_else(|| "s-new".into());
                let jsonl = format!(
                    concat!(
                        r#"{{"type":"system","subtype":"init","session_id":"{k}"}}"#,
                        "\n",
                        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"reply"}}]}}}}"#,
                        "\n",
                        r#"{{"type":"result","subtype":"success","session_id":"{k}","usage":{{"input_tokens":1,"output_tokens":1}}}}"#,
                        "\n",
                    ),
                    k = key
                );
                format!("printf '%s' \"{}\"", jsonl.replace('"', "\\\""))
            };
            Ok(("sh".to_string(), vec!["-c".to_string(), script]))
        }
        fn parse_line(
            &self,
            line: &str,
            st: &mut CliStreamState,
        ) -> Result<Option<CliEvent>, CliError> {
            crate::backends::backend_for("claude")
                .expect("claude backend")
                .parse_line(line, st)
        }
    }

    fn spec() -> CliSpec {
        CliSpec {
            backend: "fake".into(),
            command: None,
            args: Vec::new(),
            mode: crate::backend::CliMode::Agent,
            allow_write: false,
            timeout_secs: 30,
            max_turns: None,
            profile: Default::default(),
            profile_overrides: None,
            system_prompt_mode: Default::default(),
        }
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage {
            role: bebok_llm::ChatRole::Assistant,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            content_parts: Vec::new(),
        }
    }

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: "fake/sonnet".into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            max_tokens: 128,
            thinking: bebok_llm::Thinking::Off,
            sampling: bebok_llm::Sampling::default(),
        }
    }

    async fn run(
        provider: &CliProvider,
        messages: Vec<ChatMessage>,
    ) -> Vec<StreamResult<StreamEvent>> {
        provider
            .stream(request(messages))
            .await
            .expect("streams")
            .collect()
            .await
    }

    fn assert_one_final_done(events: &[StreamResult<StreamEvent>]) {
        let dones = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::Done(_))))
            .count();
        assert_eq!(dones, 1, "exactly one Done: {events:?}");
        assert!(
            matches!(events.last(), Some(Ok(StreamEvent::Done(_)))),
            "Done must be last"
        );
    }

    fn provider(calls: &Calls, lose: Lose, sink: &CliSessionSink) -> CliProvider {
        CliProvider::new(
            Arc::new(RecordingBackend {
                calls: calls.clone(),
                lose,
            }),
            spec(),
            PathBuf::from("/tmp"),
        )
        .with_session_sink(Some(sink.clone()))
    }

    fn resume(from_msg: usize, profile_hash: &str) -> Option<CliResume> {
        Some(CliResume {
            key: "s-new".into(),
            from_msg,
            profile_hash: profile_hash.into(),
        })
    }

    #[tokio::test]
    async fn second_turn_sends_only_the_new_messages() {
        let calls = Calls::default();
        let sink = CliSessionSink::default();

        let first = provider(&calls, Lose::Never, &sink);
        let events = run(&first, vec![ChatMessage::user("one")]).await;
        assert_one_final_done(&events);
        let report = sink.lock().unwrap().take().expect("turn reported");
        assert_eq!(report.key.as_deref(), Some("s-new"));

        // The CLI has seen both the user message and the assistant reply.
        let second = provider(&calls, Lose::Never, &sink).with_resume(resume(2, ""));
        let events = run(
            &second,
            vec![
                ChatMessage::user("one"),
                assistant("reply"),
                ChatMessage::user("two"),
            ],
        )
        .await;
        assert_one_final_done(&events);

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2, "one call per turn: {recorded:?}");
        assert_eq!(recorded[1].0.as_deref(), Some("s-new"), "resumes the key");
        assert!(
            recorded[1].1.contains("two"),
            "new message sent: {recorded:?}"
        );
        assert!(
            !recorded[1].1.contains("one") && !recorded[1].1.contains("reply"),
            "earlier messages not re-sent: {recorded:?}"
        );
    }

    #[tokio::test]
    async fn a_changed_profile_starts_over_with_the_full_transcript() {
        let calls = Calls::default();
        let sink = CliSessionSink::default();
        // The recorded profile digest differs from this turn's (none: the fake
        // has no profile writer), so the conversation must not be resumed.
        let stale = provider(&calls, Lose::Never, &sink).with_resume(resume(2, "old-profile"));
        let events = run(
            &stale,
            vec![
                ChatMessage::user("one"),
                assistant("reply"),
                ChatMessage::user("two"),
            ],
        )
        .await;
        assert_one_final_done(&events);

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, None, "no --resume after a profile change");
        assert!(recorded[0].1.contains("one") && recorded[0].1.contains("two"));
    }

    #[tokio::test]
    async fn a_lost_conversation_is_retried_once_with_the_full_transcript() {
        let calls = Calls::default();
        let sink = CliSessionSink::default();
        let first = provider(&calls, Lose::OnResume, &sink);
        run(&first, vec![ChatMessage::user("one")]).await;

        let second = provider(&calls, Lose::OnResume, &sink).with_resume(resume(2, ""));
        let events = run(
            &second,
            vec![
                ChatMessage::user("one"),
                assistant("reply"),
                ChatMessage::user("two"),
            ],
        )
        .await;
        assert_one_final_done(&events);
        assert!(
            events.iter().all(|e| e.is_ok()),
            "the retry hides the lost conversation: {events:?}"
        );

        let recorded = calls.lock().unwrap().clone();
        // fresh turn, the resumed attempt, then exactly one full-transcript retry.
        assert_eq!(recorded.len(), 3, "{recorded:?}");
        assert_eq!(recorded[1].0.as_deref(), Some("s-new"));
        assert_eq!(recorded[2].0, None, "retry drops the key");
        assert!(recorded[2].1.contains("one") && recorded[2].1.contains("two"));
        let report = sink.lock().unwrap().take().expect("turn reported");
        assert_eq!(report.key.as_deref(), Some("s-new"));
    }

    #[tokio::test]
    async fn a_second_lost_conversation_is_reported_not_retried_again() {
        let calls = Calls::default();
        let sink = CliSessionSink::default();
        let second = provider(&calls, Lose::Always, &sink).with_resume(resume(2, ""));
        let events = run(
            &second,
            vec![
                ChatMessage::user("one"),
                assistant("reply"),
                ChatMessage::user("two"),
            ],
        )
        .await;

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            2,
            "resumed attempt + one retry: {recorded:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Err(LlmError::Provider(m)) if m.contains("no longer has"))),
            "error surfaces: {events:?}"
        );
    }
}
