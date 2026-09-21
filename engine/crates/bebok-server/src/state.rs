//! Shared application state (DI root).
//!
//! `AppState` is the single dependency-injection root for all handlers and
//! services. Handlers extract it via `State<AppState>`; services take `&AppState`.

use std::collections::HashMap;
use std::sync::Arc;

use bebok_core::{DebugLog, InstanceStore, LlmTrace};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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

// ── Command queue (phase 2: pull-based remote piloting) ──────────────────

/// A command waiting to be picked up by an extension.
#[derive(Clone, Debug, Serialize)]
pub struct QueuedCommand {
    pub id: String,
    pub method: String,
    pub params: serde_json::Value,
    pub session_id: String,
}

/// Channel half for the engine side that waits for a command result.
pub struct WaiterHandle {
    pub tx: oneshot::Sender<serde_json::Value>,
}

/// Per-session command queue and waiter registry.
pub struct CommandRegistry {
    /// Commands waiting to be polled by the extension (keyed by session_id).
    pub queue: HashMap<String, Vec<QueuedCommand>>,
    /// Engine-side waiters — one per in-flight `POST /browser/remote/{action}`.
    /// Keyed by command_id (uuid generated in remote()), allowing multiple
    /// in-flight commands per session.
    pub waiters: HashMap<String, WaiterHandle>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            queue: HashMap::new(),
            waiters: HashMap::new(),
        }
    }
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
    pub command_queue: Arc<tokio::sync::Mutex<CommandRegistry>>,
}
