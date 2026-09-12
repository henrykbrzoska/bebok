//! Per-session runtime state (metadata + transcript + turn lock).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::Value;
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::ResolvedConfig;
use crate::error::Result;
use crate::permission::{CachedDecision, DecisionKey, PermissionAnswer, ResolveOutcome};
use crate::session::persist;
use crate::session::{Message, Session};

/// Active child task info (emitted with `task.started`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChildTask {
    #[serde(rename = "taskID")]
    pub task_id: String,
    pub description: String,
    #[serde(rename = "childSessionID")]
    pub child_session_id: String,
    pub name: String,
    pub agent: String,
    /// Effective model the child runs on (F6-12; `None` for legacy callers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Unix ms when the child turn was registered (F6-12).
    #[serde(rename = "startedAt")]
    pub started_at: i64,
}

struct PendingPermissionRequest {
    sender: oneshot::Sender<PermissionAnswer>,
    properties: Value,
}

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
    pending_asks: Mutex<HashMap<String, PendingPermissionRequest>>,
    /// Session-scoped decision cache: identical `(tool, pattern)` is not asked
    /// twice within one session (M2).
    decision_cache: Mutex<HashMap<DecisionKey, CachedDecision>>,
    /// Active child tasks spawned by the orchestrator via the `task` tool.
    /// Keyed by task ID; each holds the child's cancellation token + metadata.
    child_tasks: Mutex<HashMap<String, (CancellationToken, ChildTask)>>,
    /// Set of allocated child names within this session (for uniqueness).
    child_names: Mutex<HashSet<String>>,
    /// Monotonic counter for fallback child names (`<role>-<n>`).
    child_name_counter: Mutex<u64>,
}

impl SessionState {
    pub(crate) fn new(
        meta: Session,
        #[allow(dead_code)] instance_dir: PathBuf,
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
            child_tasks: Mutex::new(HashMap::new()),
            child_names: Mutex::new(HashSet::new()),
            child_name_counter: Mutex::new(1),
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
                tracing::error!(
                    "failed to persist message {idx} of session {}: {e}",
                    self.id
                );
            }
        }
        self.max_message_index.fetch_max(idx + 1, Ordering::Relaxed);
    }

    /// Append the user prompt message and persist it. Returns its index.
    pub async fn append_user_message(&self, text: &str) -> Result<usize> {
        self.append_user_message_with_images(text, Vec::new()).await
    }

    /// Append a user message carrying text plus image parts, persist it.
    pub async fn append_user_message_with_images(
        &self,
        text: &str,
        images: Vec<crate::session::Part>,
    ) -> Result<usize> {
        let idx = {
            let mut messages = self.messages.write().await;
            messages.push(Message::user_with_images(text, images));
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

    /// Record the context size of the latest LLM call (tokens the provider
    /// read as input, cache hits included) together with the model that
    /// produced it, and persist metadata. Overwrites: this is a live gauge,
    /// not a running total (see `Session::context_used`).
    pub async fn set_context_used(&self, tokens: u64, model: &str) {
        let session = {
            let mut meta = self.meta.write().await;
            meta.context_used = Some(tokens);
            meta.context_model = Some(model.to_string());
            meta.clone()
        };
        if let Err(e) = persist::persist_session_meta(&self.disk_dir, &session).await {
            tracing::error!("failed to persist session meta: {e}");
        }
    }

    /// Record the outcome of this session's delegated sub-turn (F6-12).
    ///
    /// Called by the `task`/`fleet` tools on the *child* session when its
    /// turn ends, with the same `status` string that goes out in the
    /// `task.ended` SSE event (`completed` | `aborted` | `error`). Persisted
    /// to the session metadata so `GET /session/{parent}/agents` can report
    /// an honest `done` / `failed` / `aborted` after the fact instead of
    /// guessing from the transcript.
    pub async fn set_task_status(&self, status: &str, error: Option<&str>) {
        let session = {
            let mut meta = self.meta.write().await;
            meta.task_status = Some(status.to_string());
            meta.task_error = error.map(str::to_string);
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

    // -- child task tracking --------------------------------------------------

    /// Allocate a unique child name within this session.
    ///
    /// Normalizes the preferred name (lowercase, replace non-alnum with `-`,
    /// collapse/trim, truncate to 32 chars, must match `^[a-z0-9][a-z0-9-]{0,30}$`).
    /// If empty/invalid after normalization → fallback `<role>-<n>`.
    /// If valid but taken → append `-2`, `-3`, etc.
    pub async fn allocate_child_name(&self, preferred: Option<&str>, role: &str) -> String {
        use std::sync::OnceLock;
        static CLEAN: OnceLock<regex::Regex> = OnceLock::new();
        static VALID: OnceLock<regex::Regex> = OnceLock::new();
        let clean = CLEAN.get_or_init(|| regex::Regex::new(r"[^a-z0-9]+").unwrap());
        let valid = VALID.get_or_init(|| regex::Regex::new(r"^[a-z0-9][a-z0-9-]{0,30}$").unwrap());

        let mut names = self.child_names.lock().await;
        let mut counter = self.child_name_counter.lock().await;

        // Try the preferred name first.
        if let Some(pref) = preferred {
            let normalized = {
                let lower = pref.to_lowercase();
                let cleaned = clean.replace_all(&lower, "-").to_string();
                let trimmed = cleaned.trim_matches('-').to_string();
                if trimmed.len() > 32 {
                    trimmed[..32].trim_matches('-').to_string()
                } else {
                    trimmed
                }
            };
            if valid.is_match(&normalized) {
                if !names.contains(&normalized) {
                    names.insert(normalized.clone());
                    return normalized;
                }
                // Taken: try -2, -3, ...
                let mut n = 2u64;
                loop {
                    let candidate = format!("{normalized}-{n}");
                    if !names.contains(&candidate) {
                        names.insert(candidate.clone());
                        return candidate;
                    }
                    n += 1;
                }
            }
        }

        // Fallback: <role>-<n>, skipping taken names.
        let role_lower = role.to_lowercase();
        loop {
            let candidate = format!("{role_lower}-{counter}");
            *counter += 1;
            if !names.contains(&candidate) {
                names.insert(candidate.clone());
                return candidate;
            }
        }
    }

    /// Register a child task's cancellation token + metadata.
    pub async fn register_child_task(
        &self,
        task_id: &str,
        description: &str,
        child_session_id: &str,
        name: &str,
        agent: &str,
        token: CancellationToken,
    ) -> ChildTask {
        self.register_child_task_with_model(
            task_id,
            description,
            child_session_id,
            name,
            agent,
            None,
            token,
        )
        .await
    }

    /// Same as [`register_child_task`](Self::register_child_task) but records
    /// the child's effective model so `GET /session/{id}/agents` can show it
    /// while the child is still running (F6-12).
    #[allow(clippy::too_many_arguments)]
    pub async fn register_child_task_with_model(
        &self,
        task_id: &str,
        description: &str,
        child_session_id: &str,
        name: &str,
        agent: &str,
        model: Option<&str>,
        token: CancellationToken,
    ) -> ChildTask {
        let info = ChildTask {
            task_id: task_id.to_string(),
            description: description.to_string(),
            child_session_id: child_session_id.to_string(),
            name: name.to_string(),
            agent: agent.to_string(),
            model: model.map(str::to_string),
            started_at: crate::util::now_ms(),
        };
        self.child_tasks
            .lock()
            .await
            .insert(task_id.to_string(), (token, info.clone()));
        info
    }

    /// Unregister a child task (when it finishes or is aborted).
    pub async fn unregister_child_task(&self, task_id: &str) {
        self.child_tasks.lock().await.remove(task_id);
    }

    /// Cancel a specific child task. Returns `true` if it existed and was cancelled.
    pub async fn abort_child_task(&self, task_id: &str) -> bool {
        if let Some((token, _info)) = self.child_tasks.lock().await.get(task_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Snapshot of active child tasks (for listing / rendering).
    pub async fn child_tasks_snapshot(&self) -> Vec<ChildTask> {
        self.child_tasks
            .lock()
            .await
            .values()
            .map(|(_, info)| info.clone())
            .collect()
    }

    /// Abort all child tasks and cancel the parent turn.
    /// Called when any child is aborted — propagates up to the orchestrator.
    pub async fn abort_children_and_parent(&self, reason: &str) {
        // Cancel all child tokens.
        {
            let mut tasks = self.child_tasks.lock().await;
            for (token, _) in tasks.values() {
                token.cancel();
            }
            tasks.clear();
        }
        // Cancel the parent turn.
        if let Some(token) = self.abort.lock().await.clone() {
            token.cancel();
        }
        tracing::info!("session {}: abort_children_and_parent: {reason}", self.id);
    }

    // -- permission -----------------------------------------------------------

    /// Register a pending permission request under its unique request id.
    pub async fn register_permission_request(
        &self,
        request_id: &str,
        sender: oneshot::Sender<PermissionAnswer>,
        properties: Value,
    ) {
        self.pending_asks.lock().await.insert(
            request_id.to_string(),
            PendingPermissionRequest { sender, properties },
        );
    }

    /// Snapshot unresolved asks for clients reconnecting after lost SSE events.
    pub async fn pending_permission_requests(&self) -> Vec<Value> {
        self.pending_asks
            .lock()
            .await
            .values()
            .map(|request| request.properties.clone())
            .collect()
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
            Some(request) => {
                // If the turn ended/aborted in the meantime the receiver is
                // gone and the answer is dropped silently.
                let _ = request.sender.send(answer);
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
