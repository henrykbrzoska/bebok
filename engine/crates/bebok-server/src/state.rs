//! Shared application state (DI root).
//!
//! `AppState` is the single dependency-injection root for all handlers and
//! services. Handlers extract it via `State<AppState>`; services take `&AppState`.

use std::collections::HashMap;
use std::sync::Arc;

use bebok_core::{DebugLog, InstanceStore, LlmTrace};
use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "android"))]
use bebok_pty::PtyManager;

/// Registration record for a remote browser extension.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteExtension {
    pub port: String,
    pub session_id: String,
    pub directory: String,
    pub last_seen: i64, // Unix timestamp (seconds)
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<InstanceStore>,
    #[cfg(not(target_os = "android"))]
    pub ptys: Arc<PtyManager>,
    pub debug: Arc<DebugLog>,
    pub llm_trace: Arc<LlmTrace>,
    pub remote_extensions: Arc<tokio::sync::Mutex<HashMap<String, RemoteExtension>>>,
}
