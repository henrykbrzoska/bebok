//! Agent loop (SPEC §3.3): presets + catalog + request builder +
//! permission gate + tool execution + turn orchestrator.
//!
//! Split (Task 2, no behavior change):
//! - `preset.rs` — `Agent` + CODE/ASK/PLAN/DEBUG/ORCHESTRATOR prompts
//!   (pure data; future seam for config/plugin editable prompts).
//! - `catalog.rs` — `AgentInfo`, `AgentCatalog`, hot-reload watcher.
//! - `request.rs` — `RequestBuilder` (`build_request` + `prune_for_budget`).
//! - `turn.rs` — `TurnRunner` (owns state/agent/tools/provider/permission/
//!   bus/abort/model); `run_turn` kept as a delegating shim.
//! - `gate.rs` — permission Strategy (`GateCtx`, `resolve_permission`,
//!   `ask_for_permission`).
//! - `exec.rs` — tool execution + `fail_tool`.
//! - `observe.rs` — Observer (`emit_message`/`emit_part`/`emit_session`,
//!   `title_from`) on the `EventBus`.
//!
//! Public surface is unchanged: `crate::agent::{Agent, AgentCatalog, ...,
//! run_turn, build_request, ...}` keep resolving here.

pub mod catalog;
pub mod exec;
pub mod fleet_tool;
pub mod gate;
pub mod images;
pub mod observe;
pub mod preset;
pub mod request;
pub mod task_tool;
pub mod turn;

#[cfg(test)]
mod tests;

pub use catalog::{AgentCatalog, AgentInfo, spawn_agent_watcher};
pub use exec::{ToolOutcome, fail_tool};
pub use fleet_tool::FleetTool;
pub use gate::{GateCtx, ask_for_permission, fire_permission_hook, resolve_permission};
pub use images::{AgentImageInput, ALLOWED_IMAGE_TYPES, MAX_IMAGES_PER_PROMPT, MAX_IMAGE_BASE64_LEN, MAX_IMAGE_BYTES, model_supports_images, validate_agent_images};
pub use observe::{emit_message, emit_part, emit_session, title_from};
pub use preset::Agent;
pub use request::{RequestBuilder, build_request, prune_for_budget};
pub use task_tool::TaskTool;
pub use turn::{TurnRunner, run_turn};
