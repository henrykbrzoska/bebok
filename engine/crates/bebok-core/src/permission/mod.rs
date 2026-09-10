//! Permission engine (SPEC §3.6, milestone M2).
//!
//! Tool execution goes through an `allow / ask / deny` gate. Rules are glob
//! patterns over the canonical call string `tool(arg-text)`, e.g. `bash(git *)`,
//! `edit(*)`, or plain tool-name globs such as `mcp__github__*`.
//!
//! Resolution order: agent overrides → project rules → global rules → default
//! (`Allow` for read-only tools, `Ask` for mutating tools). Globs are compiled
//! once per instance (`globset::GlobSet`) and recompiled only when a rule is
//! added (`always allow`) or the config is reloaded.
//!
//! `ask` suspends the agent loop: a oneshot channel is registered per request,
//! a `permission.asked` event reaches the clients, and the loop awaits. The
//! decision endpoint answers through the oneshot; `always` persists a project
//! rule (`ask → allow`) to `<project>/.bebok/config.json`.

pub mod engine;
pub mod matcher;
pub mod rule;
pub mod store;

pub use engine::{
    CachedDecision, CompiledLayer, DecisionKey, Evaluation, PermissionAnswer, PermissionEngine,
    ResolveOutcome, Verdict,
};
pub use matcher::{call_arg_text, call_string};
pub use rule::{Action, Rule, parse_rules};
pub use store::{persist_project_rule, read_rules_from};
