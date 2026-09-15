//! InstanceStore (SPEC §3.10): per-directory runtime state.
//!
//! One engine process serves multiple working directories. Runtime state
//! (sessions, config, tool registry) is keyed by normalized path; sessions are
//! looked up globally by id. Nothing session-critical lives only in RAM: the
//! disk journal is authoritative, RAM is a cache.

pub mod code_index;
pub mod instance;
pub mod instance_store;
pub mod lifecycle;
pub mod session_state;

pub use code_index::{CodeIndexStatus, InstanceCodeIndex};

pub use instance::Instance;
pub use instance_store::InstanceStore;
pub use session_state::{ChildTask, SessionState, TaskResult};

/// F9-9: the model a session runs on when nothing overrides it per prompt:
/// the session's own `model`, else the agent preset's model, else
/// `models.<agent>` / the config default. Used for the `effective_model`
/// field of session DTOs.
pub fn effective_model(
    session: &crate::session::Session,
    instance: &Instance,
    cfg: &crate::config::ResolvedConfig,
) -> String {
    session
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| instance.resolve_agent(&session.agent).model.clone())
        .unwrap_or_else(|| cfg.model_for(&session.agent))
}

/// F9-10: the parent model the delegation policy maps from: the model of
/// the parent's latest LLM call when one happened (`context_model`, which
/// also covers a per-prompt override), else [`effective_model`].
pub fn parent_model_for_delegation(
    session: &crate::session::Session,
    instance: &Instance,
    cfg: &crate::config::ResolvedConfig,
) -> String {
    session
        .context_model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| effective_model(session, instance, cfg))
}
