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

/// A tool with a config-supplied description (`tool_config.<name>.description`).
///
/// Wraps the real tool so every LLM schema builder sees the custom text while
/// execution, parameters and safety hints stay the built-in ones.
struct DescriptionOverride {
    inner: Arc<dyn Tool>,
    description: String,
}

#[async_trait::async_trait]
impl Tool for DescriptionOverride {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }
    async fn execute(&self, ctx: crate::ToolCtx, args: serde_json::Value) -> crate::ToolOutput {
        self.inner.execute(ctx, args).await
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn is_read_only_for(&self, args: &serde_json::Value) -> bool {
        self.inner.is_read_only_for(args)
    }
    fn destructive_hint(&self) -> Option<bool> {
        self.inner.destructive_hint()
    }
    fn read_only_hint(&self) -> Option<bool> {
        self.inner.read_only_hint()
    }
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
    /// Built-ins and every other tool switched off by config
    /// (`ssh.enabled: false`, `tool_config.<name>.enabled: false`). Built-ins
    /// stay in `builtin` so re-enabling needs no reconstruction, but every
    /// lookup and `list()` skips a disabled name in *every* slice, so it never
    /// reaches a provider request.
    disabled: RwLock<HashSet<String>>,
    /// Config-supplied description overrides (`tool_config.<name>.description`).
    /// Lookups wrap the tool so the LLM schema carries the custom text.
    descriptions: RwLock<HashMap<String, String>>,
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
            descriptions: RwLock::new(HashMap::new()),
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
        self.set_enabled(name, enabled);
    }

    /// Enable or disable a tool of **any** slice (built-in, dynamic or MCP) by
    /// name. A disabled name is skipped by every lookup ([`get`](Self::get),
    /// [`list`](Self::list), [`names`](Self::names),
    /// [`list_with_source`](Self::list_with_source),
    /// [`source_of`](Self::source_of)), so a disabled tool's schema never
    /// reaches a provider request. Unknown names may still be marked disabled
    /// so a future registration is honoured.
    pub fn set_enabled(&self, name: &str, enabled: bool) {
        let mut disabled = self.disabled.write().unwrap();
        if enabled {
            disabled.remove(name);
        } else {
            disabled.insert(name.to_string());
        }
    }

    /// Whether a tool of that name is currently switched off.
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.read().unwrap().contains(name)
    }

    /// Whether a built-in of that name is currently switched off.
    pub fn builtin_disabled(&self, name: &str) -> bool {
        self.is_disabled(name)
    }

    /// Clear the disabled set (every tool becomes visible again). Used when
    /// re-deriving the whole `tool_config` + `ssh.enabled` state on reload.
    pub fn clear_disabled(&self) {
        self.disabled.write().unwrap().clear();
    }

    /// Drop every configured description override.
    pub fn clear_description_overrides(&self) {
        self.descriptions.write().unwrap().clear();
    }

    /// Set (or clear, with `None`) a config-supplied description override for a
    /// tool. When set, every lookup wraps the tool so its schema carries the
    /// custom text; execution and parameters are unaffected.
    pub fn set_description_override(&self, name: &str, description: Option<&str>) {
        let mut map = self.descriptions.write().unwrap();
        match description.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => {
                map.insert(name.to_string(), d.to_string());
            }
            None => {
                map.remove(name);
            }
        }
    }

    /// The description override in effect for a tool, if any.
    pub fn description_override(&self, name: &str) -> Option<String> {
        self.descriptions.read().unwrap().get(name).cloned()
    }

    /// Wrap a tool with its description override, if one is configured.
    fn with_override(&self, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        let name = tool.name();
        match self.descriptions.read().unwrap().get(name) {
            Some(description) => Arc::new(DescriptionOverride {
                inner: tool,
                description: description.clone(),
            }),
            None => tool,
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if self.is_disabled(name) {
            return None;
        }
        if let Some(t) = self.dynamic.read().unwrap().get(name) {
            return Some(self.with_override(t.clone()));
        }
        if let Some(t) = self.builtin.iter().find(|t| t.name() == name) {
            return Some(self.with_override(t.clone()));
        }
        let mcp = self.mcp.read().unwrap().get(name).cloned();
        mcp.map(|t| self.with_override(t))
    }

    /// All registered tools (plugin first, then built-ins, then MCP).
    ///
    /// A dynamic tool shadowing a built-in of the same name is listed once,
    /// as dynamic (same precedence as [`ToolRegistry::get`]). Without this
    /// dedup the provider request would carry two tools under one name and
    /// e.g. DeepSeek rejects it with 400 "Tool names must be unique".
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.list_with_source()
            .into_iter()
            .map(|(_, t)| t)
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.list().iter().map(|t| t.name().to_string()).collect()
    }

    /// Every registered tool with the slice it lives in (F7-7), in the same
    /// order as [`ToolRegistry::list`]: plugin/dynamic first, then built-ins,
    /// then MCP. A dynamic tool shadowing a built-in is listed once, as
    /// dynamic. Disabled names are skipped in every slice and a configured
    /// description override is applied.
    pub fn list_with_source(&self) -> Vec<(ToolSource, Arc<dyn Tool>)> {
        let dynamic = self.dynamic.read().unwrap();
        let disabled = self.disabled.read().unwrap();
        let mut out: Vec<(ToolSource, Arc<dyn Tool>)> = dynamic
            .values()
            .filter(|t| !disabled.contains(t.name()))
            .map(|t| (ToolSource::Dynamic, t.clone()))
            .collect();
        out.extend(
            self.builtin
                .iter()
                .filter(|t| !dynamic.contains_key(t.name()) && !disabled.contains(t.name()))
                .map(|t| (ToolSource::Builtin, t.clone())),
        );
        drop(disabled);
        drop(dynamic);
        out.extend(
            self.mcp
                .read()
                .unwrap()
                .values()
                .filter(|t| !self.is_disabled(t.name()))
                .map(|t| (ToolSource::Mcp, t.clone())),
        );
        out.into_iter()
            .map(|(s, t)| (s, self.with_override(t)))
            .collect()
    }

    /// Like [`list_with_source`](Self::list_with_source) but includes tools
    /// currently disabled by config (so the Settings "Tools" tab can list and
    /// re-enable them). Description overrides are still applied.
    pub fn list_all_with_source(&self) -> Vec<(ToolSource, Arc<dyn Tool>)> {
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
        out.into_iter()
            .map(|(s, t)| (s, self.with_override(t)))
            .collect()
    }

    /// The tool's own (built-in) description, ignoring any config override.
    pub fn default_description(&self, name: &str) -> Option<String> {
        if let Some(t) = self.dynamic.read().unwrap().get(name) {
            return Some(t.description().to_string());
        }
        if let Some(t) = self.builtin.iter().find(|t| t.name() == name) {
            return Some(t.description().to_string());
        }
        self.mcp
            .read()
            .unwrap()
            .get(name)
            .map(|t| t.description().to_string())
    }

    /// Where a tool by that name comes from (same precedence as `get`).
    pub fn source_of(&self, name: &str) -> Option<ToolSource> {
        if self.is_disabled(name) {
            return None;
        }
        if self.dynamic.read().unwrap().contains_key(name) {
            return Some(ToolSource::Dynamic);
        }
        if self.builtin.iter().any(|t| t.name() == name) {
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

    /// `set_enabled` hides a tool in *every* slice (dynamic/MCP too), and a
    /// description override changes the description the schema carries while
    /// leaving the tool usable.
    #[test]
    fn set_enabled_covers_all_slices_and_description_override_applies() {
        let reg = ToolRegistry::new(vec![Arc::new(StubTool("bash"))]);
        reg.register_tool(Arc::new(StubTool("code_ast")));
        reg.set_mcp_tools(vec![Arc::new(StubTool("mcp__srv__thing"))]);

        reg.set_enabled("code_ast", false);
        reg.set_enabled("mcp__srv__thing", false);
        assert!(reg.get("code_ast").is_none());
        assert!(reg.get("mcp__srv__thing").is_none());
        assert!(!reg.names().contains(&"code_ast".to_string()));
        assert!(!reg.names().contains(&"mcp__srv__thing".to_string()));
        assert_eq!(reg.source_of("code_ast"), None);
        // list_all still reports them (Settings can re-enable).
        let all: Vec<String> = reg
            .list_all_with_source()
            .into_iter()
            .map(|(_, t)| t.name().to_string())
            .collect();
        assert!(all.contains(&"code_ast".to_string()));
        assert!(all.contains(&"mcp__srv__thing".to_string()));

        reg.set_enabled("code_ast", true);
        assert!(reg.get("code_ast").is_some());

        assert_eq!(reg.default_description("bash").as_deref(), Some("stub"));
        reg.set_description_override("bash", Some("custom bash text"));
        let bash = reg.get("bash").unwrap();
        assert_eq!(bash.description(), "custom bash text");
        assert_eq!(reg.default_description("bash").as_deref(), Some("stub"));
        // Clearing restores the built-in description.
        reg.set_description_override("bash", None);
        assert_eq!(reg.get("bash").unwrap().description(), "stub");
    }
}
