//! InstanceStore (SPEC §3.10): per-directory runtime state.
//!
//! One engine process serves multiple working directories. Runtime state
//! (sessions, config, tool registry) is keyed by normalized path; sessions are
//! looked up globally by id. Nothing session-critical lives only in RAM: the
//! disk journal is authoritative, RAM is a cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_mcp::{McpManager, McpServerSpec};
use bebok_tools::{Runtimes, ToolRegistry, builtin_tools};

use crate::agent::{AgentCatalog, spawn_agent_watcher};
use crate::config::{self, ResolvedConfig};
use crate::error::{CoreError, Result};
use crate::event::EventBus;
use crate::permission::{
    CachedDecision, DecisionKey, PermissionAnswer, PermissionEngine, ResolveOutcome,
};
use crate::session::persist::{self, data_root};
use crate::session::{Message, Session};
use crate::util::normalize_path;

/// A directory-keyed instance: resolved config + tool registry + permission engine.
pub struct Instance {
    pub directory: String,
    pub root: PathBuf,
    /// Live resolved config (reloadable via `PUT /config` / toggles).
    pub config: Arc<std::sync::RwLock<ResolvedConfig>>,
    pub tools: Arc<ToolRegistry>,
    pub permission: Arc<PermissionEngine>,
    /// Agent presets for this instance (built-ins + file presets, hot reloaded).
    pub agents: Arc<std::sync::RwLock<AgentCatalog>>,
    /// MCP bridge state for this instance.
    pub mcp: Arc<McpManager>,
    /// Pending "environment changed" notes (MCP/skill/yolo toggles) injected
    /// into the next prompt's context and then cleared.
    pub context_notes: std::sync::RwLock<Vec<String>>,
}

impl Instance {
    /// Snapshot of the current resolved config.
    pub fn config_snapshot(&self) -> ResolvedConfig {
        self.config.read().unwrap().clone()
    }

    /// Resolve an agent preset by name (falls back to `code`).
    pub fn resolve_agent(&self, name: &str) -> crate::agent::Agent {
        self.agents.read().unwrap().resolve(name)
    }

    /// Record an environment change to surface in the next prompt's context.
    pub fn add_context_note(&self, note: impl Into<String>) {
        self.context_notes.write().unwrap().push(note.into());
    }

    /// Take (and clear) the pending context notes.
    pub fn take_context_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.context_notes.write().unwrap())
    }
}

/// Per-session runtime state (metadata + transcript + turn lock).
pub struct SessionState {
    id: Uuid,
    directory: String,
    #[allow(dead_code)]
    instance_dir: PathBuf,
    disk_dir: PathBuf,
    config: ResolvedConfig,
    pub(crate) meta: RwLock<Session>,
    pub(crate) messages: RwLock<Vec<Message>>,
    max_message_index: AtomicUsize,
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

/// Global store: instances keyed by normalized directory, sessions by id.
pub struct InstanceStore {
    data_dir: PathBuf,
    bus: EventBus,
    instances: RwLock<HashMap<String, Arc<Instance>>>,
    sessions: RwLock<HashMap<Uuid, Arc<SessionState>>>,
    /// Metadata of every known session (startup scan + created).
    meta: RwLock<HashMap<Uuid, Session>>,
}

impl InstanceStore {
    /// Create the store and run the startup repair pass.
    pub fn new() -> Self {
        Self::with_data_dir(data_root())
    }

    /// Create the store rooted at a custom data directory (tests / embedding).
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        let recovered = persist::repair(&data_dir);
        let meta: HashMap<Uuid, Session> =
            recovered.into_iter().map(|s| (s.id, s)).collect();
        tracing::info!("recovered {} session(s) from disk", meta.len());
        Self {
            data_dir,
            bus: EventBus::default(),
            instances: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            meta: RwLock::new(meta),
        }
    }

    pub fn bus(&self) -> EventBus {
        self.bus.clone()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get or lazily create the instance for a directory.
    pub async fn get_or_create_instance(&self, directory: &str) -> Result<Arc<Instance>> {
        let normalized = normalize_path(Path::new(directory));
        if let Some(inst) = self.instances.read().await.get(&normalized) {
            return Ok(inst.clone());
        }

        // Ensure the project directory exists (tools write relative to root).
        let root = PathBuf::from(&normalized);
        if !root.exists() {
            tokio::fs::create_dir_all(&root).await.map_err(CoreError::Io)?;
            let normalized = normalize_path(&root);
            return Box::pin(self.get_or_create_instance(&normalized)).await;
        }

        let config = Arc::new(std::sync::RwLock::new(config::load(&root)));
        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let permission = Arc::new(PermissionEngine::load(&root));
        permission.set_yolo(config.read().unwrap().yolo);
        let agents = Arc::new(std::sync::RwLock::new(AgentCatalog::load(&root)));
        let mcp = Arc::new(McpManager::new());
        let instance = Arc::new(Instance {
            directory: normalized.clone(),
            root: root.clone(),
            config,
            tools,
            permission,
            agents,
            mcp,
            context_notes: std::sync::RwLock::new(Vec::new()),
        });

        // Insert under the write lock (double-check to avoid a race).
        {
            let mut instances = self.instances.write().await;
            if let Some(inst) = instances.get(&normalized) {
                return Ok(inst.clone());
            }
            instances.insert(normalized.clone(), instance.clone());
        }

        // Async side effects: connect enabled MCP servers and register their
        // tools, then start the agent hot-reload watcher.
        let specs = McpServerSpec::parse_all(&instance.config_snapshot().mcp);
        let runtimes = Runtimes::from_config(&instance.config_snapshot().runtimes);
        let mcp_tools = instance.mcp.sync(&specs, &runtimes).await;
        instance.tools.set_mcp_tools(mcp_tools);

        let _ = spawn_agent_watcher(
            root.clone(),
            instance.agents.clone(),
            self.bus.clone(),
        );

        Ok(instance)
    }

    /// Reload the resolved config for a directory after `PUT /config` (or an
    /// external edit), recompile the permission engine, and re-sync MCP servers.
    pub async fn reload_instance(&self, directory: &str) -> Result<Arc<Instance>> {
        let instance = self.get_or_create_instance(directory).await?;

        let config = config::load(&instance.root);
        *instance.config.write().unwrap() = config.clone();
        instance.permission.reload();
        instance.permission.set_yolo(config.yolo);

        let specs = McpServerSpec::parse_all(&config.mcp);
        let runtimes = Runtimes::from_config(&config.runtimes);
        let mcp_tools = instance.mcp.sync(&specs, &runtimes).await;
        instance.tools.set_mcp_tools(mcp_tools);

        self.bus.publish(crate::event::Event::new(
            "config.changed",
            &instance.directory,
            "",
        ));

        Ok(instance)
    }

    /// Create a new session for a directory.
    pub async fn create_session(
        &self,
        directory: &str,
        agent: &str,
        model: Option<&str>,
    ) -> Result<Arc<SessionState>> {
        let instance = self.get_or_create_instance(directory).await?;

        let mut session = Session::new(instance.directory.clone(), agent);
        session.model = model.map(str::to_string);

        self.spawn_session(&instance, session).await
    }

    /// Resolve the most recent session for a directory (for `continueLast`).
    pub async fn continue_last_session(&self, directory: &str) -> Result<Option<Arc<SessionState>>> {
        let sessions = self.list_sessions(directory).await;
        let Some(last) = sessions.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(self.open_session(last.id).await?))
    }

    /// Fork a session: a new, independent session that materializes a copy of
    /// messages `0..=message_index`, recording `parent: (source, message_index)`.
    pub async fn fork_session(&self, source: Uuid, message_index: usize) -> Result<Arc<SessionState>> {
        let source_state = self.open_session(source).await?;
        let source_meta = source_state.meta_snapshot().await;
        let messages = source_state.messages_snapshot().await;
        let up_to = (message_index + 1).min(messages.len());

        let instance = self.get_or_create_instance(&source_meta.directory).await?;

        let mut session = Session::new(source_meta.directory.clone(), source_meta.agent.clone());
        session.model = source_meta.model.clone();
        session.parent = Some((source, message_index));

        let state = self.spawn_session(&instance, session).await?;
        self.copy_messages(&state, &messages[..up_to]).await;
        Ok(state)
    }

    /// Compaction as an internal fork: a new session whose transcript is a
    /// `[summary of messages 0..N]` text followed by the tail (`messages[tail_from..]`).
    /// The original session is untouched on disk ("show full history" works).
    pub async fn compact_session(
        &self,
        source: Uuid,
        summary: String,
        tail_from: usize,
    ) -> Result<Arc<SessionState>> {
        let source_state = self.open_session(source).await?;
        let source_meta = source_state.meta_snapshot().await;
        let messages = source_state.messages_snapshot().await;
        let tail_from = tail_from.min(messages.len());

        let instance = self.get_or_create_instance(&source_meta.directory).await?;

        let mut session = Session::new(source_meta.directory.clone(), source_meta.agent.clone());
        session.model = source_meta.model.clone();
        session.parent = Some((source, tail_from));

        let state = self.spawn_session(&instance, session).await?;
        // [summary] first, then the tail.
        let summary_message = Message::summary(&summary);
        self.copy_messages(&state, std::slice::from_ref(&summary_message)).await;
        self.copy_messages(&state, &messages[tail_from..]).await;
        Ok(state)
    }

    /// Rewind a session in place: drop every message with index `>= keep`, so
    /// the transcript ends at `messages[..keep]`. Orphaned message files on disk
    /// are removed and the session's `updated_at` is bumped. This is the
    /// rollback path - unlike `fork_session` it does not create a new session,
    /// it erases the tail of the *current* one. Refuses while a turn is running.
    pub async fn truncate_session(&self, id: Uuid, keep: usize) -> Result<Arc<SessionState>> {
        let state = self.open_session(id).await?;
        if state.is_running() {
            return Err(CoreError::SessionBusy);
        }
        {
            let mut messages = state.messages.write().await;
            if keep >= messages.len() {
                return Err(CoreError::Other(
                    "nothing to truncate: message index is beyond the transcript".into(),
                ));
            }
            messages.truncate(keep);
        }
        // Remove orphaned message files for the dropped tail.
        let dir = state.disk_dir().to_path_buf();
        let mut idx = keep;
        loop {
            let path = persist::message_path(&dir, idx);
            if !path.exists() {
                break;
            }
            if let Err(e) = tokio::fs::remove_file(&path).await {
                tracing::error!(
                    "failed to remove message {idx} of session {}: {e}",
                    state.id()
                );
            }
            idx += 1;
        }
        state.max_message_index.store(keep, Ordering::Relaxed);
        state.touch().await;
        self.bus.publish(crate::event::Event::new(
            "session.updated",
            state.directory(),
            &id.to_string(),
        ));
        Ok(state)
    }

    /// Full JSON export of a session (metadata + transcript).
    pub async fn export_session(&self, id: Uuid) -> Result<serde_json::Value> {
        let state = self.open_session(id).await?;
        let meta = state.meta_snapshot().await;
        let messages = state.messages_snapshot().await;
        Ok(serde_json::json!({
            "session": meta,
            "messages": messages,
        }))
    }

    /// Delete a session permanently: remove it from memory (state + metadata
    /// cache) and drop its on-disk transcript directory. Refuses while a turn
    /// is running (409), so a live agent loop cannot lose its target. The
    /// append-only `index.jsonl` keeps the historical `created`/`deleted`
    /// events. Emits `session.deleted` on the bus.
    pub async fn delete_session(&self, id: Uuid) -> Result<Session> {
        let state = self.open_session(id).await?;
        if state.is_running() {
            return Err(CoreError::SessionBusy);
        }

        // Snapshot the freshest metadata before unwinding anything.
        let snapshot = state.meta_snapshot().await;
        // Take the session out of the live map first (new lookups fail 404),
        // then the metadata cache (session lists stop returning it). If the
        // session was never opened this run, fall back to the scanned metadata.
        let _removed = self.sessions.write().await.remove(&id);
        let meta = self.meta.write().await.remove(&id).unwrap_or(snapshot);

        // Drop the on-disk session directory (session.json + msg-*.json).
        let disk_dir = state.disk_dir().to_path_buf();
        if disk_dir.exists() {
            if let Err(e) = tokio::fs::remove_dir_all(&disk_dir).await {
                tracing::error!("failed to remove session dir {}: {e}", disk_dir.display());
            }
        }

        // Append-only index entry so the deletion itself is auditable.
        let inst_dir = persist::instance_dir(self.data_dir(), &meta.directory);
        persist::append_index_event(&inst_dir, "deleted", &meta).await;

        self.bus.publish(crate::event::Event::new(
            "session.deleted",
            &meta.directory,
            &id.to_string(),
        ));
        Ok(meta)
    }

    /// Low-level session spawn: create the on-disk dir, persist metadata, append
    /// the index event, register in memory and emit `session.created`.
    async fn spawn_session(&self, instance: &Instance, session: Session) -> Result<Arc<SessionState>> {
        let inst_dir = persist::instance_dir(self.data_dir(), &instance.directory);
        let disk_dir = persist::session_dir(&inst_dir, session.id);
        tokio::fs::create_dir_all(&disk_dir)
            .await
            .map_err(CoreError::Io)?;

        persist::persist_session_meta(&disk_dir, &session).await?;
        persist::append_index_event(&inst_dir, "created", &session).await;

        let state = Arc::new(SessionState::new(
            session.clone(),
            inst_dir,
            disk_dir,
            instance.config_snapshot(),
        ));

        self.meta.write().await.insert(session.id, session.clone());
        self.sessions.write().await.insert(session.id, state.clone());
        self.bus.publish(
            crate::event::Event::new("session.created", &instance.directory, &session.id.to_string())
                .with_properties(serde_json::json!({ "session": session })),
        );
        Ok(state)
    }

    /// Copy messages into a session (in-memory + persisted atomically).
    async fn copy_messages(&self, state: &SessionState, messages: &[Message]) {
        let mut guard = state.messages.write().await;
        let start = guard.len();
        for (offset, message) in messages.iter().enumerate() {
            let idx = start + offset;
            guard.push(message.clone());
            if let Err(e) = persist::persist_message(state.disk_dir(), idx, message).await {
                tracing::error!("failed to persist message {idx} of session {}: {e}", state.id());
            }
        }
        state.note_message_index(start + messages.len());
    }

    /// Look up a session, loading its transcript from disk on first access.
    pub async fn open_session(&self, id: Uuid) -> Result<Arc<SessionState>> {
        if let Some(state) = self.sessions.read().await.get(&id) {
            return Ok(state.clone());
        }

        let meta = self
            .meta
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;

        let instance = self.get_or_create_instance(&meta.directory).await?;
        let inst_dir = persist::instance_dir(self.data_dir(), &meta.directory);
        let disk_dir = persist::session_dir(&inst_dir, meta.id);

        let state = Arc::new(SessionState::new(
            meta,
            inst_dir,
            disk_dir,
            instance.config_snapshot(),
        ));

        // Load the transcript (msg-000000.json, msg-000001.json, ...).
        let mut messages = Vec::new();
        loop {
            let path = persist::message_path(&state.disk_dir, messages.len());
            match persist::load_message(&path) {
                Some(m) => messages.push(m),
                None => break,
            }
        }
        let count = messages.len();
        *state.messages.write().await = messages;
        state.note_message_index(count);

        self.sessions.write().await.insert(id, state.clone());
        Ok(state)
    }

    /// List session metadata for a directory (restart-safe: from the scan).
    pub async fn list_sessions(&self, directory: &str) -> Vec<Session> {
        let normalized = normalize_path(Path::new(directory));
        let mut sessions: Vec<Session> = self
            .meta
            .read()
            .await
            .values()
            .filter(|s| s.directory == normalized)
            .cloned()
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        sessions
    }

    /// Fetch metadata for one session.
    pub async fn session_meta(&self, id: Uuid) -> Result<Session> {
        if let Some(state) = self.sessions.read().await.get(&id) {
            return Ok(state.meta_snapshot().await);
        }
        self.meta
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))
    }
}

impl Default for InstanceStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fork_export_compact_lifecycle() {
        let base = std::env::temp_dir().join(format!("bebok-store-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data);
        let s = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        for text in ["one", "two", "three", "four"] {
            s.append_user_message(text).await.unwrap();
        }

        // Export returns the full transcript.
        let exp = store.export_session(s.id()).await.unwrap();
        assert_eq!(exp["messages"].as_array().unwrap().len(), 4);
        assert_eq!(exp["session"]["id"], s.id().to_string());

        // Fork at index 1 -> copies messages 0..=1, records parent.
        let fork = store.fork_session(s.id(), 1).await.unwrap();
        assert_eq!(fork.meta_snapshot().await.parent, Some((s.id(), 1)));
        assert_eq!(fork.messages_snapshot().await.len(), 2);
        assert_ne!(fork.id(), s.id(), "fork must be an independent session");

        // Compaction: summary + tail, original untouched.
        let compact = store
            .compact_session(s.id(), "[summary of messages 0..1]".into(), 2)
            .await
            .unwrap();
        let compact_msgs = compact.messages_snapshot().await;
        assert_eq!(compact_msgs.len(), 3, "summary + 2 tail messages");
        assert!(compact_msgs[0].text_content().contains("[summary"));
        // Original transcript still has all 4 messages.
        assert_eq!(s.messages_snapshot().await.len(), 4);

        // continueLast resolves some session for the directory.
        let last = store
            .continue_last_session(project.to_str().unwrap())
            .await
            .unwrap();
        assert!(last.is_some());

        // Delete removes the session from memory AND disk (M6).
        let disk_dir = fork.disk_dir().to_path_buf();
        assert!(disk_dir.exists(), "session dir exists before delete");
        store.delete_session(fork.id()).await.unwrap();
        assert!(!disk_dir.exists(), "session dir removed by delete");
        assert!(store.open_session(fork.id()).await.is_err());
        assert!(
            !store
                .list_sessions(project.to_str().unwrap())
                .await
                .iter()
                .any(|s| s.id == fork.id())
        );
        // Deleting again -> 404 (already gone).
        assert!(store.delete_session(fork.id()).await.is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
