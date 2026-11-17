//! Provider factory (Factory): build a provider client for a `provider/model`
//! string. Lives in core (not the server) so core-side features — e.g. the
//! sub-agent `task` tool — can build providers without depending on the
//! server layer.
//!
//! Selection is by model prefix (`zai/...`, `openai/...`, `anthropic/...`,
//! `xai/...`, `deepseek/...`, `google/...`, `openrouter/...`, `ollama/...` or
//! any provider name from config). The API key comes from the provider spec
//! (`api_key` field or the provider env var) with a final fallback to the
//! resolved `config.api_key`.
//!
//! Two shapes of provider are built here:
//! - HTTP (`openai` / `anthropic`) — delegated to [`bebok_llm::build_provider`];
//! - `kind: "cli"` — an agentic CLI driven as a transport, built by
//!   `bebok-cli-agents`. That needs the project root (the CLI's `current_dir`),
//!   which is why [`build_provider_for`] takes a [`ProviderCtx`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bebok_cli_agents::{CliResume, CliSessionSink};
use bebok_llm::{Provider, ProviderKind};
use sha2::{Digest, Sha256};

use crate::config::{ResolvedConfig, provider_from_model};
use crate::error::CoreError;
use crate::session::{CliSessionMeta, Message, Part};

/// A CLI conversation the next turn may continue (see [`cli_resume_hint`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeHint {
    /// The CLI's conversation id.
    pub key: String,
    /// First session message the CLI has not seen; the turn sends from here.
    pub from_msg: usize,
    /// Profile digest the conversation was recorded with.
    pub profile_hash: String,
}

/// Everything a provider may need beyond the config: where to run, the
/// per-turn environment and, for CLI providers, the conversation to continue.
///
/// - [`ProviderCtx::none`] preserves the pre-CLI behaviour (HTTP providers only);
/// - [`ProviderCtx::root`] is what every real turn uses.
#[derive(Debug, Clone, Default)]
pub struct ProviderCtx {
    pub project_root: Option<PathBuf>,
    /// Extra environment a CLI provider must pass to its child for this turn:
    /// the hook endpoint and its turn-scoped token (`cli_turn_token`), and any
    /// profile override (`CLAUDE_CONFIG_DIR` in `isolated` mode). Ignored by
    /// HTTP providers.
    pub hook_env: Vec<(String, String)>,
    /// CLI conversation to continue on this turn. Ignored by HTTP providers.
    pub resume: Option<ResumeHint>,
    /// Receives the CLI conversation key at turn end. Ignored by HTTP providers.
    pub session_sink: Option<CliSessionSink>,
}

impl ProviderCtx {
    /// No project root: CLI providers cannot be built from here.
    pub fn none() -> Self {
        Self::default()
    }

    /// Run CLI providers in `root`.
    pub fn root(root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: Some(root.into()),
            ..Self::default()
        }
    }

    /// Add the per-turn environment the CLI child must see (see `hook_env`).
    pub fn with_hook_env(mut self, env: Vec<(String, String)>) -> Self {
        self.hook_env = env;
        self
    }

    /// Continue the CLI conversation described by `hint` (see [`ResumeHint`]).
    pub fn with_resume(mut self, hint: Option<ResumeHint>) -> Self {
        self.resume = hint;
        self
    }

    /// Have the CLI provider report its conversation key into `sink`.
    pub fn with_session_sink(mut self, sink: CliSessionSink) -> Self {
        self.session_sink = Some(sink);
        self
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }
}

/// Build the provider for a model (see module docs for selection rules).
///
/// Kept for call sites that only ever see HTTP providers; equivalent to
/// [`build_provider_for`] with [`ProviderCtx::none`].
pub fn build_provider(
    config: &ResolvedConfig,
    model: &str,
) -> Result<Arc<dyn Provider>, CoreError> {
    build_provider_for(config, model, &ProviderCtx::none())
}

/// Build the provider for a model, telling CLI providers where to run.
pub fn build_provider_for(
    config: &ResolvedConfig,
    model: &str,
    ctx: &ProviderCtx,
) -> Result<Arc<dyn Provider>, CoreError> {
    let name = provider_from_model(model);
    let spec = config
        .provider_spec(&name)
        .ok_or_else(|| CoreError::ProviderConfig(format!("unknown provider '{name}'")))?;

    if spec.kind == ProviderKind::Cli {
        // A CLI runs in a project directory; without one there is nothing
        // coherent to point it at, and guessing a cwd would let it read or
        // edit the wrong tree. Failing loudly beats a surprise.
        let root = ctx.project_root().ok_or_else(|| {
            CoreError::ProviderConfig(format!(
                "provider '{name}' is a CLI provider but no project root was supplied"
            ))
        })?;
        let resume = ctx.resume.as_ref().map(|hint| CliResume {
            key: hint.key.clone(),
            from_msg: hint.from_msg,
            profile_hash: hint.profile_hash.clone(),
        });
        return bebok_cli_agents::build_provider_for_turn(
            &spec,
            Some(root.to_path_buf()),
            ctx.hook_env.clone(),
            resume,
            ctx.session_sink.clone(),
        )
        .map_err(CoreError::ProviderConfig);
    }

    bebok_llm::build_provider(
        &spec,
        config.api_key.clone().filter(|s| !s.trim().is_empty()),
    )
    .map_err(CoreError::ProviderConfig)
}

/// Digest of a system prompt, as recorded in [`CliSessionMeta::system_hash`].
pub fn system_hash(system: &str) -> String {
    Sha256::digest(system.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Decide whether the next CLI turn can continue the recorded conversation.
///
/// `messages` is the transcript *including* the new user message. Resuming
/// requires the same provider, model and system prompt, a non-empty tail, and
/// a prefix in which every message maps to exactly one request message: a
/// tool call expands into an extra synthetic message (see
/// `agent::request`), which would make the recorded index point at the wrong
/// place. Anything else returns `None` and the full transcript is sent.
pub fn cli_resume_hint(
    recorded: Option<&CliSessionMeta>,
    provider: &str,
    model: &str,
    system_hash_now: &str,
    messages: &[Message],
) -> Option<ResumeHint> {
    let recorded = recorded?;
    if recorded.key.trim().is_empty()
        || recorded.provider != provider
        || recorded.model != model
        || recorded.system_hash != system_hash_now
        || recorded.msg_index >= messages.len()
    {
        return None;
    }
    let prefix_is_plain = messages[..recorded.msg_index]
        .iter()
        .all(|m| !m.parts.iter().any(|p| matches!(p, Part::Tool { .. })));
    if !prefix_is_plain {
        return None;
    }
    Some(ResumeHint {
        key: recorded.key.clone(),
        from_msg: recorded.msg_index,
        profile_hash: recorded.profile_hash.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_none_has_no_root_and_root_sets_one() {
        assert!(ProviderCtx::none().project_root().is_none());
        let ctx = ProviderCtx::root("/tmp/proj");
        assert_eq!(ctx.project_root(), Some(Path::new("/tmp/proj")));
        assert!(ctx.hook_env.is_empty());
    }

    fn recorded(msg_index: usize) -> CliSessionMeta {
        CliSessionMeta {
            key: "conv-1".into(),
            provider: "claude-cli".into(),
            model: "claude-cli/sonnet".into(),
            system_hash: system_hash("sys"),
            msg_index,
            profile_hash: "prof".into(),
        }
    }

    fn plain_transcript(n: usize) -> Vec<Message> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("u{i}"))
                } else {
                    Message::assistant_with("code", "claude-cli/sonnet")
                }
            })
            .collect()
    }

    #[test]
    fn system_hash_is_stable_and_follows_the_prompt() {
        assert_eq!(system_hash("sys"), system_hash("sys"));
        assert_ne!(system_hash("sys"), system_hash("sys2"));
        assert_eq!(system_hash("").len(), 64);
    }

    #[test]
    fn matching_record_resumes_from_the_recorded_index() {
        let messages = plain_transcript(5);
        let hint = cli_resume_hint(
            Some(&recorded(4)),
            "claude-cli",
            "claude-cli/sonnet",
            &system_hash("sys"),
            &messages,
        )
        .expect("resumes");
        assert_eq!(hint.key, "conv-1");
        assert_eq!(hint.from_msg, 4);
        assert_eq!(hint.profile_hash, "prof");
    }

    #[test]
    fn any_change_of_provider_model_or_system_prompt_starts_over() {
        let messages = plain_transcript(5);
        let sys = system_hash("sys");
        let meta = recorded(4);
        assert!(
            cli_resume_hint(Some(&meta), "other", "claude-cli/sonnet", &sys, &messages).is_none()
        );
        assert!(
            cli_resume_hint(
                Some(&meta),
                "claude-cli",
                "claude-cli/opus",
                &sys,
                &messages
            )
            .is_none()
        );
        assert!(
            cli_resume_hint(
                Some(&meta),
                "claude-cli",
                "claude-cli/sonnet",
                &system_hash("changed"),
                &messages
            )
            .is_none()
        );
    }

    #[test]
    fn no_record_or_an_empty_tail_or_an_empty_key_means_no_resume() {
        let messages = plain_transcript(5);
        let sys = system_hash("sys");
        assert!(
            cli_resume_hint(None, "claude-cli", "claude-cli/sonnet", &sys, &messages).is_none()
        );
        // The record claims the CLI has seen everything, leaving nothing to send.
        assert!(
            cli_resume_hint(
                Some(&recorded(5)),
                "claude-cli",
                "claude-cli/sonnet",
                &sys,
                &messages
            )
            .is_none()
        );
        let mut blank = recorded(4);
        blank.key = " ".into();
        assert!(
            cli_resume_hint(
                Some(&blank),
                "claude-cli",
                "claude-cli/sonnet",
                &sys,
                &messages
            )
            .is_none()
        );
    }

    #[test]
    fn a_tool_call_before_the_record_disables_resume() {
        // A tool call expands into a synthetic request message, so the recorded
        // index would no longer point at the same place in the request.
        let mut messages = plain_transcript(5);
        messages[2].add_tool_call("t1".into(), "read_file".into(), serde_json::json!({}));
        assert!(
            cli_resume_hint(
                Some(&recorded(4)),
                "claude-cli",
                "claude-cli/sonnet",
                &system_hash("sys"),
                &messages
            )
            .is_none()
        );
    }

    #[test]
    fn hook_env_is_carried_per_turn() {
        let ctx = ProviderCtx::root("/tmp/proj").with_hook_env(vec![(
            "BEBOK_HOOK_URL".to_string(),
            "http://127.0.0.1:9/cli-hook/x".to_string(),
        )]);
        assert_eq!(ctx.hook_env.len(), 1);
        assert_eq!(ctx.hook_env[0].0, "BEBOK_HOOK_URL");
    }
}
