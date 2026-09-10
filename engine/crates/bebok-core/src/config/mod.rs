//! Layered JSONC configuration.
//!
//! Precedence: defaults -> global (`~/.config/bebok/config.json`) ->
//! project (`<project>/.bebok/config.json`) -> request args.
//!
//! `model` + provider selection are consumed here; `permission` rules are
//! parsed by `crate::permission` directly from the same files (it needs the
//! per-layer rules, so it re-reads them instead of using the merged view).
//! `mcp.*`, `skills.*` and `terminal.*` are kept as raw sections for later
//! milestones. `ui.*` carries client-only custom CSS (plain text, never
//! executed in the engine).

pub mod diagnostics;
pub mod jsonc;
pub mod loader;
pub mod model;
pub mod providers;
pub mod writer;

// --- model (ResolvedConfig + Default + model_for/provider_spec, Builder) ---
pub use model::{
    DEFAULT_CONTEXT_BUDGET, DEFAULT_MAX_TOKENS, DEFAULT_MODEL, DEFAULT_TOOL_OUTPUT_CAP,
    FleetConfig, FleetMember, MAX_CUSTOM_CSS_FILES, MAX_CUSTOM_CSS_LEN, ResolvedConfig,
    ResolvedConfigBuilder, UiConfig,
};

// --- loader (load/load_with_global/apply/apply_file/merge_providers/...) ---
pub use loader::{
    apply, apply_file, global_config_path, load, load_with_global, merge_providers, parse_thinking,
    project_config_path, provider_from_model, read_layer_json,
};

// --- writer (deltas + full, JSONC round-trip only) ---
pub use writer::{
    write_delta_to, write_full_global, write_full_project, write_full_to, write_global_delta,
    write_project_delta,
};

// --- providers (save_provider_models*) ---
pub use providers::{save_provider_models, save_provider_models_global};

// --- diagnostics (record_unknown_tool/record_llm_error/ensure_global_runtimes) ---
pub use diagnostics::{ensure_global_runtimes, record_llm_error, record_unknown_tool};
