//! Shared application state (DI root).
//!
//! `AppState` is the single dependency-injection root for all handlers and
//! services. Handlers extract it via `State<AppState>`; services take `&AppState`.

use std::sync::Arc;

use bebok_core::{DebugLog, InstanceStore, LlmTrace};
#[cfg(not(target_os = "android"))]
use bebok_pty::PtyManager;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<InstanceStore>,
    #[cfg(not(target_os = "android"))]
    pub ptys: Arc<PtyManager>,
    pub debug: Arc<DebugLog>,
    pub llm_trace: Arc<LlmTrace>,
}
