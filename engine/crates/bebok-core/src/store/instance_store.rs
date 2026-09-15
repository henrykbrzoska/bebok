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
use crate::index::BackendRegistry;
use crate::permission::PermissionEngine;
use crate::session::persist::{self, data_root};
use crate::session::{Part, Session, ToolState};
use crate::store::CodeIndexStatus;
use crate::util::normalize_path;

/// Global store: instances keyed by normalized directory, sessions by id.
pub struct InstanceStore {
    pub(crate) data_dir: PathBuf,
    pub(crate) bus: EventBus,
    pub(crate) instances: RwLock<HashMap<String, Arc<Instance>>>,
    pub(crate) sessions: RwLock<HashMap<Uuid, Arc<SessionState>>>,
    /// Metadata of every known session (startup scan + created).
    pub(crate) meta: RwLock<HashMap<Uuid, Session>>,
    /// Code-index backend registry (one active slot + disabled fallback).
    /// `Instance::attach_code_index` is fed by `backends.spawn(..)`, so the
    /// store never references a concrete backend implementation.
    pub(crate) backends: Arc<BackendRegistry>,
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
    /// Uses the process-wide [`BackendRegistry::global`].
    pub fn with_data_dir(data_dir: PathBuf) -> Arc<Self> {
        Self::with_backend_registry(data_dir, BackendRegistry::global())
    }

    /// Create the shared store with an explicit code-index backend registry
    /// (injection point for tests: an empty registry exercises the disabled
    /// fallback path).
    pub fn with_backend_registry(data_dir: PathBuf, backends: Arc<BackendRegistry>) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            let store = Self::build_with(data_dir, backends);
            let _ = store.self_weak.set(weak.clone());
            store
        })
    }

    /// Build the store value (shared constructors wrap it in `Arc`).
    fn build_with(data_dir: PathBuf, backends: Arc<BackendRegistry>) -> Self {
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
            backends,
            self_weak: std::sync::OnceLock::new(),
        }
    }

    pub fn bus(&self) -> EventBus {
        self.bus.clone()
    }

    /// The code-index backend registry this store spawns backends from.
    pub fn backend_registry(&self) -> Arc<BackendRegistry> {
        self.backends.clone()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Phase 0 code-index wiring: `<data_dir>/instances/<hash>/index` for a
    /// directory (same hash/data_dir mechanism as session persistence).
    pub fn code_index_dir_for(&self, directory: &str) -> PathBuf {
        super::code_index::code_index_dir(&self.data_dir, directory)
    }

    /// Idempotently create the per-instance code-index directory
    /// (`create_dir_all`); returns the directory path.
    pub fn ensure_code_index_dir(&self, directory: &str) -> Result<PathBuf> {
        super::code_index::ensure_code_index_dir(&self.code_index_dir_for(directory))
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
    /// `BadRequest` (no arbitrary URLs, only public repos). The installer
    /// clones `url` into `<root>/.bebok/plugins/<name>/`, checks out the
    /// latest tag, and validates the plugin's `bebok-plugin.json` manifest
    /// (name match + engine compatibility). Creates the declaration file
    /// (`enabled: true`) and publishes `plugin.changed` (`installed`).
    /// Idempotent: an existing declaration is kept (its `enabled` switch is
    /// preserved); a missing slot dir is re-cloned.
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
            Self::clone_plugin_slot(&slot, &entry.url).await?;
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
            };
            crate::plugin_decl::write_decl(root, &decl)?;
            decl
        };
        crate::git::ensure_plugin_slots_ignored(root)?;
        let installed = crate::plugin_decl::install_dir(root, name).exists();
        let decl_out = crate::DeclaredPlugin::from((decl, installed));
        if name == crate::plugin_decl::KNOWN_PLUGIN_NAME && decl_out.enabled {
            // Fresh install with the switch on: (re)attach the backend now so
            // indexing starts without waiting for a server restart. Installs
            // into an already-running instance (the route resolves it first).
            self.attach_code_index_backend(instance, true);
        }
        self.bus.publish(crate::event::Event::plugin_changed(
            &instance.directory,
            name,
            "installed",
        ));
        Ok(decl_out)
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
    /// (read-modify-write of `<root>/.bebok/plugins/<name>.json`),
    /// hot-swap the per-instance code-index backend and publish
    /// `plugin.changed` (`enabled` / `disabled`). Errors with
    /// `BadRequest` when the declaration does not exist.
    ///
    /// For the known `bebok-index` plugin this starts/stops indexing
    /// **without a server restart**: `enabled=true` spawns a fresh backend
    /// from the registry slot and attaches it to the instance;
    /// `enabled=false` attaches the inert [`DisabledBackend`](crate::index::DisabledBackend)
    /// instead. The dropped backend's worker is aborted on drop
    /// (`IndexOrchestrator` owns an `AbortHandle`); status transitions flow
    /// through the existing `code_index_updated` SSE event (the status
    /// callback is reinstalled on every swap, same as at instance creation).
    /// Unknown plugin names only flip the declaration flag (no backend).
    pub fn set_plugin_enabled(
        &self,
        instance: &Arc<Instance>,
        name: &str,
        enabled: bool,
    ) -> Result<crate::DeclaredPlugin> {
        let out = crate::plugin_decl::set_enabled(&instance.root, name, enabled)?;
        if name == crate::plugin_decl::KNOWN_PLUGIN_NAME {
            self.attach_code_index_backend(instance, enabled);
        }
        self.bus.publish(crate::event::Event::plugin_changed(
            &instance.directory,
            name,
            if enabled { "enabled" } else { "disabled" },
        ));
        Ok(out)
    }

    /// The plugin declaration's `enabled` switch for `bebok-index`
    /// (default on when undeclared — a fresh project has no declaration
    /// file yet but indexes by default).
    fn plugin_decl_enabled(root: &Path) -> bool {
        crate::plugin_decl::list_declared(root)
            .into_iter()
            .find(|p| p.name == crate::plugin_decl::KNOWN_PLUGIN_NAME)
            .map(|p| p.enabled)
            .unwrap_or(true)
    }

    /// Effective code-index switch: the plugin declaration ANDed with the
    /// config flag. `config code_index.enabled=false` (or `BEBOK_NO_INDEX=1`)
    /// keeps the index off even when the plugin is enabled — the declaration
    /// is the user-facing on/off switch, the config stays the policy floor.
    fn plugin_index_enabled(instance: &Instance, cfg: &config::ResolvedConfig) -> bool {
        Self::plugin_decl_enabled(&instance.root) && config::code_index_enabled(cfg)
    }

    /// Spawn (or park) the code-index backend for `instance` and attach it.
    /// `plugin_on=false` forces the disabled fallback regardless of config.
    /// Status transitions republish onto the instance snapshot and the SSE
    /// bus through a fresh callback (same shape as at instance creation).
    /// The `code_search` tool needs no re-registration: it reads the live
    /// backend from `ToolCtx.index` on every call.
    ///
    /// Design note: a disabled plugin forces the fallback **even when the
    /// registry slot holds the built-in tantivy factory** (the slot is a
    /// process-wide capability; the declaration is the per-project switch).
    /// `enabled=true` respawns from the slot (config + `BEBOK_NO_INDEX`
    /// still apply inside `spawn`).
    fn attach_code_index_backend(&self, instance: &Arc<Instance>, plugin_on: bool) {
        let cfg = instance.config_snapshot();
        let enabled = plugin_on && config::code_index_enabled(&cfg);
        let index_dir = instance.code_index_dir();
        let directory = instance.directory.clone();
        let inst_weak = Arc::downgrade(instance);
        let bus = self.bus.clone();
        let on_status: crate::index::StatusCallback =
            Arc::new(move |dto: crate::index::CodeIndexStatusDto| {
                if let Some(inst) = inst_weak.upgrade() {
                    *inst.code_index_status.write().unwrap() = CodeIndexStatus::from(dto.clone());
                }
                bus.publish(crate::event::Event::code_index_updated(
                    &directory,
                    &dto.status,
                    dto.files,
                    dto.symbols,
                ));
            });
        let request = crate::index::BackendRequest {
            root: instance.root.clone(),
            index_dir,
            excludes: cfg.code_index.exclude.clone(),
            max_files: cfg.code_index.max_files,
            enabled,
            on_status,
        };
        let backend = if plugin_on {
            self.backends.spawn(request)
        } else {
            // Hot-stop: bypass the slot so the tantivy worker is dropped
            // (aborted on drop) even though the factory stays installed
            // for other projects / the next toggle-on. `with_callback`
            // publishes the initial `disabled` snapshot onto our fresh
            // callback, and `attach_code_index` syncs the instance
            // snapshot from `backend.status()`.
            let root = request.root.clone();
            let cb = request.on_status.clone();
            std::sync::Arc::new(crate::index::DisabledBackend::with_callback(root, cb))
                as std::sync::Arc<dyn crate::index::CodeIndexBackend>
        };
        instance.attach_code_index(backend);
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
        configure_browser(&root, &config.read().unwrap());
        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let permission = Arc::new(PermissionEngine::load(&root));
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
            index_dir: super::code_index::code_index_dir(&self.data_dir, &normalized),
            code_index_status: std::sync::RwLock::new(CodeIndexStatus::default()),
            code_index: std::sync::RwLock::new(None),
        });

        // PR1-część 2: attach a code-index backend. The store names no
        // concrete backend: it asks the registry (one active slot) for one.
        // An empty slot yields the disabled fallback, so instance creation —
        // and with it server startup — never depends on an index plugin.
        // The backend reads `code_index.*` from the resolved config, starts
        // the first build in the background and republishes status
        // transitions both onto the instance snapshot and the SSE bus.
        {
            let cfg = instance.config_snapshot();
            let enabled = Self::plugin_index_enabled(&instance, &cfg);
            let index_dir = instance.code_index_dir();
            let directory = instance.directory.clone();
            let inst_weak = Arc::downgrade(&instance);
            let bus = self.bus.clone();
            let on_status: crate::index::StatusCallback =
                Arc::new(move |dto: crate::index::CodeIndexStatusDto| {
                    if let Some(inst) = inst_weak.upgrade() {
                        *inst.code_index_status.write().unwrap() =
                            CodeIndexStatus::from(dto.clone());
                    }
                    bus.publish(crate::event::Event::code_index_updated(
                        &directory,
                        &dto.status,
                        dto.files,
                        dto.symbols,
                    ));
                });
            let backend = self.backends.spawn(crate::index::BackendRequest {
                root: root.clone(),
                index_dir,
                excludes: cfg.code_index.exclude.clone(),
                max_files: cfg.code_index.max_files,
                enabled,
                on_status,
            });
            instance.attach_code_index(backend.clone());
            // Tool-facing index adapter (core -> tools, no
            // `use bebok_core` in bebok-tools by construction).
            // Installed into the `code_search` tool slot right after
            // the backend is attached; when no backend is available
            // (empty registry -> disabled fallback, or no instance),
            // the tool keeps its `None` slot and answers with a
            // graceful message.
            let adapter: Arc<dyn bebok_tools::CodeIndexQuery> =
                crate::code_index_query_adapter::CodeIndexQueryAdapter::from_backend(backend);
            instance
                .tools
                .register_tool(Arc::new(bebok_tools::code_search::CodeSearch::new(Some(
                    adapter,
                ))));
        }

        // Insert under the write lock (double-check to avoid a race).
        {
            let mut instances = self.instances.write().await;
            if let Some(inst) = instances.get(&normalized) {
                return Ok(inst.clone());
            }
            instances.insert(normalized.clone(), instance.clone());
        }

        // PR1-część 2: announce the new instance (backend attached above)
        // on the `instance.created` hook. Never fails instance creation:
        // plugin errors are logged and skipped inside `run_hook`.
        {
            let backend_on = instance
                .code_index_backend()
                .map(|b| b.enabled())
                .unwrap_or(false);
            let mut payload = crate::plugin::InstanceCreatedHook {
                directory: normalized.clone(),
                index_enabled: backend_on,
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

        // FS watcher -> code-index rescan: external edits (editor save, git
        // checkout, another process) nudge the backend through the same
        // debounced seam the tool-mutation hook in `agent/exec.rs` uses
        // (`Instance::notify_code_index_changed` -> orchestrator, 1500 ms
        // debounce). Only spawned when the backend is enabled; construction
        // failure is logged, never fatal to instance creation or a turn.
        // The notify callback runs on a plain (non-tokio) thread, so it only
        // filters + forwards over an unbounded channel — the detached task
        // below owns the `RecommendedWatcher` (keeps events flowing) and
        // calls into the backend from runtime context (`notify_changed`
        // uses `tokio::spawn` and would panic off-runtime). Instances live
        // for the process lifetime and reload mutates in place, so there is
        // no per-reload restart and nothing to shut down on session delete.
        if std::env::var("BEBOK_NO_WATCH").as_deref() != Ok("1")
            && instance.code_index_backend().is_some_and(|b| b.enabled())
        {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
            let excludes = instance.config_snapshot().code_index.exclude.clone();
            match bebok_code_index::watcher::spawn_index_watcher(
                root.clone(),
                excludes,
                move |rel| {
                    let _ = tx.send(rel);
                },
            ) {
                Ok(watcher) => {
                    let inst_weak = Arc::downgrade(&instance);
                    tokio::spawn(async move {
                        let _watcher = watcher;
                        while let Some(rel) = rx.recv().await {
                            if let Some(inst) = inst_weak.upgrade() {
                                inst.notify_code_index_changed(rel.as_deref());
                            } else {
                                break;
                            }
                        }
                    });
                }
                Err(e) => tracing::warn!("code-index FS watcher failed to start: {e}"),
            }
        }

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
        let normalized = normalize_path(Path::new(directory));
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
        let normalized = normalize_path(Path::new(directory));
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
        Self::build_with(data_root(), BackendRegistry::global())
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

    /// Code-index wiring: `Instance::code_index_dir()` points at
    /// `<data>/instances/<hash>/index`, the store helper creates it on disk
    /// and the default backend (tantivy, `code_index.enabled` defaults to
    /// `true`) is attached and indexing.
    #[tokio::test]
    async fn code_index_wiring_points_at_instances_hash_index_and_creates_it() {
        let base = std::env::temp_dir().join(format!("bebok-code-index-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let dir = project.to_str().unwrap();

        let instance = store.get_or_create_instance(dir).await.unwrap();
        let expected = base
            .join("data")
            .join("instances")
            .join(crate::util::hash_dir(&instance.directory))
            .join("index");
        assert_eq!(instance.code_index_dir(), expected);
        assert_eq!(store.code_index_dir_for(dir), expected);

        // Backend attached from the registry: enabled by default, so the
        // status is `indexing` (build running) or already `ready`.
        let backend = instance.code_index_backend().expect("backend attached");
        assert_eq!(backend.name(), crate::index::factory::TANTIVY_BACKEND_NAME);
        assert!(backend.enabled());
        let status = instance.code_index_status().status;
        assert!(
            status == crate::store::code_index::CODE_INDEX_INDEXING
                || status == crate::store::code_index::CODE_INDEX_READY,
            "unexpected status {status}"
        );

        // The enabled backend creates its index directory at spawn time;
        // the store helper returns the same path idempotently.
        assert!(expected.is_dir());
        let created = store.ensure_code_index_dir(dir).unwrap();
        assert_eq!(created, expected);
        assert!(expected.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Plugin toggle hot-swaps the code-index backend without a server
    /// restart: `enabled=false` parks the tantivy worker and attaches the
    /// disabled fallback; `enabled=true` respawns and reattaches it (build
    /// restarts in the background). Unknown plugin names only flip the
    /// declaration flag and leave the backend alone.
    #[tokio::test]
    async fn plugin_toggle_hot_swaps_code_index_backend() {
        let base =
            std::env::temp_dir().join(format!("bebok-plugin-toggle-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let dir = project.to_str().unwrap();

        let instance = store.get_or_create_instance(dir).await.unwrap();
        assert!(instance.code_index_backend().expect("attached").enabled());

        // A declaration file is required for the toggle (unknown = 400).
        crate::plugin_decl::write_decl(
            &project,
            &crate::plugin_decl::PluginDecl::bebok_index(true),
        )
        .unwrap();

        let off = store
            .set_plugin_enabled(&instance, "bebok-index", false)
            .expect("toggle off");
        assert!(!off.enabled);
        let backend = instance.code_index_backend().expect("fallback attached");
        assert_eq!(backend.name(), crate::index::DISABLED_BACKEND_NAME);
        assert!(!backend.enabled());
        assert_eq!(
            instance.code_index_status().status,
            crate::store::code_index::CODE_INDEX_DISABLED
        );
        // The tool-facing adapter follows the swap (fresh per call).
        let adapter = instance.code_index_query_adapter().expect("adapter");
        assert!(!adapter.is_available());

        let on = store
            .set_plugin_enabled(&instance, "bebok-index", true)
            .expect("toggle on");
        assert!(on.enabled);
        let backend = instance.code_index_backend().expect("respawned");
        assert_eq!(backend.name(), crate::index::factory::TANTIVY_BACKEND_NAME);
        assert!(backend.enabled());
        let status = instance.code_index_status().status;
        assert!(
            status == crate::store::code_index::CODE_INDEX_INDEXING
                || status == crate::store::code_index::CODE_INDEX_READY,
            "unexpected status {status}"
        );
        let adapter = instance.code_index_query_adapter().expect("adapter");
        assert!(adapter.is_available());

        // Unknown plugins flip only the flag: an existing declaration for
        // another name leaves the index backend untouched.
        crate::plugin_decl::write_decl(
            &project,
            &crate::plugin_decl::PluginDecl {
                name: "other".to_string(),
                repo: String::new(),
                url: String::new(),
                enabled: true,
            },
        )
        .unwrap();
        let other = store.set_plugin_enabled(&instance, "other", false).unwrap();
        assert!(!other.enabled);
        assert!(instance.code_index_backend().expect("attached").enabled());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A `bebok-index` declaration with `enabled=false` is honoured at
    /// instance creation: the backend spawns parked (disabled fallback
    /// semantics — `code_search` degrades gracefully from the start).
    #[tokio::test]
    async fn disabled_declaration_parks_backend_at_creation() {
        let base = std::env::temp_dir().join(format!("bebok-plugin-decl-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        crate::plugin_decl::write_decl(
            &project,
            &crate::plugin_decl::PluginDecl::bebok_index(false),
        )
        .unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));

        let instance = store
            .get_or_create_instance(project.to_str().unwrap())
            .await
            .unwrap();
        let backend = instance.code_index_backend().expect("fallback attached");
        assert!(!backend.enabled());
        assert_eq!(
            instance.code_index_status().status,
            crate::store::code_index::CODE_INDEX_DISABLED
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// An empty backend registry (no index plugin installed) still yields a
    /// usable instance: the disabled fallback is attached, so the server
    /// starts and `/index/status` answers `disabled`.
    #[tokio::test]
    async fn empty_backend_registry_falls_back_to_disabled() {
        let base = std::env::temp_dir().join(format!("bebok-code-index-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let registry = std::sync::Arc::new(crate::index::BackendRegistry::new());
        assert!(!registry.is_installed());
        let store = InstanceStore::with_backend_registry(base.join("data"), registry);

        let instance = store
            .get_or_create_instance(project.to_str().unwrap())
            .await
            .unwrap();
        let backend = instance.code_index_backend().expect("fallback attached");
        assert_eq!(backend.name(), crate::index::DISABLED_BACKEND_NAME);
        assert!(!backend.enabled());
        assert_eq!(
            instance.code_index_status().status,
            crate::store::code_index::CODE_INDEX_DISABLED
        );
        assert_eq!(instance.code_index_status_dto().status, "disabled");

        // The fallback never touches the filesystem; the store helper is what
        // creates the index directory on demand.
        let index_dir = store.code_index_dir_for(project.to_str().unwrap());
        assert!(!index_dir.exists());
        assert_eq!(
            store
                .ensure_code_index_dir(project.to_str().unwrap())
                .unwrap(),
            index_dir
        );
        assert!(index_dir.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }

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
}
