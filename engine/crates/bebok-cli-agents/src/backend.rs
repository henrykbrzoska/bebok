//! The CLI-backend contract.
//!
//! An agentic CLI (Claude Code and friends) performs its own tool use: it runs
//! Bash, edits files, reads the tree. It cannot be handed Bebok's tool registry
//! the way a [`bebok_llm::ChatRequest`] hands over `tools`. So a CLI is a
//! *transport* provider: its tool activity is a presentation stream, never
//! [`bebok_llm::StreamEvent::ToolCall`] (the agent loop would try to execute
//! those through `exec.rs` and fail the turn).
//!
//! Adding a new CLI is meant to be one file under [`crate::backends`] plus a
//! registry line — see `docs/cli-agents.md`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::profile::{ProfileInput, ProfileWriter};

/// What a backend can do. The runner and the provider consult these instead of
/// probing the binary at call time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CliCapabilities {
    /// Emits structured per-line events (JSONL) instead of one blob.
    pub streaming: bool,
    /// Can continue a previous conversation (`--resume` / `--continue`).
    pub resume: bool,
    /// Accepts an appended system prompt.
    pub system_prompt: bool,
    /// Emits structural tool events we can render (still not `ToolCall`).
    pub tool_events: bool,
    /// Reports token usage / cost.
    pub usage: bool,
    /// Accepts image inputs.
    pub images: bool,
    /// Can mount MCP servers (phase 4).
    pub mcp: bool,
}

impl Default for CliCapabilities {
    fn default() -> Self {
        Self {
            streaming: true,
            resume: false,
            system_prompt: true,
            tool_events: false,
            usage: false,
            images: false,
            mcp: false,
        }
    }
}

/// How the CLI should be driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CliMode {
    /// The CLI runs its own agent loop over the project (tools enabled).
    #[default]
    Agent,
    /// Single-shot completion: text in, text out, no tool use.
    Text,
}

/// One piece of a prompt that can travel as structured content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptBlock {
    Text(String),
    /// Base64 payload, already validated upstream (`agent/images.rs`).
    Image {
        media_type: String,
        data: String,
    },
}

/// What the installed binary says about itself, read once per binary (see
/// [`crate::probe`]). Backends gate every flag they are not certain of on
/// [`CliProbe::has`] or [`CliProbe::mentions`], so an older or newer CLI gets
/// the conservative argv path instead of a flag it rejects.
#[derive(Debug, Clone, Default)]
pub struct CliProbe {
    features: crate::probe::CliFeatures,
}

impl CliProbe {
    /// A probe built from help text alone (no version). Used by tests and by
    /// callers that only hold the help output.
    pub fn new(help: String) -> Self {
        Self {
            features: crate::probe::CliFeatures::from_outputs(None, &help),
        }
    }

    pub fn from_features(features: crate::probe::CliFeatures) -> Self {
        Self { features }
    }

    /// The full feature set, for the profile lock and diagnostics.
    pub fn features(&self) -> &crate::probe::CliFeatures {
        &self.features
    }

    /// True when the help text contains `needle`. An empty probe (the binary
    /// did not answer `--help`) mentions nothing.
    pub fn mentions(&self, needle: &str) -> bool {
        self.features.mentions(needle)
    }

    /// True when the help lists `flag` as an option of its own.
    pub fn has(&self, flag: &str) -> bool {
        self.features.has(flag)
    }

    /// Case-insensitive substring match over the help text.
    pub fn mentions_ci(&self, needle: &str) -> bool {
        self.features.mentions_ci(needle)
    }
}

/// Files a single turn must leave on disk while the CLI runs (for example a
/// system prompt passed by path). The directory is created lazily and removed
/// on drop; the runner keeps the value alive for as long as the process
/// stream, so the CLI can read its files at startup.
#[derive(Debug, Default)]
pub struct TurnScratch {
    dir: Option<PathBuf>,
}

impl TurnScratch {
    /// Write `contents` to `name` inside this turn's directory and return the
    /// absolute path.
    pub fn write_file(&mut self, name: &str, contents: &str) -> Result<PathBuf, CliError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = match &self.dir {
            Some(dir) => dir.clone(),
            None => {
                let unique = format!(
                    "bebok-cli-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                let dir = std::env::temp_dir().join(unique);
                // `create_dir`, not `create_dir_all`: an existing directory is
                // refused rather than reused, so a pre-planted one is never
                // written into.
                std::fs::create_dir(&dir)
                    .map_err(|e| CliError::Scratch(format!("{}: {e}", dir.display())))?;
                self.dir = Some(dir.clone());
                dir
            }
        };
        let path = dir.join(name);
        std::fs::write(&path, contents)
            .map_err(|e| CliError::Scratch(format!("{}: {e}", path.display())))?;
        Ok(path)
    }
}

impl Drop for TurnScratch {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// One turn handed to a CLI backend.
#[derive(Debug, Clone)]
pub struct CliRequest {
    /// The prompt as plain text: the rendered transcript (see
    /// [`crate::transcript`]), with images replaced by placeholders. Used when
    /// the CLI takes the prompt on its command line.
    pub prompt: String,
    /// The same prompt as structured blocks, images included. Used when the
    /// CLI takes the prompt on stdin (see [`CliBackend::stdin_payload`]).
    pub blocks: Vec<PromptBlock>,
    /// System prompt to append, when the backend supports one.
    pub system: Option<String>,
    /// Model id as the CLI understands it (`opus`, `sonnet`, ...).
    pub model: Option<String>,
    /// Project root; always the CLI's `current_dir`.
    pub cwd: PathBuf,
    /// Conversation id on the CLI side, for `--resume`.
    pub resume_key: Option<String>,
    /// Whether the CLI may modify files. Default is **false**: a CLI's edits
    /// bypass Bebok's permission engine and change tracking, so writing is
    /// opt-in.
    pub write: bool,
    pub max_turns: Option<u32>,
    pub timeout: Duration,
    /// Extra environment variables for this turn (e.g. the hook endpoint and
    /// its turn-scoped token, see `bebok_core::cli_turn_token`). Merged into
    /// the scrubbed inherited environment by the runner.
    pub extra_env: Vec<(String, String)>,
}

/// Where the managed CLI profile lives.
///
/// `dir: None` means "the default location for this project and backend" (see
/// `ProfileDir::resolve` in `profile.rs`). How the CLI is layered on top of the
/// user's own settings is decided by `CliSpec::profile`, not here, so there is
/// one source of truth for the mode.
#[derive(Debug, Clone, Default)]
pub struct ProfileDir {
    /// Explicit profile directory. `None` selects the default location.
    pub dir: Option<PathBuf>,
}

/// Everything a backend prepares for one turn beyond `build_command`'s argv:
/// the extra environment, extra argv and the stdin payload.
///
/// The runner consumes this as the single source of truth for the child's
/// environment (after scrubbing). Keeping it a separate value lets a backend
/// assemble env/argv from the profile without threading half a dozen
/// parameters through `build_command`.
#[derive(Debug, Default)]
pub struct PreparedTurn {
    /// Extra argv appended to the built command (empty for now: `build_command`
    /// stays the authority on argv).
    pub args_extra: Vec<String>,
    /// Environment variables for the child (merged over the scrubbed inherited
    /// environment; these win on a name collision).
    pub env: Vec<(String, String)>,
    /// stdin payload, when the backend overrides `stdin_payload` per turn.
    pub stdin: Option<Vec<u8>>,
    /// Digest of the rendered profile this turn runs with (`None` when the
    /// backend has no profile writer). A resumed conversation is only valid
    /// while this digest is unchanged, see `CliSessionMeta::profile_hash`.
    pub profile_hash: Option<String>,
}

/// An event parsed out of the CLI's output.
#[derive(Debug, Clone)]
pub enum CliEvent {
    Text(String),
    /// Reasoning / narration. Tool activity is folded in here in v1 so the
    /// existing client renders it without new message parts.
    Thinking(String),
    /// A tool the CLI ran *itself*. Presentation only — never a `ToolCall`.
    ToolActivity {
        name: String,
        detail: String,
    },
    ExternalTool {
        id: String,
        name: String,
        input: serde_json::Value,
        output: String,
        is_error: bool,
    },
    Usage(bebok_llm::Usage),
    /// Conversation id to reuse on the next turn.
    SessionKey(String),
}

/// Mutable per-stream parse state (a backend often needs to remember whether it
/// already emitted a partial chunk, and the last session id it saw).
#[derive(Debug, Default, Clone)]
pub struct CliStreamState {
    pub session_key: Option<String>,
    /// Partial line buffer for backends that do not emit one JSON object per
    /// line.
    pub buffer: String,
    /// Events one line produced beyond the one `parse_line` returned. The
    /// runner delivers them, in order, right after that event.
    pub pending: VecDeque<CliEvent>,
    /// Set when the backend reports the run itself as failed (for example a
    /// `result` with `is_error`). The runner delivers it as
    /// [`CliError::RunFailed`] after `pending`, so usage reported on the same
    /// line is still accounted for.
    pub failure: Option<String>,
    /// Id of the message whose partial deltas were streamed, if any.
    pub message_id: Option<String>,
    /// Text and thinking blocks of `message_id` already shown from deltas, so
    /// the full message that follows does not show them a second time.
    pub streamed: Vec<String>,
    /// Text of the content block currently streaming, if it is text/thinking.
    pub block: Option<String>,
    /// Prompt size of the latest API call (input plus cache buckets), from the
    /// latest `assistant` message's usage.
    pub context_tokens: Option<u64>,
    /// Pending tool_use blocks keyed by tool_use_id, for fallback pairing with tool_result.
    pub pending_tools: std::collections::HashMap<String, (String, serde_json::Value)>,
}

/// Errors a backend or the runner can produce.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("CLI backend '{0}' is not registered")]
    UnknownBackend(String),
    #[error("CLI binary not found (looked for {names}); set {env_var} to point at it")]
    BinaryMissing { names: String, env_var: String },
    #[error("failed to spawn CLI: {0}")]
    Spawn(String),
    #[error("CLI exited with status {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("CLI timed out after {0:?}")]
    Timeout(Duration),
    #[error("CLI stream error: {0}")]
    Stream(String),
    /// The CLI reported the run as failed (`result` with `is_error`, or an
    /// `error_*` subtype). The message is meant for the user.
    #[error("CLI run failed: {0}")]
    RunFailed(String),
    #[error("cannot prepare files for the CLI turn: {0}")]
    Scratch(String),
    #[error("invalid CLI provider config: {0}")]
    Config(String),
    /// The CLI no longer knows the conversation this turn tried to resume
    /// (purged, or a different binary). Never retried as-is: the caller sends
    /// the full transcript instead.
    #[error("the CLI no longer has the resumed conversation: {0}")]
    ResumeLost(String),
}

impl CliError {
    /// True when retrying the same turn could succeed. A missing binary or a
    /// bad config never becomes good on the second attempt — and the agent loop
    /// retries transient errors 3x, which would triple the wait for nothing.
    /// A run the CLI itself reported as failed is not retried either: the same
    /// prompt would most likely hit the same limit again.
    pub fn is_transient(&self) -> bool {
        matches!(self, CliError::Stream(_))
    }
}

impl From<CliError> for bebok_llm::LlmError {
    fn from(e: CliError) -> Self {
        // Everything a CLI does is either a config or a process failure; there
        // is no HTTP status to classify. `Provider` maps to the right
        // non-transient `ProviderErrorKind` in `LlmError::kind()`.
        bebok_llm::LlmError::Provider(e.to_string())
    }
}

/// The trait every CLI backend implements. Implementing it plus one registry
/// line is the whole cost of adding a CLI.
pub trait CliBackend: Send + Sync {
    /// Stable id used in config (`extra.backend`).
    fn id(&self) -> &'static str;
    /// Human label for the settings card.
    fn label(&self) -> &'static str;
    fn capabilities(&self) -> CliCapabilities;
    /// Model ids offered when a provider entry lists none.
    fn default_models(&self) -> Vec<String>;
    /// Executable names to look up on `PATH`.
    fn binary_names(&self) -> &'static [&'static str];
    /// Environment variable that overrides the binary path.
    fn env_override(&self) -> &'static str;
    /// Build `(program, args)` for one turn. `probe` says what the installed
    /// binary supports; files the CLI must read go through `scratch`.
    fn build_command(
        &self,
        spec: &crate::spec::CliSpec,
        req: &CliRequest,
        probe: &CliProbe,
        scratch: &mut TurnScratch,
    ) -> Result<(String, Vec<String>), CliError>;
    /// Profile generator for this backend, when it supports managed profiles.
    fn profile_writer(&self) -> Option<&dyn ProfileWriter> {
        None
    }
    /// Everything beyond argv that this turn needs: the managed profile files
    /// (written only when their content hash changed), the profile's extra argv
    /// and env, and the caller's `req.extra_env` (hook endpoint + turn token).
    ///
    /// Default for a backend without a profile writer: forward `req.extra_env`
    /// untouched. A backend with a writer gets profile generation for free.
    fn prepare(
        &self,
        spec: &crate::spec::CliSpec,
        req: &CliRequest,
        probe: &CliProbe,
        profile: &ProfileDir,
    ) -> Result<PreparedTurn, CliError> {
        let mut env = req.extra_env.clone();
        let Some(writer) = self.profile_writer() else {
            return Ok(PreparedTurn {
                args_extra: Vec::new(),
                env,
                stdin: None,
                profile_hash: None,
            });
        };
        let dir = profile.resolve(&req.cwd, self.id())?;
        let rendered = writer.render(&ProfileInput {
            spec,
            req,
            probe,
            dir: &dir,
        })?;
        crate::profile::sync_profile(&dir, &rendered)?;
        let profile_hash = Some(crate::profile::content_hash(&rendered));
        env.extend(rendered.env.iter().cloned());
        Ok(PreparedTurn {
            args_extra: rendered.args,
            env,
            stdin: None,
            profile_hash,
        })
    }
    /// Whether the installed binary can continue a conversation by key
    /// (`--resume`). Probe-gated: a binary that cannot resume gets the full
    /// transcript every turn, never a tail that the CLI would read as a whole.
    fn resume_flag_available(&self, _probe: &CliProbe) -> bool {
        self.capabilities().resume
    }
    /// Bytes to write to the child's stdin, or `None` to leave stdin closed
    /// (the prompt then travels on the command line). When `Some`, the runner
    /// closes stdin after the write, which is what lets the CLI start.
    fn stdin_payload(&self, _req: &CliRequest, _probe: &CliProbe) -> Option<Vec<u8>> {
        None
    }
    /// Map the CLI's own model name (and the id it reported at startup, if
    /// any) to an id in the model catalog, for context-window lookup.
    fn catalog_model(&self, _cli_model: &str, _init_model: Option<&str>) -> Option<String> {
        None
    }
    /// Parse one stdout line; `Ok(None)` = nothing to emit. Extra events and a
    /// failure go into `st` (see [`CliStreamState::pending`]).
    fn parse_line(&self, line: &str, st: &mut CliStreamState)
    -> Result<Option<CliEvent>, CliError>;
    /// Args for availability probing.
    fn probe_args(&self) -> &'static [&'static str] {
        &["--version"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_default_is_read_only_friendly() {
        let caps = CliCapabilities::default();
        assert!(caps.streaming);
        assert!(!caps.mcp);
    }

    #[test]
    fn only_stream_errors_are_transient() {
        assert!(CliError::Stream("blip".into()).is_transient());
        assert!(
            !CliError::BinaryMissing {
                names: "claude".into(),
                env_var: "BEBOK_CLAUDE_BIN".into()
            }
            .is_transient()
        );
        assert!(
            !CliError::Exit {
                status: 1,
                stderr: String::new()
            }
            .is_transient()
        );
    }

    #[test]
    fn mode_serde_is_lowercase() {
        assert_eq!(serde_json::to_string(&CliMode::Agent).unwrap(), "\"agent\"");
        assert_eq!(serde_json::to_string(&CliMode::Text).unwrap(), "\"text\"");
    }

    #[test]
    fn empty_probe_mentions_nothing() {
        assert!(!CliProbe::default().mentions("--input-format"));
        let probe = CliProbe::new("  --input-format <format>  ...".into());
        assert!(probe.mentions("--input-format"));
        assert!(!probe.mentions("--include-partial-messages"));
    }

    #[test]
    fn turn_scratch_writes_under_a_temp_dir_and_removes_it_on_drop() {
        let mut scratch = TurnScratch::default();
        let path = scratch.write_file("system-prompt.txt", "be terse").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "be terse");
        assert!(path.is_absolute());

        let second = scratch.write_file("other.txt", "x").unwrap();
        assert_eq!(second.parent(), path.parent(), "one directory per turn");

        let dir = path.parent().unwrap().to_path_buf();
        drop(scratch);
        assert!(!dir.exists(), "scratch dir must be removed on drop");
    }
}
