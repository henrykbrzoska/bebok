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
pub mod provider;
pub mod runner;
pub mod spec;
pub mod transcript;

pub use backend::{
    CliBackend, CliCapabilities, CliError, CliEvent, CliMode, CliRequest, CliStreamState,
};
pub use backends::{all_backends, backend_for};
pub use discovery::{discover_binary, which_on_path};
pub use provider::{CliProvider, build_provider};
pub use runner::{CliRun, spawn_cli};
pub use spec::CliSpec;
