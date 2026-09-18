//! Core engine: configuration, session/part model, persistence, agent loop,
//! event bus, plugin host and the per-directory instance store.

pub mod agent;
pub mod change_tracking;
pub mod config;
pub mod context;
pub mod debug;
pub mod error;
pub mod event;
pub mod fleet_gen;
pub mod git;
pub mod llm_trace;
pub mod permission;
pub mod plugin;
pub mod plugin_decl;
pub mod plugin_download;
pub mod plugin_process;
pub mod plugin_registry;
pub mod provider;
pub mod scheduler;
pub mod session;
pub mod stats;
pub mod store;
pub mod tool_safety;
pub mod util;

pub use agent::{Agent, AgentCatalog, AgentInfo};
pub use error::CoreError;
pub use llm_trace::{
    LLM_TRACE, LlmCall, LlmTrace, begin_llm_call, complete_llm_call, push_llm_call,
};
pub use permission::{
    Action, CachedDecision, CompiledLayer, DecisionKey, Evaluation, PermissionAnswer,
    PermissionEngine, ResolveOutcome, Rule, Verdict,
};
pub use provider::build_provider;
pub use store::{Instance, InstanceStore, SessionState};
pub use tool_safety::{SafetyCategory, SafetyOverrides, ToolSafetyEntry};

/// Plugin host + observer API (event-observer with typed lifecycle hooks).
pub use plugin::{
    BebokPlugin, Hook, HookResult, PermissionHook, PluginHost, RequestHook, RequestMessage,
    ToolCallHook, ToolResultHook, TurnHook, hook_names,
};

/// On-disk plugin declaration contract (`<project>/.bebok/plugins/*.json`).
pub use plugin_decl::{
    DeclaredPlugin, KNOWN_PLUGIN_NAME, KNOWN_PLUGIN_REPO, KNOWN_PLUGIN_URL, PluginDecl,
};

/// Subprocess-based dynamic plugin driver (JSON-lines over stdio).
pub use plugin_process::{DynamicPlugin, PluginProcess, load_dynamic_plugin};

/// Central plugin registry (remote catalogue + manifest validation).
pub use plugin_registry::{
    PLUGIN_MANIFEST_FILE, PluginManifest, PluginRegistryFile, REGISTRY_REPO, REGISTRY_TTL,
    REGISTRY_URL, RegistryPlugin, fallback_registry, latest_tag, load_registry_or_fallback,
    read_manifest,
};

/// Re-exported for the server layer (skills discovery + prompt assembly).
pub use bebok_skills as skills;

/// Re-exported for the server layer (runtime executables + Docker probe).
pub use bebok_tools::{DockerStatus, Runtimes, check_docker};

/// Re-exported for the server layer (gitignore-aware filesystem explorer).
pub use bebok_tools::explorer;

/// Debug logger (single `debug.log` file, capped, cleared on startup).
pub use debug::{DebugEntry, DebugLog};
