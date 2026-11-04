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

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

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

/// One turn handed to a CLI backend.
#[derive(Debug, Clone)]
pub struct CliRequest {
    /// The prompt: the rendered transcript (see [`crate::transcript`]).
    pub prompt: String,
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
    #[error("invalid CLI provider config: {0}")]
    Config(String),
}

impl CliError {
    /// True when retrying the same turn could succeed. A missing binary or a
    /// bad config never becomes good on the second attempt — and the agent loop
    /// retries transient errors 3x, which would triple the wait for nothing.
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
    /// Build `(program, args)` for one turn.
    fn build_command(&self, spec: &crate::spec::CliSpec, req: &CliRequest)
    -> (String, Vec<String>);
    /// Parse one stdout line; `Ok(None)` = nothing to emit.
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
}
