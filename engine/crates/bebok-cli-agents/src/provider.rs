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
use std::sync::Arc;

use bebok_llm::{ChatRequest, LlmError, Provider, StreamEvent, StreamResult, Usage};
use futures::stream::{BoxStream, StreamExt};

use crate::backend::{CliBackend, CliError, CliEvent, CliRequest};
use crate::spec::CliSpec;

/// A provider that drives an agentic CLI.
pub struct CliProvider {
    backend: Arc<dyn CliBackend>,
    spec: CliSpec,
    cwd: PathBuf,
}

impl CliProvider {
    pub fn new(backend: Arc<dyn CliBackend>, spec: CliSpec, cwd: PathBuf) -> Self {
        Self { backend, spec, cwd }
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
}

/// How a CLI event is presented to the agent loop.
///
/// Returns `None` for events that are bookkeeping rather than output (the
/// session key is remembered by the caller, not shown).
fn present(ev: CliEvent) -> Option<StreamEvent> {
    match ev {
        CliEvent::Text(t) => Some(StreamEvent::Text(t)),
        CliEvent::Thinking(t) => Some(StreamEvent::Thinking(t)),
        // Presentation only. A `StreamEvent::ToolCall` here would be executed
        // by `bebok-core`'s agent loop against a registry that never defined
        // the CLI's tools — see this module's docs and `crate::backend`.
        CliEvent::ToolActivity { name, detail } => {
            Some(StreamEvent::Thinking(format!("⚙ {name}: {detail}")))
        }
        CliEvent::Usage(_) | CliEvent::SessionKey(_) => None,
    }
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
}

impl RunState {
    /// Await the child, then decide what the tail of the stream looks like.
    async fn finalise(&mut self, stream_error: Option<LlmError>) {
        let exit_result = match self.exit.take() {
            Some(handle) => handle.await,
            // Already reaped (or never created): nothing to add.
            None => Ok(Ok(None)),
        };

        let mut error: Option<LlmError> = stream_error;
        if error.is_none() {
            match exit_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => error = Some(LlmError::from(e)),
                Err(join) => {
                    error = Some(LlmError::Provider(format!("CLI supervisor failed: {join}")));
                }
            }
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

        let cli_req = CliRequest {
            prompt: crate::transcript::render_prompt(&req),
            system,
            model,
            cwd: self.cwd.clone(),
            // v1 is stateless: the transcript carries the whole conversation.
            // TODO: persist the CLI's SessionKey under
            // `<project>/.bebok/cli/<session>.json` and resume from it, so the
            // transcript is not re-sent every turn.
            resume_key: None,
            // `allow_write` is the single opt-in for letting the CLI mutate the
            // tree; it bypasses Bebok's permission engine and change tracking,
            // so it stays false unless the user asked for it in config.
            write: self.spec.allow_write,
            max_turns: self.spec.max_turns,
            timeout: self.spec.timeout(),
        };

        let run = crate::runner::spawn_cli(self.backend.clone(), self.spec.clone(), cli_req)
            .await
            .map_err(LlmError::from)?;

        let state = RunState {
            inner: run.events,
            exit: Some(run.exit),
            usage: Usage::default(),
            saw_usage: false,
            queue: VecDeque::new(),
            finalised: false,
        };

        let stream = futures::stream::unfold(Some(state), |slot: Option<RunState>| async move {
            let mut st = slot?;

            loop {
                if let Some(item) = st.queue.pop_front() {
                    return Some((item, Some(st)));
                }
                if st.finalised {
                    // Queue drained and nothing left to say: end the stream.
                    return None;
                }

                match st.inner.next().await {
                    Some(Ok(ev)) => {
                        if let Some(key) = session_key_of(&ev) {
                            // Remembered for the eventual `--resume` work; not
                            // shown to the user.
                            tracing::debug!(session = %key, "cli session key");
                        }
                        if let CliEvent::Usage(u) = ev {
                            merge_usage(&mut st.usage, u);
                            st.saw_usage = true;
                            continue;
                        }
                        if let Some(event) = present(ev) {
                            return Some((Ok(event), Some(st)));
                        }
                        // Bookkeeping event: keep reading.
                        continue;
                    }
                    Some(Err(e)) => {
                        st.finalise(Some(LlmError::from(e))).await;
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

/// The session key carried by an event, if any (borrowing helper so `present`
/// can stay consuming).
fn session_key_of(ev: &CliEvent) -> Option<&str> {
    match ev {
        CliEvent::SessionKey(k) => Some(k.as_str()),
        _ => None,
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
    let cli_spec = CliSpec::from_extra(&spec.extra).map_err(|e| e.to_string())?;

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

    Ok(Arc::new(CliProvider::new(backend, cli_spec, root)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CliCapabilities, CliStreamState};

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
            },
        );
        assert_eq!(total.input_tokens, 11);
        assert_eq!(total.output_tokens, 7);
        assert_eq!(total.cost, Some(0.02));
        assert_eq!(total.cache_read_input_tokens, Some(7));
        assert_eq!(total.cache_creation_input_tokens, Some(7));
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
            fn build_command(&self, _spec: &CliSpec, _req: &CliRequest) -> (String, Vec<String>) {
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
                (
                    "sh".to_string(),
                    vec![
                        "-c".to_string(),
                        format!("printf '%s' \"{}\"", jsonl.replace('"', "\\\"")),
                    ],
                )
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
