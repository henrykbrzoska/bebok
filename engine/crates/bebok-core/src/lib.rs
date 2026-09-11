//! Core engine: configuration, session/part model, persistence, agent loop,
//! event bus, plugin host and the per-directory instance store.

pub mod agent;
pub mod config;
pub mod context;
pub mod debug;
pub mod error;
pub mod event;
pub mod llm_trace;
pub mod permission;
pub mod plugin;
pub mod provider;
pub mod session;
pub mod store;
pub mod util;

pub use agent::{Agent, AgentCatalog, AgentInfo};
pub use error::CoreError;
pub use llm_trace::{LLM_TRACE, LlmCall, LlmTrace, begin_llm_call, complete_llm_call, push_llm_call};
pub use permission::{
    Action, CachedDecision, CompiledLayer, DecisionKey, Evaluation, PermissionAnswer,
    PermissionEngine, ResolveOutcome, Rule, Verdict,
};
pub use provider::build_provider;
pub use store::{Instance, InstanceStore, SessionState};

/// Plugin host + observer API (event-observer with typed lifecycle hooks).
pub use plugin::{
    BebokPlugin, Hook, HookResult, PermissionHook, PluginHost, RequestHook, RequestMessage,
    ToolCallHook, ToolResultHook, TurnHook, hook_names,
};

/// Re-exported for the server layer (skills discovery + prompt assembly).
pub use bebok_skills as skills;

/// Re-exported for the server layer (runtime executables + Docker probe).
pub use bebok_tools::{DockerStatus, Runtimes, check_docker};

/// Re-exported for the server layer (gitignore-aware filesystem explorer).
pub use bebok_tools::explorer;

/// Debug logger (single `debug.log` file, capped, cleared on startup).
pub use debug::{DebugEntry, DebugLog};
