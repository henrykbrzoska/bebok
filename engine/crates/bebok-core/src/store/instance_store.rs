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
use crate::session::persist::{self, data_root};
use crate::session::{Part, Session, ToolState};
use crate::util::normalize_path;

/// Instance key for hub-mode sessions that arrive with an empty directory.
/// Sessions under this key live in `<data_dir>/global/` (pathguard confines
/// writes to that sandbox).
const GLOBAL_INSTANCE_KEY: &str = "__global__";

/// Normalize a directory string, preserving the global key literally.
fn normalize_directory(directory: &str) -> String {
    if directory == GLOBAL_INSTANCE_KEY {
        GLOBAL_INSTANCE_KEY.to_string()
    } else {
        normalize_path(Path::new(directory))
    }
}

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
        let bus = EventBus::default();
        install_browser_frame_sink(bus.clone());
        Self {
            data_dir,
            bus,
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

    /// TOR B plugin declarations: list `<root>/.bebok/plugins/*.json` for a
    /// directory (sorted by name; `installed` derived from the slot dir).
    /// Broken declaration files are skipped, never fatal.
    pub fn list_declared_plugins(&self, root: &Path) -> Vec<crate::plugin_decl::DeclaredPlugin> {
        crate::plugin_decl::list_declared(root)
    }

    /// Install a plugin slot from the central registry. The name must be
    /// listed in the registry catalogue (fetched + cached; bundled
    /// `bebok-index` fallback when offline) — anything else is a
    /// `BadRequest` (no arbitrary URLs, only public repos). Creates the
    /// declaration file (`enabled: true`) and publishes `plugin.changed`
    /// (`installed`). Idempotent: an existing declaration is kept (its
    /// `enabled` switch is preserved); a missing slot dir is installed.
    ///
    /// When the registry entry carries `asset_url`, the installer downloads
    /// the pre-built archive, verifies `asset_sha256`, and unpacks it
    /// instead of cloning the git repo.
    pub async fn install_plugin(
        &self,
        instance: &Arc<Instance>,
        name: &str,
    ) -> Result<crate::DeclaredPlugin> {
        if name.trim().is_empty()
            || name.contains(['/', '\\', '.'])
            || name.contains("..")
            || name.trim() != name
        {
            return Err(CoreError::BadRequest(format!(
                "invalid plugin name '{name}'"
            )));
        }
        let data_dir = self.data_dir().to_path_buf();
        let registry =
            crate::plugin_registry::load_registry_or_fallback(&data_dir, crate::REGISTRY_URL).await;
        let entry = registry.find(name).ok_or_else(|| {
            CoreError::BadRequest(format!(
                "unknown plugin '{name}': not listed in the plugin registry"
            ))
        })?;
        if !entry.url.starts_with("https://") {
            return Err(CoreError::BadRequest(format!(
                "refusing non-https plugin url for '{name}'"
            )));
        }
        let root = &instance.root;
        let path = crate::plugin_decl::decl_path(root, name);
        let slot = crate::plugin_decl::install_dir(root, name);
        if !slot.is_dir() {
            if entry.asset_url.is_some() {
                // Download + verify + unpack path.
                self.download_install_slot(&slot, entry, &instance.directory, name)
                    .await?;
            } else {
                // Fallback: git clone path.
                Self::clone_plugin_slot(&slot, &entry.url).await?;
            }
            let manifest = crate::plugin_registry::read_manifest(&slot)?;
            if manifest.name != name {
                let _ = std::fs::remove_dir_all(&slot);
                return Err(CoreError::BadRequest(format!(
                    "plugin manifest name '{}' does not match '{name}'",
                    manifest.name
                )));
            }
            if !manifest.engine_compatible(env!("CARGO_PKG_VERSION")) {
                let _ = std::fs::remove_dir_all(&slot);
                return Err(CoreError::BadRequest(format!(
                    "plugin '{name}' needs engine >= {} (this engine is {})",
                    manifest.min_engine_version,
                    env!("CARGO_PKG_VERSION")
                )));
            }
        }
        let decl = if path.is_file() {
            crate::plugin_decl::read_decl(&path)?
        } else {
            let decl = crate::PluginDecl {
                name: entry.name.clone(),
                repo: entry.repo.clone(),
                url: entry.url.clone(),
                enabled: true,
                asset_url: entry.asset_url.clone(),
                asset_sha256: entry.asset_sha256.clone(),
            };
            crate::plugin_decl::write_decl(root, &decl)?;
            decl
        };
        crate::git::ensure_plugin_slots_ignored(root)?;
        // `is_dir` (not `exists`): a stray file at the slot path must not
        // count as an installed plugin (consistent with `slot_state` and
        // the update route's 503 check).
        let installed = crate::plugin_decl::install_dir(root, name).is_dir();
        let decl_out = crate::DeclaredPlugin::with_state(root, decl, installed);
        self.bus.publish(crate::event::Event::plugin_changed(
            &instance.directory,
            name,
            "installed",
        ));
        Ok(decl_out)
    }

    /// Download + verify + unpack a plugin from an asset URL into the slot.
    /// On any error, both staging and slot dirs are cleaned up.
    async fn download_install_slot(
        &self,
        slot: &std::path::Path,
        entry: &crate::RegistryPlugin,
        directory: &str,
        name: &str,
    ) -> Result<()> {
        let asset_url = entry.asset_url.as_ref().expect("checked by caller");
        let asset_sha256 = entry.asset_sha256.as_ref().expect("checked by caller");
        let staging = slot.with_extension("staging");

        // Clean up on error (helper closure).
        let cleanup = |staging: &Path, slot: &Path| {
            let _ = std::fs::remove_dir_all(staging);
            let _ = std::fs::remove_dir_all(slot);
        };

        // Ensure staging dir exists and is clean.
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        std::fs::create_dir_all(&staging).map_err(CoreError::Io)?;

        // Determine archive kind from URL.
        let kind = crate::plugin_download::kind_from_url(asset_url).ok_or_else(|| {
            cleanup(&staging, slot);
            CoreError::BadRequest(format!(
                "unsupported archive format for '{name}': {asset_url}"
            ))
        })?;

        // Download.
        let archive_name = match kind {
            crate::plugin_download::ArchiveKind::Zip => format!("{name}.zip"),
            crate::plugin_download::ArchiveKind::TarGz => format!("{name}.tar.gz"),
        };
        let archive_path = staging.join(&archive_name);

        if let Err(e) = crate::plugin_download::download_archive(
            asset_url,
            &archive_path,
            Some(&self.bus),
            directory,
            name,
        )
        .await
        {
            cleanup(&staging, slot);
            return Err(e);
        }

        // Verify SHA-256.
        self.bus
            .publish(crate::event::Event::plugin_install_progress(
                directory,
                name,
                "verify",
                asset_sha256,
            ));
        if let Err(e) = crate::plugin_download::verify_sha256(&archive_path, asset_sha256) {
            cleanup(&staging, slot);
            return Err(e);
        }

        // Unpack into staging.
        self.bus
            .publish(crate::event::Event::plugin_install_progress(
                directory,
                name,
                "unpack",
                "extracting",
            ));
        let unpack_target = staging.join(name);
        std::fs::create_dir_all(&unpack_target).map_err(CoreError::Io)?;
        if let Err(e) = crate::plugin_download::unpack(&archive_path, &unpack_target, kind) {
            cleanup(&staging, slot);
            return Err(e);
        }

        // Atomic rename staging/name → slot.
        self.bus
            .publish(crate::event::Event::plugin_install_progress(
                directory,
                name,
                "install",
                "finalizing",
            ));
        if slot.exists() {
            let _ = std::fs::remove_dir_all(slot);
        }
        if let Err(e) = std::fs::rename(&unpack_target, slot) {
            cleanup(&staging, slot);
            return Err(CoreError::Io(e));
        }

        // Clean up staging dir (downloaded archive no longer needed).
        let _ = std::fs::remove_dir_all(&staging);

        Ok(())
    }

    /// Remove the existing slot and re-install from the registry entry.
    /// Used by the `POST /plugins/{name}/update` route.
    /// The route handler rejects a running plugin subprocess (409) and a
    /// slot with a missing binary (503); here we just re-install.
    pub async fn update_plugin(
        &self,
        instance: &Arc<Instance>,
        name: &str,
    ) -> Result<crate::DeclaredPlugin> {
        // Validate name.
        if name.trim().is_empty()
            || name.contains(['/', '\\', '.'])
            || name.contains("..")
            || name.trim() != name
        {
            return Err(CoreError::BadRequest(format!(
                "invalid plugin name '{name}'"
            )));
        }
        let root = &instance.root;
        let slot = crate::plugin_decl::install_dir(root, name);
        let path = crate::plugin_decl::decl_path(root, name);

        // Declaration must exist (404 if not).
        if !path.is_file() {
            return Err(CoreError::BadRequest(format!(
                "plugin '{name}' is not declared (install it first)"
            )));
        }

        // The 503 `binary_missing` check lives in the route handler
        // (shared `plugin_decl::slot_state` logic); here we just re-install.

        // Remove the old slot.
        if slot.is_dir() {
            std::fs::remove_dir_all(&slot).map_err(CoreError::Io)?;
        }

        // Re-install from registry.
        self.install_plugin(instance, name).await
    }

    /// Clone a plugin repo into its slot dir and check out the latest tag.
    /// A failed clone/checkout removes the partial dir and surfaces a
    /// `BadRequest` (user-facing: bad network or bad repo).
    async fn clone_plugin_slot(slot: &Path, url: &str) -> Result<()> {
        let parent = slot
            .parent()
            .ok_or_else(|| CoreError::BadRequest("cannot resolve plugin slot dir".to_string()))?;
        std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        if slot.exists() {
            let _ = std::fs::remove_dir_all(slot);
        }
        let slot_str = slot.to_string_lossy().to_string();
        // Shallow clone of the default branch, then pin the latest tag.
        let clone = crate::git::run(parent, &["clone", "--depth", "1", url, &slot_str])
            .await
            .filter(|o| o.success)
            .is_some();
        if !clone {
            let _ = std::fs::remove_dir_all(slot);
            return Err(CoreError::BadRequest(format!(
                "plugin clone failed (is the repo public and reachable?): {url}"
            )));
        }
        if let Some(tag) = crate::plugin_registry::latest_tag(slot).await {
            let pinned = crate::git::run(slot, &["checkout", "--quiet", &tag])
                .await
                .filter(|o| o.success)
                .is_some();
            if !pinned {
                let _ = std::fs::remove_dir_all(slot);
                return Err(CoreError::BadRequest(format!(
                    "plugin tag checkout failed: {tag}"
                )));
            }
        }
        Ok(())
    }

    /// TOR B: flip the `enabled` switch of a declared plugin
    /// (read-modify-write of `<root>/.bebok/plugins/<name>.json`) and publish
    /// `plugin.changed` (`enabled` / `disabled`). Errors with
    /// `BadRequest` when the declaration does not exist.
    pub fn set_plugin_enabled(
        &self,
        instance: &Arc<Instance>,
        name: &str,
        enabled: bool,
    ) -> Result<crate::DeclaredPlugin> {
        let out = crate::plugin_decl::set_enabled(&instance.root, name, enabled)?;
        self.bus.publish(crate::event::Event::plugin_changed(
            &instance.directory,
            name,
            if enabled { "enabled" } else { "disabled" },
        ));
        Ok(out)
    }

    /// Get or lazily create the instance for a directory.
    ///
    /// An empty or whitespace-only `directory` resolves to the *global*
    /// instance: `directory = "__global__"`, `root = data_dir/global/`.
    /// This lets hub-mode sessions (`--global`) run with full tools in a
    /// sandbox isolated from any project on disk.
    pub async fn get_or_create_instance(&self, directory: &str) -> Result<Arc<Instance>> {
        // Hub global instance: empty directory → sandboxed root.
        let (normalized, root) = if directory.trim().is_empty() {
            let root = self.data_dir.join("global");
            (GLOBAL_INSTANCE_KEY.to_string(), root)
        } else {
            let normalized = normalize_path(Path::new(directory));
            (normalized.clone(), PathBuf::from(&normalized))
        };

        if let Some(inst) = self.instances.read().await.get(&normalized) {
            return Ok(inst.clone());
        }

        // Ensure the project directory exists (tools write relative to root).
        if !root.exists() {
            tokio::fs::create_dir_all(&root)
                .await
                .map_err(CoreError::Io)?;
        }

        let config = Arc::new(std::sync::RwLock::new(config::load(&root)));
        configure_browser(&root, &config.read().unwrap());
        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let permission = Arc::new(PermissionEngine::load(&root));
        if normalized == GLOBAL_INSTANCE_KEY {
            // Hub mode: YOLO is never allowed, mutating tools always Ask.
            permission.set_global_mode();
        }
        permission.set_yolo(config.read().unwrap().yolo);
        permission.set_browser_auto(
            config
                .read()
                .unwrap()
                .frontend_verify()
                .auto_allows_browser(),
        );
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

        // Announce the new instance on the `instance.created` hook.
        // Never fails instance creation: plugin errors are logged and
        // skipped inside `run_hook`.
        {
            let mut payload = crate::plugin::InstanceCreatedHook {
                directory: normalized.clone(),
            };
            crate::plugin::PluginHost::global()
                .run_hook(crate::plugin::Hook::INSTANCE_CREATED, &mut payload)
                .await;
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
            // WP-DELEGATION: supervision tools for background children.
            instance
                .tools
                .register_tool(Arc::new(crate::agent::TaskStatusTool::new(weak.clone())));
            instance
                .tools
                .register_tool(Arc::new(crate::agent::TaskWaitTool::new(weak.clone())));
            instance
                .tools
                .register_tool(Arc::new(crate::agent::TaskCancelTool::new(weak.clone())));
            // In-process code-index tools: delegate to the `bebok-index` plugin.
            instance
                .tools
                .register_tool(Arc::new(crate::agent::CodeIndexStatus));
            instance
                .tools
                .register_tool(Arc::new(crate::agent::CodeIndexSearch));
        }

        // Async side effects: connect enabled MCP servers and register their
        // tools, then start the agent hot-reload watcher.
        let specs = McpServerSpec::parse_all(&instance.config_snapshot().mcp);
        let runtimes = Runtimes::from_config(&instance.config_snapshot().runtimes);
        let mcp_tools = instance.mcp.sync(&specs, &runtimes).await;
        instance.tools.set_mcp_tools(mcp_tools);

        let _ = spawn_agent_watcher(root.clone(), instance.agents.clone(), self.bus.clone());
        // NOTE: the watcher JoinHandle is intentionally detached (engine
        // "hot reload" semantics: one watcher per instance for the process
        // lifetime). In tests set `BEBOK_NO_WATCH=1` to skip spawning it —
        // otherwise every test instance leaks a task + inotify FD and the
        // runtime never goes idle.

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
        instance
            .permission
            .set_browser_auto(config.frontend_verify().auto_allows_browser());
        configure_browser(&instance.root, &config);

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

    /// Create a session bound to a fresh git worktree of `root` (WP-GIT /
    /// F6-15): runs `git worktree add <root>/.bebok/worktrees/<branch>
    /// [<base>]`, then creates the session with the worktree path as its
    /// directory like any other session. Returns the session and the
    /// worktree path (unnormalised; `session.directory()` is the normalised
    /// form). Session-creation internals are untouched - only the directory
    /// string differs.
    pub async fn create_worktree_session(
        &self,
        root: &str,
        branch: &str,
        base: Option<&str>,
        agent: &str,
        model: Option<&str>,
    ) -> Result<(Arc<SessionState>, PathBuf)> {
        let root_path = PathBuf::from(normalize_path(Path::new(root)));
        let path = crate::git::add_worktree(&root_path, branch, base)
            .await
            .map_err(CoreError::from)?;
        let directory = path.to_string_lossy().to_string();
        let session = self.create_session(&directory, agent, model).await?;
        Ok((session, path))
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

        let state = Arc::new(
            SessionState::new(meta, inst_dir, disk_dir, instance.config_snapshot())
                .with_live_config(instance.config.clone()),
        );

        // Load the transcript (msg-000000.json, msg-000001.json, ...).
        let mut messages = Vec::new();
        loop {
            let path = persist::message_path(state.disk_dir(), messages.len());
            match persist::load_message(&path) {
                Some(m) => messages.push(m),
                None => break,
            }
        }
        // Repair interrupted tool calls left behind by a crash or engine
        // restart. Tool parts stuck in `Running` or `Pending` are dead —
        // no active turn will resume them — so transition them to `Error`
        // to unblock future prompts (without this the model sees an
        // orphaned tool call and returns 400 Missing tool response).
        let mut repaired = false;
        for msg in &mut messages {
            if msg.role != crate::session::Role::Assistant {
                continue;
            }
            for part in &mut msg.parts {
                if let Part::Tool {
                    state: tool_state, ..
                } = part
                    && matches!(
                        tool_state,
                        ToolState::Running { .. } | ToolState::Pending { .. }
                    )
                {
                    let input = tool_state.input().clone();
                    *tool_state = ToolState::Error {
                        input,
                        error: "interrupted by engine restart".to_string(),
                    };
                    repaired = true;
                }
            }
        }

        // Persist repaired messages so the fix survives future opens.
        if repaired {
            for (i, msg) in messages.iter().enumerate() {
                if let Err(e) = persist::persist_message(state.disk_dir(), i, msg).await {
                    tracing::warn!("failed to persist repaired message {i}: {e}");
                }
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

    /// List session metadata for a directory. The startup scan supplies closed
    /// sessions; open sessions contribute their current title, usage and time.
    ///
    /// Sessions running in one of the directory's git worktrees
    /// (`<directory>/.bebok/worktrees/<branch>`, WP-GIT) are listed with the
    /// project they belong to, so they show up in its sidebar and Start list.
    pub async fn list_sessions(&self, directory: &str) -> Vec<Session> {
        let normalized = normalize_directory(directory);
        let root = Path::new(&normalized);
        let mut sessions: Vec<Session> = self
            .meta
            .read()
            .await
            .values()
            .filter(|s| {
                s.directory == normalized
                    || crate::git::is_worktree_of(Path::new(&s.directory), root)
            })
            .cloned()
            .collect();
        // The startup/creation index is not rewritten after every turn. Take
        // live snapshots without holding the map lock across an await, or the
        // sidebar would keep showing the original title and zero usage until
        // the engine restarts.
        let live = {
            let states = self.sessions.read().await;
            sessions
                .iter()
                .map(|session| states.get(&session.id).cloned())
                .collect::<Vec<_>>()
        };
        for (session, state) in sessions.iter_mut().zip(live) {
            if let Some(state) = state {
                *session = state.meta_snapshot().await;
            }
        }
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

    /// F9-6: every session spawned (directly or transitively) by `id`, in
    /// creation order, breadth-first: `task`/`fleet` children first, then
    /// their children. Walks persisted `parent` links, so finished children
    /// are included. Bounded (depth 16) against corrupt cycles.
    pub async fn descendant_sessions(&self, id: Uuid) -> Vec<crate::session::Session> {
        let Ok(root) = self.session_meta(id).await else {
            return Vec::new();
        };
        let mut all = self.list_sessions(&root.directory).await;
        all.sort_by_key(|s| s.created_at);
        let mut out: Vec<crate::session::Session> = Vec::new();
        let mut frontier = vec![id];
        let mut seen = std::collections::HashSet::new();
        seen.insert(id);
        for _ in 0..16 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for s in &all {
                if let Some((parent, _)) = s.parent
                    && frontier.contains(&parent)
                    && seen.insert(s.id)
                {
                    next.push(s.id);
                    out.push(s.clone());
                }
            }
            frontier = next;
        }
        out
    }

    /// Return unresolved permission requests for all live sessions in a directory,
    /// including delegated child sessions whose asks appear in the parent UI.
    pub async fn pending_permissions(&self, directory: &str) -> Vec<serde_json::Value> {
        let normalized = normalize_directory(directory);
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

    /// F9-5: after an "always allow" answer wrote `rule`, answer every other
    /// pending ask in the directory that the new rule now covers (a sibling
    /// sub-agent waiting on the same tool must not prompt again). Returns the
    /// number of asks resolved. `except` is the request that was answered by
    /// hand and is skipped.
    pub async fn resolve_pending_matching(
        &self,
        directory: &str,
        rule: &str,
        except: &str,
    ) -> usize {
        let normalized = normalize_directory(directory);
        let sessions: Vec<Arc<SessionState>> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|session| session.directory() == normalized)
            .cloned()
            .collect();
        let mut resolved = 0;
        for session in sessions {
            for ask in session.pending_permission_requests().await {
                let Some(request_id) = ask.get("requestID").and_then(|v| v.as_str()) else {
                    continue;
                };
                if request_id == except {
                    continue;
                }
                let tool = ask.get("toolName").and_then(|v| v.as_str()).unwrap_or("");
                let input = ask.get("input").cloned().unwrap_or(serde_json::Value::Null);
                let call = crate::permission::call_string(tool, &input);
                if !PermissionEngine::rule_matches(rule, &call) {
                    continue;
                }
                if session
                    .resolve_permission_request(
                        request_id,
                        crate::permission::PermissionAnswer {
                            allow: true,
                            always: false,
                        },
                    )
                    .await
                    == crate::permission::ResolveOutcome::Resolved
                {
                    resolved += 1;
                }
            }
        }
        resolved
    }
}

impl Default for InstanceStore {
    fn default() -> Self {
        Self::build_with(data_root())
    }
}

/// WP-BROWSER2 (F7-6): hand the instance's `browser` config section to the
/// browser driver (display mode + window position; applied at the next launch).
fn configure_browser(root: &Path, cfg: &config::ResolvedConfig) {
    bebok_tools::browser::configure(
        root,
        bebok_tools::browser::BrowserSettings::from_config(&cfg.browser),
    );
}

/// WP-BROWSER2 (F7-6): every streamed browser frame becomes a `browser.frame`
/// event on the bus (`properties` = the frame: url, title, media_type, data,
/// width, height, seq, headed). The client's viewer window renders them; the
/// chat view ignores the type.
fn install_browser_frame_sink(bus: EventBus) {
    bebok_tools::browser::set_frame_sink(Arc::new(move |frame: bebok_tools::browser::Frame| {
        let (directory, session_id) = (frame.directory.clone(), frame.session_id.clone());
        let properties = serde_json::to_value(&frame).unwrap_or(serde_json::Value::Null);
        bus.publish(
            crate::event::Event::new("browser.frame", &directory, &session_id)
                .with_properties(properties),
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::InstanceStore;
    use crate::permission::{PermissionAnswer, ResolveOutcome};

    #[tokio::test]
    async fn list_sessions_uses_current_metadata_and_survives_restart() {
        let base = std::env::temp_dir().join(format!("bebok-list-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let data = base.join("data");
        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.set_title_if_empty("Build Studio Board").await;
        session.touch().await;
        session.add_usage(17, 3, None, None, None).await;

        for store in [store, InstanceStore::with_data_dir(data)] {
            let listed = store.list_sessions(project.to_str().unwrap()).await;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].title.as_deref(), Some("Build Studio Board"));
            assert_eq!(listed[0].usage.input_tokens, 17);
            assert_eq!(listed[0].usage.output_tokens, 3);
        }

        let _ = std::fs::remove_dir_all(base);
    }

    /// WP-GIT / F6-15: a worktree session is a real `git worktree` on disk,
    /// its directory is the worktree path, and it is listed under the project
    /// root it belongs to. Deleting the session leaves the worktree alone.
    #[tokio::test]
    async fn worktree_session_creates_a_git_worktree_and_lists_under_the_root() {
        if !crate::git::git_available().await {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let base = std::env::temp_dir().join(format!("bebok-wt-session-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["symbolic-ref", "HEAD", "refs/heads/main"],
            vec!["config", "user.email", "bebok@example.com"],
            vec!["config", "user.name", "Bebok Test"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            let out = crate::git::run(&project, &args).await.expect("git spawns");
            assert!(out.success, "git {args:?}: {}", out.stderr);
        }

        let store = InstanceStore::with_data_dir(base.join("data"));
        let (session, path) = store
            .create_worktree_session(
                project.to_str().unwrap(),
                "bebok/session-abc",
                None,
                "code",
                None,
            )
            .await
            .unwrap();
        assert!(
            path.join(".git").exists(),
            "a real worktree has a .git link file"
        );
        // CI's Windows TEMP may use an 8.3 path (RUNNER~1) while the store
        // canonicalizes the root (runneradmin): compare canonical forms.
        assert_eq!(
            std::fs::canonicalize(&path).unwrap(),
            std::fs::canonicalize(
                crate::git::worktrees_dir(&project)
                    .join("bebok")
                    .join("session-abc")
            )
            .unwrap()
        );
        assert_eq!(session.directory(), crate::util::normalize_path(&path));
        let info = crate::git::worktree_info(std::path::Path::new(session.directory())).unwrap();
        assert_eq!(info.branch, "bebok/session-abc");

        // Listed under the project root (and under its own directory).
        let listed = store.list_sessions(project.to_str().unwrap()).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, session.id());
        assert_eq!(store.list_sessions(session.directory()).await.len(), 1);

        // A second, plain session in the root is listed too; the worktree
        // session is not listed under an unrelated directory.
        store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        assert_eq!(
            store.list_sessions(project.to_str().unwrap()).await.len(),
            2
        );
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(
            store
                .list_sessions(other.to_str().unwrap())
                .await
                .is_empty()
        );

        // Deleting the session never removes the worktree by itself.
        let meta = store.delete_session(session.id()).await.unwrap();
        assert_eq!(meta.directory, session.directory());
        assert!(
            path.join(".git").exists(),
            "worktree survives session deletion"
        );

        let _ = crate::git::remove_worktree(&project, &path).await;
        let _ = std::fs::remove_dir_all(base);
    }

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

    /// F9-5 integration: the parent answers "always allow this tool" for a
    /// `write_file` ask -> the project rule is `write_file(*)`, the shared
    /// permission engine auto-allows the child's *next* write to a different
    /// path, and a sibling child's ask already pending for the same tool is
    /// settled without another prompt.
    #[tokio::test]
    async fn always_allow_on_parent_auto_allows_child_requests() {
        use crate::permission::Verdict;
        let base = std::env::temp_dir().join(format!("bebok-always-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let dir = project.to_str().unwrap();
        let parent = store.create_session(dir, "code", None).await.unwrap();
        let child_a = store
            .create_subagent_session(&parent, "code", None, Some("api-orders"))
            .await
            .unwrap();
        let child_b = store
            .create_subagent_session(&parent, "code", None, Some("frontend"))
            .await
            .unwrap();
        let instance = store.get_or_create_instance(dir).await.unwrap();
        let engine = instance.permission.clone();
        // Hermetic: the instance loads the real global config, which may set
        // `yolo` (auto-allow everything) on a dev machine. Force it off so
        // the default `Ask` for mutating tools is what we exercise here.
        engine.set_yolo(false);

        // Child A asks for one path: default `Ask`, suggested rule is the TOOL.
        let first = engine.evaluate(
            None,
            "write_file",
            &serde_json::json!({ "path": "apps/api/src/orders.ts", "content": "x" }),
            false,
        );
        assert_eq!(first.verdict, Verdict::Ask);
        assert_eq!(first.pattern, "write_file(apps/api/src/orders.ts)");
        assert_eq!(first.suggested_rule, "write_file(*)");

        // Child B is already waiting on its own write_file ask.
        let (sender_b, receiver_b) = tokio::sync::oneshot::channel();
        child_b
            .register_permission_request(
                "ask-b",
                sender_b,
                serde_json::json!({
                    "requestID": "ask-b",
                    "toolName": "write_file",
                    "input": { "path": "apps/frontend/src/app.ts", "content": "y" },
                    "suggestedRule": "write_file(*)",
                }),
            )
            .await;
        // ...and so is a bash ask that the write_file rule must NOT cover.
        let (sender_c, mut receiver_c) = tokio::sync::oneshot::channel();
        child_b
            .register_permission_request(
                "ask-c",
                sender_c,
                serde_json::json!({
                    "requestID": "ask-c",
                    "toolName": "bash",
                    "input": { "command": "npm test" },
                    "suggestedRule": "bash(*)",
                }),
            )
            .await;

        // The user clicks "Always allow this tool" on child A's ask (what the
        // gate does with the answer) ...
        engine.always_allow(&first.suggested_rule).unwrap();
        // ... and the route settles the siblings the new rule covers.
        let settled = store
            .resolve_pending_matching(dir, &first.suggested_rule, "ask-a")
            .await;
        assert_eq!(settled, 1);
        let answer = receiver_b.await.unwrap();
        assert!(answer.allow && !answer.always);
        assert!(receiver_c.try_recv().is_err(), "bash ask stays pending");

        // Every later write_file in ANY session of the directory is allowed
        // (child A's next path, the parent's own edit) without a prompt.
        for (session, path) in [
            (&child_a, "apps/api/src/orders.spec.ts"),
            (&parent, "README.md"),
        ] {
            let _ = session;
            let eval = engine.evaluate(
                None,
                "write_file",
                &serde_json::json!({ "path": path, "content": "z" }),
                false,
            );
            assert_eq!(eval.verdict, Verdict::Allow, "{path}");
            assert_eq!(eval.pattern, "write_file(*)");
        }
        // The rule is persisted at project level, not per path.
        let cfg = std::fs::read_to_string(project.join(".bebok").join("config.json")).unwrap();
        assert!(cfg.contains("write_file(*)"), "{cfg}");
        assert!(!cfg.contains("orders.ts"), "{cfg}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Hub global instance: empty directory → `__global__` key, root at
    /// `data_dir/global/`, full tools available.
    #[tokio::test]
    async fn empty_directory_resolves_to_global_instance() {
        let base = std::env::temp_dir().join(format!("bebok-global-{}", uuid::Uuid::new_v4()));
        let data = base.join("data");
        let store = InstanceStore::with_data_dir(data.clone());

        let instance = store.get_or_create_instance("").await.unwrap();
        assert_eq!(
            instance.directory, "__global__",
            "empty directory must map to __global__ key"
        );
        assert_eq!(
            instance.root,
            data.join("global"),
            "root must be data_dir/global/"
        );
        assert!(
            instance.root.exists(),
            "data_dir/global/ must be created on first access"
        );

        // A session can be created and opened under the global instance.
        let session = store.create_session("", "code", None).await.unwrap();
        assert_eq!(session.directory(), "__global__");
        let listed = store.list_sessions("__global__").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, session.id());

        // A second call with empty directory returns the cached instance.
        let again = store.get_or_create_instance("").await.unwrap();
        assert!(std::sync::Arc::ptr_eq(&instance, &again));

        // Whitespace-only is also treated as empty (global).
        let ws = store.get_or_create_instance("   ").await.unwrap();
        assert!(std::sync::Arc::ptr_eq(&instance, &ws));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The global sandbox rejects path escapes: `../x` does not leave
    /// `data_dir/global/`.
    #[tokio::test]
    async fn global_instance_rejects_path_escape() {
        let base =
            std::env::temp_dir().join(format!("bebok-global-escape-{}", uuid::Uuid::new_v4()));
        let data = base.join("data");
        let store = InstanceStore::with_data_dir(data.clone());
        let instance = store.get_or_create_instance("").await.unwrap();

        // pathguard: `../x` resolves outside data_dir/global/ → rejected.
        let result = bebok_tools::resolve_in_root(&instance.root, "../x");
        assert!(result.is_err(), "../x must be rejected in global sandbox");
        assert!(
            result.unwrap_err().contains("escapes"),
            "error must mention escape"
        );

        // A file inside the sandbox is accepted (directory must exist).
        tokio::fs::create_dir_all(instance.root.join("sub"))
            .await
            .unwrap();
        assert!(
            bebok_tools::resolve_in_root(&instance.root, "sub/file.txt").is_ok(),
            "relative path inside global sandbox must be accepted"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
