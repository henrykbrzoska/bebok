//! Provider factory (Factory): `build_provider(config, model)`.
//!
//! The implementation moved to `bebok_core::provider` so core-side features
//! (the sub-agent `task` tool) can build providers too. This module keeps the
//! old server-side path resolving (`super::provider_factory::build_provider`).

pub use bebok_core::provider::build_provider;
