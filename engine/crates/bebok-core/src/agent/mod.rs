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

pub mod build_test_gate;
pub mod build_test_prompt;
pub mod catalog;
pub mod code_index_tools;
pub mod delegation;
pub mod delegation_policy;
pub mod exec;
pub mod fleet_tool;
pub mod gate;
pub mod images;
pub mod index_prompt;
pub mod model_policy;
pub mod observe;
pub mod preset;
pub mod prompt_env;
pub mod request;
pub mod sampling_defaults;
pub mod status_rows;
pub mod supervision_tools;
pub mod task_tool;
pub mod turn;
pub mod verify_prompt;
pub mod watchdog;

#[cfg(test)]
mod tests;

pub use build_test_gate::check_build_test_policy;
pub use build_test_prompt::build_test_section;
pub use catalog::{AgentCatalog, AgentInfo, spawn_agent_watcher};
pub use code_index_tools::{CodeIndexSearch, CodeIndexStatus};
pub use delegation::{
    LOOP_MIN_REPEAT, LOOP_WINDOW, LoopHit, TaskProgress, WANDER_MIN_DISTINCT_READS,
    WANDER_NO_ADVANCE_WINDOW, WANDER_READ_RATIO, WanderHit, summarize_progress,
};
pub use delegation_policy::{
    FLEET_AGENT, FleetContext, FleetMemberInfo, MAX_FLEET_ROSTER, delegation_policy_note,
    subagent_note,
};
pub use exec::{ToolOutcome, fail_tool};
pub use fleet_tool::FleetTool;
pub use gate::{GateCtx, ask_for_permission, fire_permission_hook, resolve_permission};
pub use images::{
    ALLOWED_IMAGE_TYPES, AgentImageInput, MAX_IMAGE_BASE64_LEN, MAX_IMAGE_BYTES,
    MAX_IMAGES_PER_PROMPT, model_supports_images, validate_agent_images,
};
pub use index_prompt::index_section;
pub use model_policy::{HEAVY, resolve_subagent_model};
pub use observe::{emit_message, emit_part, emit_session, title_from};
pub use preset::Agent;
pub use prompt_env::host_os_note;
pub use request::{RequestBuilder, build_request, prune_for_budget};
pub use status_rows::{format_duration, format_tokens};
pub use supervision_tools::{TaskCancelTool, TaskStatusTool, TaskWaitTool};
pub use task_tool::TaskTool;
pub use turn::{TurnRunner, run_turn};
pub use verify_prompt::verification_section;
