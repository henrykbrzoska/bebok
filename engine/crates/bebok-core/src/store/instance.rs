//! Directory-keyed instance: resolved config + tool registry + permission engine.

use std::path::PathBuf;
use std::sync::Arc;

use bebok_mcp::McpManager;
use bebok_tools::ToolRegistry;

use crate::agent::{Agent, AgentCatalog};
use crate::config::ResolvedConfig;
use crate::permission::PermissionEngine;

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
    pub fn resolve_agent(&self, name: &str) -> Agent {
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
