//! Drive agentic CLIs (Claude Code and future ones) as Bebok providers.
//!
//! Layering, lowest to highest: `bebok-llm` (HTTP protocols) ← this crate
//! (process transport + [`bebok_llm::Provider`] impl) ← `bebok-core` (dispatch
//! by [`bebok_llm::ProviderKind::Cli`]) ← `bebok-server` (`/cli/backends`).
//! This crate therefore stays free of core concepts: it sees a
//! [`bebok_llm::ChatRequest`], a project root and a config blob.
//!
//! Read [`backend`] first: the hard rule that CLI tool activity is never a
//! `StreamEvent::ToolCall` drives everything else.

pub mod backend;
pub mod backends;
pub mod discovery;
pub mod probe;
pub mod profile;
pub mod provider;
pub mod runner;
pub mod spec;
pub mod transcript;

pub use backend::{
    CliBackend, CliCapabilities, CliError, CliEvent, CliMode, CliProbe, CliRequest, CliStreamState,
    PreparedTurn, ProfileDir,
};
pub use backends::{all_backends, backend_for};
pub use discovery::{discover_binary, which_on_path};
pub use probe::{CliFeatures, probe_features};
pub use profile::{ProfilePreview, preview_profile};
pub use provider::{
    CliProvider, CliResume, CliSessionReport, CliSessionSink, build_provider,
    build_provider_for_turn, build_provider_with_env,
};
pub use runner::{CliRun, ResumePreflight, preflight, spawn_cli};
pub use spec::{CliSpec, ProfileMode, SystemPromptMode};

use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped whenever the engine's configuration changes (`reload_instance`), so
/// the managed-profile writer knows its cached `profile.lock.json` is stale and
/// regenerates on the next turn.
///
/// A process-global counter instead of a callback: `bebok-core` may invalidate
/// without depending on the profile generator, and the generator can compare
/// the epoch it wrote with the current one.
static PROFILE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Invalidate every managed CLI profile: the next turn regenerates it.
pub fn invalidate_profile_cache() {
    PROFILE_EPOCH.fetch_add(1, Ordering::Release);
}

/// Current profile epoch (see [`invalidate_profile_cache`]).
pub fn profile_epoch() -> u64 {
    PROFILE_EPOCH.load(Ordering::Acquire)
}
