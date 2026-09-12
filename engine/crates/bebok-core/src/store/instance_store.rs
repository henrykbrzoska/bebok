//! InstanceStore facade: instance cache + open/list/meta.
//! Lifecycle (fork/compact/truncate/export/…) lives in `lifecycle.rs`
//! as a Repository next to `context.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Weak;

use bebok_mcp::{McpManager, McpServerSpec};
use bebok_tools::{Runtimes, ToolRegistry, builtin_tools};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::instance::Instance;
use super::session_state::SessionState;
use crate::agent::{AgentCatalog, spawn_agent_watcher};
use crate::config;
use crate::error::{CoreError, Result};
use crate::event::EventBus;
use crate::permission::PermissionEngine;
use crate::session::Session;
use crate::session::persist::{self, data_root};
use crate::util::normalize_path;

/// Global store: instances keyed by normalized directory, sessions by id.
pub struct InstanceStore {
    pub(crate) data_dir: PathBuf,
    pub(crate) bus: EventBus,
    pub(crate) instances: RwLock<HashMap<String, Arc<Instance>>>,
    pub(crate) sessions: RwLock<HashMap<Uuid, Arc<SessionState>>>,
    /// Metadata of every known session (startup scan + created).
    pub(crate) meta: RwLock<HashMap<Uuid, Session>>,
    /// Weak self-reference, set once via `Arc::new_cyclic`. Lets the per-instance
    /// `task` tool reach the store (child sessions, bus) without an ownership
    /// cycle (store -> instance -> tools -> task tool -> store).
    pub(crate) self_weak: std::sync::OnceLock<Weak<InstanceStore>>,
}

impl InstanceStore {
    /// Create the shared store and run the startup repair pass.
    ///
    /// Returns an `Arc` (built with `Arc::new_cyclic`) so the store can give the
    /// per-instance `task` tool a `Weak<InstanceStore>` back-reference without
    /// creating an ownership cycle.
    pub fn new() -> Arc<Self> {
        Self::with_data_dir(data_root())
    }

    /// Create the shared store rooted at a custom data directory.
    pub fn with_data_dir(data_dir: PathBuf) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            let store = Self::build_with(data_dir);
            let _ = store.self_weak.set(weak.clone());
            store
        })
    }

    /// Build the store value (shared constructors wrap it in `Arc`).
    fn build_with(data_dir: PathBuf) -> Self {
        let recovered = persist::repair(&data_dir);
        let meta: HashMap<Uuid, Session> = recovered.into_iter().map(|s| (s.id, s)).collect();
        tracing::info!("recovered {} session(s) from disk", meta.len());
        Self {
            data_dir,
            bus: EventBus::default(),
            instances: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            meta: RwLock::new(meta),
            self_weak: std::sync::OnceLock::new(),
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
            tokio::fs::create_dir_all(&root)
                .await
                .map_err(CoreError::Io)?;
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

        // Register the sub-agent `task` tool. It needs the store back-reference
        // (set once via `Arc::new_cyclic`); a plain (non-shared) store skips it.
        if let Some(weak) = self.self_weak.get() {
            instance
                .tools
                .register_tool(Arc::new(crate::agent::task_tool::TaskTool::new(
                    weak.clone(),
                )));
            // Orchestrator-only parallel `fleet` tool (withheld from other
            // agents in `request.rs`; same back-reference reasoning).
            instance
                .tools
                .register_tool(Arc::new(crate::agent::fleet_tool::FleetTool::new(
                    weak.clone(),
                )));
        }

        // Async side effects: connect enabled MCP servers and register their
        // tools, then start the agent hot-reload watcher.
        let specs = McpServerSpec::parse_all(&instance.config_snapshot().mcp);
        let runtimes = Runtimes::from_config(&instance.config_snapshot().runtimes);
        let mcp_tools = instance.mcp.sync(&specs, &runtimes).await;
        instance.tools.set_mcp_tools(mcp_tools);

        let _ = spawn_agent_watcher(root.clone(), instance.agents.clone(), self.bus.clone());

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
    pub async fn continue_last_session(
        &self,
        directory: &str,
    ) -> Result<Option<Arc<SessionState>>> {
        let sessions = self.list_sessions(directory).await;
        let Some(last) = sessions.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(self.open_session(last.id).await?))
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
            let path = persist::message_path(state.disk_dir(), messages.len());
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

    /// Create an isolated child session for a delegated sub-agent.
    ///
    /// The child is registered like any other session (it shows up in the GUI
    /// and its permission prompts can be answered) and tagged `parent` so the
    /// lineage is visible. Used by the `task` tool.
    pub async fn create_subagent_session(
        &self,
        parent: &SessionState,
        agent: &str,
        model: Option<&str>,
        alias: Option<&str>,
    ) -> Result<Arc<SessionState>> {
        let instance = self.get_or_create_instance(parent.directory()).await?;
        let parent_id = parent.id();
        let parent_len = parent.messages_snapshot().await.len();
        let mut session = Session::new(parent.directory(), agent);
        session.model = model.map(str::to_string);
        session.alias = alias.map(str::to_string);
        session.parent = Some((parent_id, parent_len.saturating_sub(1)));
        self.spawn_session(&instance, session).await
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

    /// Return unresolved permission requests for all live sessions in a directory,
    /// including delegated child sessions whose asks appear in the parent UI.
    pub async fn pending_permissions(&self, directory: &str) -> Vec<serde_json::Value> {
        let normalized = normalize_path(Path::new(directory));
        let sessions: Vec<Arc<SessionState>> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|session| session.directory() == normalized)
            .cloned()
            .collect();
        let mut asks = Vec::new();
        for session in sessions {
            for mut ask in session.pending_permission_requests().await {
                ask["sessionID"] = serde_json::json!(session.id().to_string());
                ask["directory"] = serde_json::json!(session.directory());
                asks.push(ask);
            }
        }
        asks
    }
}

impl Default for InstanceStore {
    fn default() -> Self {
        Self::build_with(data_root())
    }
}

#[cfg(test)]
mod tests {
    use super::InstanceStore;
    use crate::permission::{PermissionAnswer, ResolveOutcome};

    #[tokio::test]
    async fn pending_permissions_include_child_sessions_and_disappear_after_resolution() {
        let base = std::env::temp_dir().join(format!("bebok-permissions-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let parent = store
            .create_session(project.to_str().unwrap(), "orchestrator", None)
            .await
            .unwrap();
        let child = store
            .create_subagent_session(&parent, "code", None, Some("test"))
            .await
            .unwrap();
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        child
            .register_permission_request(
                "ask-1",
                sender,
                serde_json::json!({"requestID": "ask-1", "tool": "bash", "input": {"command": "echo ok"}}),
            )
            .await;

        let asks = store.pending_permissions(project.to_str().unwrap()).await;
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0]["requestID"], "ask-1");
        assert_eq!(asks[0]["sessionID"], child.id().to_string());
        assert_eq!(asks[0]["directory"], child.directory());
        assert_eq!(asks[0]["tool"], "bash");

        assert_eq!(
            child
                .resolve_permission_request(
                    "ask-1",
                    PermissionAnswer {
                        allow: true,
                        always: false
                    }
                )
                .await,
            ResolveOutcome::Resolved
        );
        assert!(
            store
                .pending_permissions(project.to_str().unwrap())
                .await
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
