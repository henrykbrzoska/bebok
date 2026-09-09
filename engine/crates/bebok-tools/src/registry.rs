use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use crate::tool::Tool;

/// A per-instance registry of available tools, keyed by name.
///
/// Built-in tools are fixed; MCP tools are contributed by the MCP bridge and
/// can change at runtime (servers toggled on/off); plugin tools are registered
/// dynamically by plugins (this is the tool-level extension point of the
/// plugin system). The mutable slices live behind a lock. The agent loop and
/// permission engine look tools up by name only and never know which slice a
/// tool came from (SPEC §3.7).
pub struct ToolRegistry {
    builtin: Vec<Arc<dyn Tool>>,
    /// Plugin-contributed tools (highest lookup precedence: a plugin can
    /// shadow a built-in of the same name).
    dynamic: RwLock<HashMap<String, Arc<dyn Tool>>>,
    mcp: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ToolRegistry {
    pub fn new(builtin: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            builtin,
            dynamic: RwLock::new(HashMap::new()),
            mcp: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self.dynamic.read().unwrap().get(name) {
            return Some(t.clone());
        }
        if let Some(t) = self.builtin.iter().find(|t| t.name() == name) {
            return Some(t.clone());
        }
        self.mcp.read().unwrap().get(name).cloned()
    }

    /// All registered tools (plugin first, then built-ins, then MCP).
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = self.dynamic.read().unwrap().values().cloned().collect();
        out.extend(self.builtin.clone());
        out.extend(self.mcp.read().unwrap().values().cloned());
        out
    }

    pub fn names(&self) -> Vec<String> {
        self.list().iter().map(|t| t.name().to_string()).collect()
    }

    /// Register a plugin tool at runtime (overrides an existing tool of the
    /// same name, including built-ins). Returns whether the name was taken.
    pub fn register_tool(&self, tool: Arc<dyn Tool>) -> bool {
        let mut map = self.dynamic.write().unwrap();
        map.insert(tool.name().to_string(), tool).is_none()
    }

    /// Drop a plugin tool. Returns true when it was registered.
    pub fn unregister_tool(&self, name: &str) -> bool {
        self.dynamic.write().unwrap().remove(name).is_some()
    }

    /// Replace the entire set of MCP-provided tools (recomputed by the MCP
    /// bridge whenever servers connect/disconnect).
    pub fn set_mcp_tools(&self, tools: Vec<Arc<dyn Tool>>) {
        let mut map = HashMap::new();
        for t in tools {
            map.insert(t.name().to_string(), t);
        }
        *self.mcp.write().unwrap() = map;
    }

    /// Drop every MCP tool (all servers toggled off).
    pub fn clear_mcp_tools(&self) {
        self.mcp.write().unwrap().clear();
    }
}
