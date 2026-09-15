//! Directory-keyed instance: resolved config + tool registry + permission engine.

use std::path::PathBuf;
use std::sync::Arc;

use bebok_mcp::McpManager;
use bebok_tools::ToolRegistry;

use crate::agent::{Agent, AgentCatalog, AgentInfo};
use crate::config::ResolvedConfig;
use crate::index::{CodeIndexBackend, CodeIndexStatusDto};
use crate::permission::PermissionEngine;
use crate::store::code_index::CodeIndexStatus;

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
    /// Phase 0 code-index wiring: `<data_dir>/instances/<hash>/index`.
    /// Computed from the store's data dir at creation; created lazily via
    /// [`code_index::ensure_code_index_dir`](super::code_index::ensure_code_index_dir).
    pub index_dir: PathBuf,
    /// Last known code-index status (Phase 0 default: `disabled`).
    pub code_index_status: std::sync::RwLock<CodeIndexStatus>,
    /// PR1-część 2: per-instance code-index backend behind the stable
    /// [`CodeIndexBackend`] contract. `None` until the store attaches it
    /// right after construction — the store asks the backend registry
    /// ([`BackendRegistry::spawn`](crate::index::BackendRegistry::spawn)) for
    /// it, which returns the active backend or a disabled fallback.
    pub code_index: std::sync::RwLock<Option<std::sync::Arc<dyn CodeIndexBackend>>>,
}

impl Instance {
    /// Snapshot of the current resolved config.
    pub fn config_snapshot(&self) -> ResolvedConfig {
        self.config.read().unwrap().clone()
    }

    /// Resolve an agent preset by name (falls back to `code`).
    pub fn resolve_agent(&self, name: &str) -> Agent {
        self.agents.read().unwrap().resolve(name)
    }

    /// Agent summaries for the GUI with the **effective** per-agent model
    /// resolved from config: `preset.model` -> `config.models.<name>` -> the
    /// global `config.model` (see `ResolvedConfig::model_for`). A preset that
    /// pins its own model wins; otherwise the GUI shows what would actually run.
    pub fn agent_infos(&self) -> Vec<AgentInfo> {
        let cfg = self.config_snapshot();
        let mut agents = self.agents.read().unwrap().list();
        for a in &mut agents {
            if a.model.is_none() {
                a.model = Some(cfg.model_for(&a.name));
            }
        }
        agents
    }

    /// Record an environment change to surface in the next prompt's context.
    pub fn add_context_note(&self, note: impl Into<String>) {
        self.context_notes.write().unwrap().push(note.into());
    }

    /// Take (and clear) the pending context notes.
    pub fn take_context_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.context_notes.write().unwrap())
    }

    /// Phase 0 code-index directory: `<data_dir>/instances/<hash>/index`
    /// (computed from the store's data dir at creation time).
    pub fn code_index_dir(&self) -> PathBuf {
        self.index_dir.clone()
    }

    /// Snapshot of the last known code-index status.
    /// Prefers the live backend value when attached.
    pub fn code_index_status(&self) -> CodeIndexStatus {
        if let Some(backend) = self.code_index.read().unwrap().as_ref() {
            return CodeIndexStatus::from(backend.status());
        }
        self.code_index_status.read().unwrap().clone()
    }

    /// Snapshot of the last known code-index status as the wire DTO.
    /// Prefers the live backend value when attached.
    pub fn code_index_status_dto(&self) -> CodeIndexStatusDto {
        if let Some(backend) = self.code_index.read().unwrap().as_ref() {
            return backend.status();
        }
        CodeIndexStatusDto::from(self.code_index_status.read().unwrap().clone())
    }

    /// PR1-część 2: full-text query handle through the backend contract.
    /// `None` when the backend is not attached (tests without a store).
    pub fn code_index_backend(&self) -> Option<std::sync::Arc<dyn CodeIndexBackend>> {
        self.code_index.read().unwrap().clone()
    }

    /// Tool-facing adapter over the attached backend, built lazily
    /// from [`Self::code_index_backend`]. `None` when the backend is
    /// not attached (the tool answers with a graceful message).
    pub fn code_index_query_adapter(&self) -> Option<Arc<dyn bebok_tools::CodeIndexQuery>> {
        self.code_index.read().unwrap().as_ref().map(|b| {
            crate::code_index_query_adapter::CodeIndexQueryAdapter::from_backend(b.clone())
        })
    }

    /// Attach the code-index backend (store only, right after
    /// [`crate::index::BackendRegistry::spawn`]).
    pub fn attach_code_index(&self, backend: std::sync::Arc<dyn CodeIndexBackend>) {
        *self.code_index_status.write().unwrap() = CodeIndexStatus::from(backend.status());
        *self.code_index.write().unwrap() = Some(backend);
    }

    /// Notify the backend that a file changed (debounced rescan seam).
    /// No-op when the backend is not attached.
    pub fn notify_code_index_changed(&self, rel_path: Option<&str>) {
        if let Some(backend) = self.code_index.read().unwrap().as_ref() {
            backend.notify_changed(rel_path);
        }
    }
}
