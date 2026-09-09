//! Per-session runtime state (metadata + transcript + turn lock).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::ResolvedConfig;
use crate::error::Result;
use crate::permission::{
    CachedDecision, DecisionKey, PermissionAnswer, ResolveOutcome,
};
use crate::session::persist;
use crate::session::{Message, Session};

/// Per-session runtime state (metadata + transcript + turn lock).
pub struct SessionState {
    id: Uuid,
    directory: String,
    #[allow(dead_code)]
    pub(crate) instance_dir: PathBuf,
    disk_dir: PathBuf,
    config: ResolvedConfig,
    pub(crate) meta: RwLock<Session>,
    pub(crate) messages: RwLock<Vec<Message>>,
    pub(crate) max_message_index: AtomicUsize,
    /// One running turn per session; a second prompt -> `SessionBusy` (409).
    pub turn: Mutex<()>,
    running: AtomicBool,
    abort: Mutex<Option<CancellationToken>>,
    /// Pending `ask` permission requests awaiting a client decision (M2).
    pending_asks: Mutex<HashMap<String, oneshot::Sender<PermissionAnswer>>>,
    /// Session-scoped decision cache: identical `(tool, pattern)` is not asked
    /// twice within one session (M2).
    decision_cache: Mutex<HashMap<DecisionKey, CachedDecision>>,
}

impl SessionState {
    pub(crate) fn new(
        meta: Session,
        #[allow(dead_code)]
        instance_dir: PathBuf,
        disk_dir: PathBuf,
        config: ResolvedConfig,
    ) -> Self {
        Self {
            id: meta.id,
            directory: meta.directory.clone(),
            instance_dir,
            disk_dir,
            config,
            meta: RwLock::new(meta),
            messages: RwLock::new(Vec::new()),
            max_message_index: AtomicUsize::new(0),
            turn: Mutex::new(()),
            running: AtomicBool::new(false),
            abort: Mutex::new(None),
            pending_asks: Mutex::new(HashMap::new()),
            decision_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn directory(&self) -> &str {
        &self.directory
    }

    pub fn disk_dir(&self) -> &Path {
        &self.disk_dir
    }

    pub fn config_snapshot(&self) -> ResolvedConfig {
        self.config.clone()
    }

    pub async fn meta_snapshot(&self) -> Session {
        self.meta.read().await.clone()
    }

    pub async fn messages_snapshot(&self) -> Vec<Message> {
        self.messages.read().await.clone()
    }

    pub(crate) fn note_message_index(&self, idx: usize) {
        self.max_message_index.fetch_max(idx + 1, Ordering::Relaxed);
    }

    /// Mutate a message in place (streaming appends).
    pub async fn append_to_part<F>(&self, idx: usize, f: F)
    where
        F: FnOnce(&mut Message),
    {
        let mut messages = self.messages.write().await;
        if let Some(m) = messages.get_mut(idx) {
            f(m);
        }
    }

    /// Mutate a tool part; `f` receives the message and the tool name.
    pub async fn update_tool_state<F>(&self, idx: usize, call_id: &str, f: F) -> bool
    where
        F: FnOnce(&mut Message, &str) -> bool,
    {
        let mut messages = self.messages.write().await;
        if let Some(m) = messages.get_mut(idx) {
            let name = m
                .parts
                .iter()
                .find_map(|p| match p {
                    crate::session::Part::Tool { id, name, .. } if id == call_id => {
                        Some(name.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            return f(m, &name);
        }
        false
    }

    /// Flush a message to disk (atomic, 1 file = 1 message).
    pub async fn persist_message_at(&self, idx: usize) {
        let message = {
            let messages = self.messages.read().await;
            messages.get(idx).cloned()
        };
        if let Some(message) = message {
            let result = persist::persist_message(&self.disk_dir, idx, &message).await;
            if let Err(e) = result {
                tracing::error!("failed to persist message {idx} of session {}: {e}", self.id);
            }
        }
        self.max_message_index.fetch_max(idx + 1, Ordering::Relaxed);
    }

    /// Append the user prompt message and persist it. Returns its index.
    pub async fn append_user_message(&self, text: &str) -> Result<usize> {
        let idx = {
            let mut messages = self.messages.write().await;
            messages.push(Message::user(text));
            messages.len() - 1
        };
        self.persist_message_at(idx).await;
        Ok(idx)
    }

    /// Set the session title from the first prompt (M1 heuristic).
    pub async fn set_title_if_empty(&self, prompt: &str) -> bool {
        let mut meta = self.meta.write().await;
        if meta.title.is_some() {
            return false;
        }
        let mut title: String = prompt.chars().take(60).collect();
        if title.is_empty() {
            return false;
        }
        if prompt.chars().count() > 60 {
            title.push('\u{2026}');
        }
        meta.title = Some(title);
        meta.touch();
        true
    }

    /// Change the session's agent mid-chat (persists to metadata).
    pub async fn set_agent(&self, agent: &str) {
        let session = {
            let mut meta = self.meta.write().await;
            if meta.agent == agent {
                return;
            }
            meta.agent = agent.to_string();
            meta.touch();
            meta.clone()
        };
        if let Err(e) = persist::persist_session_meta(&self.disk_dir, &session).await {
            tracing::error!("failed to persist session meta: {e}");
        }
    }

    /// Add token usage to session totals and persist metadata.
    pub async fn add_usage(
        &self,
        input: u64,
        output: u64,
        cost: Option<f64>,
        cache_read: Option<u64>,
        cache_write: Option<u64>,
    ) {
        let session = {
            let mut meta = self.meta.write().await;
            meta.usage.add(input, output, cost, cache_read, cache_write);
            meta.touch();
            meta.clone()
        };
        if let Err(e) = persist::persist_session_meta(&self.disk_dir, &session).await {
            tracing::error!("failed to persist session meta: {e}");
        }
    }

    /// Update `updated_at` and persist metadata.
    pub async fn touch(&self) {
        let session = {
            let mut meta = self.meta.write().await;
            meta.touch();
            meta.clone()
        };
        if let Err(e) = persist::persist_session_meta(&self.disk_dir, &session).await {
            tracing::error!("failed to persist session meta: {e}");
        }
    }

    /// Register the abort token for the running turn.
    pub async fn set_abort(&self, token: CancellationToken) {
        *self.abort.lock().await = Some(token);
    }

    /// Synchronously claim the turn slot; `false` means a turn is running.
    pub fn try_begin_turn(&self) -> bool {
        !self.running.swap(true, Ordering::AcqRel)
    }

    /// Release the turn slot when the turn ends.
    pub fn end_turn(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// Clear the abort token when the turn ends.
    pub async fn clear_abort(&self) {
        *self.abort.lock().await = None;
    }

    /// Fetch the abort token (for `POST /session/{id}/abort`).
    pub async fn abort_token(&self) -> Option<CancellationToken> {
        self.abort.lock().await.clone()
    }

    /// True while a turn is running on this session.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Register a pending permission request under its unique request id.
    pub async fn register_permission_request(
        &self,
        request_id: &str,
        sender: oneshot::Sender<PermissionAnswer>,
    ) {
        self.pending_asks
            .lock()
            .await
            .insert(request_id.to_string(), sender);
    }

    /// Drop a pending permission request (the turn moved on / aborted).
    pub async fn unregister_permission_request(&self, request_id: &str) {
        self.pending_asks.lock().await.remove(request_id);
    }

    /// Answer a pending `ask`. First resolver wins; later calls for the same
    /// id get [`ResolveOutcome::NotFound`] (no duplicated decisions).
    pub async fn resolve_permission_request(
        &self,
        request_id: &str,
        answer: PermissionAnswer,
    ) -> ResolveOutcome {
        let sender = self.pending_asks.lock().await.remove(request_id);
        match sender {
            Some(sender) => {
                // If the turn ended/aborted in the meantime the receiver is
                // gone and the answer is dropped silently.
                let _ = sender.send(answer);
                ResolveOutcome::Resolved
            }
            None => ResolveOutcome::NotFound,
        }
    }

    /// Look up the cached decision for one `(tool, pattern)`.
    pub async fn cached_decision(&self, key: &DecisionKey) -> Option<CachedDecision> {
        self.decision_cache.lock().await.get(key).copied()
    }

    /// Remember a decision for the rest of the session.
    pub async fn remember_decision(&self, key: DecisionKey, decision: CachedDecision) {
        self.decision_cache.lock().await.insert(key, decision);
    }
}
