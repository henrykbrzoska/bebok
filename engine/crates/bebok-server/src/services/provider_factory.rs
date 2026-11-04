//! Provider factory (Factory): `build_provider_for(config, model, ctx)`.
//!
//! The implementation lives in `bebok_core::provider` so core-side features
//! (the sub-agent `task` tool) can build providers too. This module keeps the
//! server-side import path (`super::provider_factory::…`) stable.
//!
//! A real turn always goes through the `ctx` form: `kind: "cli"` providers run
//! a child process inside the project directory and cannot be built without it.

pub use bebok_core::provider::{ProviderCtx, build_provider_for};
