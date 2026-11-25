//! Layered configuration, stored in the config database (`bebok.db`).
//!
//! Precedence: defaults -> `global` scope -> `project:<dir>` scope -> request
//! args. The JSON/JSONC config **files** are legacy: only [`import`] reads
//! them (one-shot `POST /config/import-to-db`). There is no file fallback at
//! runtime — an empty database resolves to the defaults.
//!
//! `model` + provider selection are consumed here; `permission` rules are
//! parsed by `crate::permission` straight from the per-scope rows (it needs the
//! per-layer rules, so it re-reads them instead of using the merged view).
//! `mcp.*`, `skills.*` and `terminal.*` are kept as raw sections for later
//! milestones. `ui.*` carries client-only custom CSS (plain text, never
//! executed in the engine).

pub mod diagnostics;
pub mod export;
pub mod import;
pub mod jsonc;
pub mod loader;
pub mod model;
pub mod projects;
pub mod providers;
pub mod secrets;
pub mod store;
pub mod store_ddl;
pub mod verify;
pub mod writer;

// --- model (ResolvedConfig + Default + model_for/provider_spec, Builder) ---
pub use model::{
    BuildTestMode, CodeGraphConfig, CodeMapConfig, DEFAULT_CODE_GRAPH_MAX_FILES,
    DEFAULT_CODE_MAP_MAX_DEPTH, DEFAULT_CODE_MAP_MAX_TOKENS, DEFAULT_CONTEXT_BUDGET,
    DEFAULT_DELEGATION_MAX_CONCURRENT, DEFAULT_MAX_TOKENS, DEFAULT_MODEL, DEFAULT_TOOL_OUTPUT_CAP,
    DelegationConfig, FleetConfig, FleetMember, MAX_CUSTOM_CSS_FILES, MAX_CUSTOM_CSS_LEN,
    MAX_DELEGATION_MAX_CONCURRENT, ResolvedConfig, ResolvedConfigBuilder, SshConfig, UiConfig,
};

// --- loader (load/apply/merge_providers/parse_thinking/provider_from_model) ---
pub use loader::{
    apply, apply_code_graph, apply_code_map, apply_delegation, apply_ssh, load, merge_providers,
    parse_thinking, provider_from_model,
};

// --- store (the config DB is the only config source) ---
// `global_config_path` / `project_config_path` exist for the one-shot import
// only; the runtime never reads a config file.
pub use store::{GLOBAL_SCOPE, global_config_path, project_config_path, project_scope};

// --- writer (deltas + full scope replace, database only) ---
pub use writer::{
    write_full_global, write_full_global_at, write_full_project, write_full_project_at,
    write_global_delta, write_global_delta_at, write_project_delta, write_project_delta_at,
};

// --- verify (`verify.frontend` policy, WP-AUTOVERIFY / F8-1) ---
pub use verify::FrontendVerify;

// --- projects (registry stored under the global config's "projects" key) ---
pub use projects::{ProjectEntry, ProjectPatch, ProjectsError};

// --- providers (save_provider_models*) ---
pub use providers::{
    save_provider_models, save_provider_models_at, save_provider_models_global,
    save_provider_models_global_at,
};

// --- diagnostics (record_unknown_tool/record_llm_error/ensure_global_runtimes) ---
pub use diagnostics::{
    ensure_global_runtimes, record_llm_error, record_llm_error_at, record_unknown_tool,
    record_unknown_tool_at,
};
