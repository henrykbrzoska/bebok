use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::RwLock;

use crate::tool::Tool;

/// Which slice of the registry a tool came from (F7-7: shown as the
/// `source` column of the tool-safety table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// A fixed built-in from `builtin_tools()`.
    Builtin,
    /// Registered at runtime through `register_tool` (plugins, but also the
    /// core's own `task`/`fleet` tools, which need a store back-reference).
    Dynamic,
    /// Contributed by the MCP bridge (`mcp__<server>__<tool>`).
    Mcp,
}

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
    /// Built-ins switched off by config (`ssh.enabled: false`). They stay in
    /// `builtin` so re-enabling needs no reconstruction, but every lookup and
    /// `list()` skips them, so they never reach a provider request.
    disabled: RwLock<HashSet<String>>,
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
            disabled: RwLock::new(HashSet::new()),
        }
    }

    /// Enable or disable a built-in tool by name (a no-op for names that are
    /// not built in). A disabled built-in is invisible to [`get`](Self::get),
    /// [`list`](Self::list) and [`names`](Self::names), so its schema is never
    /// sent to the model. A dynamic tool of the same name still wins, which
    /// keeps `register_tool` as the documented override path.
    pub fn set_builtin_enabled(&self, name: &str, enabled: bool) {
        if !self.builtin.iter().any(|t| t.name() == name) {
            return;
        }
        let mut disabled = self.disabled.write().unwrap();
        if enabled {
            disabled.remove(name);
        } else {
            disabled.insert(name.to_string());
        }
    }

    /// Whether a built-in of that name is currently switched off.
    pub fn builtin_disabled(&self, name: &str) -> bool {
        self.disabled.read().unwrap().contains(name)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self.dynamic.read().unwrap().get(name) {
            return Some(t.clone());
        }
        let disabled = self.disabled.read().unwrap();
        if let Some(t) = self
            .builtin
            .iter()
            .find(|t| t.name() == name && !disabled.contains(t.name()))
        {
            return Some(t.clone());
        }
        drop(disabled);
        self.mcp.read().unwrap().get(name).cloned()
    }

    /// All registered tools (plugin first, then built-ins, then MCP).
    ///
    /// A dynamic tool shadowing a built-in of the same name is listed once,
    /// as dynamic (same precedence as [`ToolRegistry::get`]). Without this
    /// dedup the provider request would carry two tools under one name and
    /// e.g. DeepSeek rejects it with 400 "Tool names must be unique".
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let dynamic = self.dynamic.read().unwrap();
        let disabled = self.disabled.read().unwrap();
        let mut out: Vec<Arc<dyn Tool>> = dynamic.values().cloned().collect();
        out.extend(
            self.builtin
                .iter()
                .filter(|t| !dynamic.contains_key(t.name()) && !disabled.contains(t.name()))
                .cloned(),
        );
        drop(disabled);
        drop(dynamic);
        out.extend(self.mcp.read().unwrap().values().cloned());
        out
    }

    pub fn names(&self) -> Vec<String> {
        self.list().iter().map(|t| t.name().to_string()).collect()
    }

    /// Every registered tool with the slice it lives in (F7-7), in the same
    /// order as [`ToolRegistry::list`]: plugin/dynamic first, then built-ins,
    /// then MCP. A dynamic tool shadowing a built-in is listed once, as
    /// dynamic.
    pub fn list_with_source(&self) -> Vec<(ToolSource, Arc<dyn Tool>)> {
        let dynamic = self.dynamic.read().unwrap();
        let mut out: Vec<(ToolSource, Arc<dyn Tool>)> = dynamic
            .values()
            .map(|t| (ToolSource::Dynamic, t.clone()))
            .collect();
        out.extend(
            self.builtin
                .iter()
                .filter(|t| !dynamic.contains_key(t.name()))
                .map(|t| (ToolSource::Builtin, t.clone())),
        );
        drop(dynamic);
        out.extend(
            self.mcp
                .read()
                .unwrap()
                .values()
                .map(|t| (ToolSource::Mcp, t.clone())),
        );
        out
    }

    /// Where a tool by that name comes from (same precedence as `get`).
    pub fn source_of(&self, name: &str) -> Option<ToolSource> {
        if self.dynamic.read().unwrap().contains_key(name) {
            return Some(ToolSource::Dynamic);
        }
        if self
            .builtin
            .iter()
            .any(|t| t.name() == name && !self.disabled.read().unwrap().contains(name))
        {
            return Some(ToolSource::Builtin);
        }
        if self.mcp.read().unwrap().contains_key(name) {
            return Some(ToolSource::Mcp);
        }
        None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCtx;
    use crate::tool::ToolOutput;
    use async_trait::async_trait;
    use serde_json::Value;

    struct StubTool(&'static str);

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({})
        }
        async fn execute(&self, _ctx: ToolCtx, _args: Value) -> ToolOutput {
            ToolOutput::new("", "")
        }
        fn is_read_only(&self) -> bool {
            true
        }
    }

    /// A dynamic tool shadowing a built-in must appear in `list()` exactly
    /// once (as dynamic) — otherwise the provider request carries two tools
    /// under one name and e.g. DeepSeek rejects it with 400
    /// "Tool names must be unique" (regression test)
    /// dynamically in `instance_store.rs` on top of the built-in).
    #[test]
    fn list_dedups_dynamic_shadow_of_builtin() {
        let reg = ToolRegistry::new(vec![Arc::new(StubTool("grep"))]);
        reg.register_tool(Arc::new(StubTool("grep")));
        let names = reg.names();
        assert_eq!(
            names.iter().filter(|n| *n == "grep").count(),
            1,
            "names: {names:?}"
        );
        assert_eq!(reg.source_of("grep"), Some(ToolSource::Dynamic));
    }

    /// `set_builtin_enabled("ssh", false)` must hide the built-in from every
    /// lookup path (`get`, `list`, `names`, `source_of`) so its schema is
    /// never sent to the model, and re-enabling must bring it back without
    /// rebuilding the registry.
    #[test]
    fn disabling_a_builtin_hides_it_everywhere() {
        let reg = ToolRegistry::new(vec![Arc::new(StubTool("ssh")), Arc::new(StubTool("bash"))]);
        assert!(reg.names().contains(&"ssh".to_string()));

        reg.set_builtin_enabled("ssh", false);
        assert!(reg.builtin_disabled("ssh"));
        assert!(reg.get("ssh").is_none(), "get() still returns the tool");
        assert!(
            !reg.names().contains(&"ssh".to_string()),
            "names() leaks it"
        );
        assert!(
            reg.list().iter().all(|t| t.name() != "ssh"),
            "list() leaks it"
        );
        assert_eq!(reg.source_of("ssh"), None);
        // The other tools are untouched.
        assert!(reg.names().contains(&"bash".to_string()));

        reg.set_builtin_enabled("ssh", true);
        assert!(!reg.builtin_disabled("ssh"));
        assert!(
            reg.get("ssh").is_some(),
            "re-enable did not restore the tool"
        );
        assert!(reg.names().contains(&"ssh".to_string()));
    }

    /// Unknown names are ignored instead of polluting the disabled set.
    #[test]
    fn enabling_an_unknown_builtin_is_a_no_op() {
        let reg = ToolRegistry::new(vec![Arc::new(StubTool("bash"))]);
        reg.set_builtin_enabled("not-a-tool", false);
        assert!(!reg.builtin_disabled("not-a-tool"));
    }
}
