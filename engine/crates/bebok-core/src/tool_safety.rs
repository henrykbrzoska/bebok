//! Explicit per-tool safety categories (WP-CHAT4 / F7-7).
//!
//! Every tool the engine knows gets one of four *informational* categories
//! that drive the safety dots in the chat transcript and the "Tool safety"
//! section of Settings > Permissions:
//!
//! * `safe` (green) - read-only inspection: `read_file`, `list_dir`, `tree`,
//!   `grep`, `glob`/`find`, `stat`, ..., `browser_get_text`;
//! * `caution` (yellow) - reaches out or delegates but does not write the
//!   project: `fetch`, `task`, `fleet`, the interactive `browser_*` calls,
//!   MCP tools annotated `readOnlyHint: true`;
//! * `dangerous` (orange) - writes/deletes/executes: `write_file`,
//!   `edit_file`, `append_file`, `mkdir`, `rm`, `bash`, ..., MCP tools
//!   annotated `destructiveHint: true`;
//! * `uncategorized` (gray) - everything else (unknown MCP tools, plugin
//!   tools, new built-ins nobody classified yet).
//!
//! The category is resolved once per call by the permission gate from the
//! built-in default table plus the `tool_safety` config map (global, project
//! override allowed; keys are tool names or globs over tool names) and
//! stamped onto the tool part. It is **independent of the permission
//! engine**: a category never changes an allow/ask/deny verdict, and the
//! verdict never changes a category.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bebok_tools::{Tool, ToolRegistry, ToolSource};

/// One of the four safety categories. `Uncategorized` is the explicit
/// "nobody decided yet" bucket, distinct from "not resolved".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SafetyCategory {
    Safe,
    Caution,
    Dangerous,
    #[default]
    Uncategorized,
}

impl SafetyCategory {
    /// All categories in display order.
    pub const ALL: [SafetyCategory; 4] = [
        SafetyCategory::Safe,
        SafetyCategory::Caution,
        SafetyCategory::Dangerous,
        SafetyCategory::Uncategorized,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SafetyCategory::Safe => "safe",
            SafetyCategory::Caution => "caution",
            SafetyCategory::Dangerous => "dangerous",
            SafetyCategory::Uncategorized => "uncategorized",
        }
    }

    /// Parse a config/API value (case-insensitive). `None` for anything
    /// that is not one of the four names.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "safe" => Some(SafetyCategory::Safe),
            "caution" => Some(SafetyCategory::Caution),
            "dangerous" => Some(SafetyCategory::Dangerous),
            "uncategorized" => Some(SafetyCategory::Uncategorized),
            _ => None,
        }
    }
}

/// Built-in default table: every built-in tool (plus the core's `task` /
/// `fleet` dynamic tools) with its category. Anything not listed here and
/// not carrying an MCP annotation is `Uncategorized`.
pub const BUILTIN_DEFAULTS: &[(&str, SafetyCategory)] = &[
    // --- safe: read-only inspection -------------------------------------
    ("read_file", SafetyCategory::Safe),
    ("head", SafetyCategory::Safe),
    ("tail", SafetyCategory::Safe),
    ("wc", SafetyCategory::Safe),
    ("list_dir", SafetyCategory::Safe),
    ("ls", SafetyCategory::Safe),
    ("tree", SafetyCategory::Safe),
    ("pwd", SafetyCategory::Safe),
    ("stat", SafetyCategory::Safe),
    ("du", SafetyCategory::Safe),
    ("glob", SafetyCategory::Safe),
    ("grep", SafetyCategory::Safe),
    ("find", SafetyCategory::Safe),
    ("sort", SafetyCategory::Safe),
    ("uniq", SafetyCategory::Safe),
    ("diff", SafetyCategory::Safe),
    ("which", SafetyCategory::Safe),
    ("realpath", SafetyCategory::Safe),
    ("basename", SafetyCategory::Safe),
    ("dirname", SafetyCategory::Safe),
    ("sha256sum", SafetyCategory::Safe),
    ("browser_get_text", SafetyCategory::Safe),
    // WP-AUTOVERIFY (F8-1): read-only verification helpers.
    ("browser_console", SafetyCategory::Safe),
    ("browser_wait", SafetyCategory::Safe),
    ("browser_find", SafetyCategory::Safe),
    // --- caution: reaches out / delegates / can write on request --------
    ("fetch", SafetyCategory::Caution),
    ("task", SafetyCategory::Caution),
    ("fleet", SafetyCategory::Caution),
    // WP-DELEGATION supervision tools: read the parent's own task map.
    ("task_status", SafetyCategory::Safe),
    ("task_wait", SafetyCategory::Safe),
    ("task_cancel", SafetyCategory::Caution),
    // F9-14: stops a background process started by `bash { background: true }`.
    ("bash_kill", SafetyCategory::Caution),
    // Code-index tools: read-only queries against the local tantivy index.
    ("code_index_status", SafetyCategory::Safe),
    ("code_index_search", SafetyCategory::Safe),
    // Code-graph tools: read-only queries over the cached dependency graph.
    ("code_graph_depends", SafetyCategory::Safe),
    ("code_graph_dependents", SafetyCategory::Safe),
    ("code_graph_impact", SafetyCategory::Safe),
    // AST-aware structural search: read-only queries.
    ("code_ast", SafetyCategory::Safe),
    ("base64", SafetyCategory::Caution),
    ("browser_open", SafetyCategory::Caution),
    ("browser_screenshot", SafetyCategory::Caution),
    ("browser_click", SafetyCategory::Caution),
    ("browser_type", SafetyCategory::Caution),
    ("browser_eval", SafetyCategory::Caution),
    // --- dangerous: writes / deletes / executes --------------------------
    ("write_file", SafetyCategory::Dangerous),
    ("edit_file", SafetyCategory::Dangerous),
    ("append_file", SafetyCategory::Dangerous),
    ("sed", SafetyCategory::Dangerous),
    ("mkdir", SafetyCategory::Dangerous),
    ("touch", SafetyCategory::Dangerous),
    ("cp", SafetyCategory::Dangerous),
    ("mv", SafetyCategory::Dangerous),
    ("rm", SafetyCategory::Dangerous),
    ("chmod", SafetyCategory::Dangerous),
    ("ln", SafetyCategory::Dangerous),
    ("gzip", SafetyCategory::Dangerous),
    ("bash", SafetyCategory::Dangerous),
];

/// The built-in default for a tool name (exact match in
/// [`BUILTIN_DEFAULTS`]).
pub fn builtin_default(name: &str) -> Option<SafetyCategory> {
    BUILTIN_DEFAULTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

/// Default category for a tool: the built-in table for known names, then
/// the tool's own declared annotations (MCP `destructiveHint: true` ->
/// dangerous, `readOnlyHint: true` -> caution), else `Uncategorized`.
///
/// Deliberately does **not** fall back to `Tool::is_read_only()`: the
/// engine's read-only classification defaults to `false` for anything it
/// cannot classify, which would silently turn every unknown tool into
/// "dangerous". Unknown must stay visibly gray until someone decides.
pub fn default_category(tool: &dyn Tool) -> SafetyCategory {
    if let Some(c) = builtin_default(tool.name()) {
        return c;
    }
    if tool.destructive_hint() == Some(true) {
        return SafetyCategory::Dangerous;
    }
    if tool.read_only_hint() == Some(true) {
        return SafetyCategory::Caution;
    }
    SafetyCategory::Uncategorized
}

/// One parsed `tool_safety` override: a tool name or a glob over tool names.
#[derive(Debug, Clone)]
struct Override {
    pattern: String,
    category: SafetyCategory,
    glob: Option<globset::GlobMatcher>,
}

/// The parsed `tool_safety` config map (global merged with project, project
/// wins per key). Exact names beat globs; among matching globs the most
/// specific (longest pattern) wins, so `mcp__github__*` beats `mcp__*`.
#[derive(Debug, Clone, Default)]
pub struct SafetyOverrides {
    exact: BTreeMap<String, SafetyCategory>,
    globs: Vec<Override>,
}

impl SafetyOverrides {
    /// Parse a `tool_safety` section (`{ "<name or glob>": "<category>" }`).
    /// Entries with an unknown category value are skipped with a warning;
    /// a non-object section yields no overrides.
    pub fn parse(section: &Value) -> Self {
        let mut out = Self::default();
        let Some(map) = section.as_object() else {
            return out;
        };
        for (key, value) in map {
            let Some(category) = value.as_str().and_then(SafetyCategory::parse) else {
                tracing::warn!("tool_safety: ignoring {key:?} = {value} (not a safety category)");
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            if key.contains(['*', '?', '[']) {
                match globset::GlobBuilder::new(key)
                    .literal_separator(false)
                    .build()
                {
                    Ok(glob) => out.globs.push(Override {
                        pattern: key.to_string(),
                        category,
                        glob: Some(glob.compile_matcher()),
                    }),
                    Err(e) => tracing::warn!("tool_safety: bad glob {key:?}: {e}"),
                }
            } else {
                out.exact.insert(key.to_string(), category);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.globs.is_empty()
    }

    /// The override that applies to `name`, if any, with the pattern that
    /// matched.
    pub fn lookup<'a>(&'a self, name: &'a str) -> Option<(SafetyCategory, &'a str)> {
        if let Some(c) = self.exact.get(name) {
            return Some((*c, name));
        }
        self.globs
            .iter()
            .filter(|o| o.glob.as_ref().is_some_and(|g| g.is_match(name)))
            .max_by_key(|o| o.pattern.len())
            .map(|o| (o.category, o.pattern.as_str()))
    }
}

/// Resolve the effective category of a tool: config override, else the
/// default derived from the tool itself.
pub fn categorize(tool: &dyn Tool, overrides: &SafetyOverrides) -> SafetyCategory {
    match overrides.lookup(tool.name()) {
        Some((c, _)) => c,
        None => default_category(tool),
    }
}

/// Resolve the effective category of a tool by name through the registry.
/// A name the registry does not know (a tool removed since the call was
/// recorded, or an MCP server that went away) is `Uncategorized` unless an
/// override names it.
pub fn categorize_by_name(
    registry: &ToolRegistry,
    name: &str,
    overrides: &SafetyOverrides,
) -> SafetyCategory {
    if let Some((c, _)) = overrides.lookup(name) {
        return c;
    }
    match registry.get(name) {
        Some(tool) => default_category(tool.as_ref()),
        None => builtin_default(name).unwrap_or(SafetyCategory::Uncategorized),
    }
}

/// One row of `GET /tools/safety`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSafetyEntry {
    pub name: String,
    /// `built-in`, `mcp:<server>` or `plugin`.
    pub source: String,
    pub category: SafetyCategory,
    pub default_category: SafetyCategory,
    pub is_override: bool,
    /// The `tool_safety` key that produced the override (the name itself
    /// or a glob), `None` without an override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_pattern: Option<String>,
}

/// Display source for a registry slice: dynamic tools that are part of the
/// built-in default table (`task`, `fleet`) are reported as built-in, every
/// other dynamic tool is a plugin tool; MCP tools carry their server name.
pub fn source_label(source: ToolSource, name: &str) -> String {
    match source {
        ToolSource::Builtin => "built-in".to_string(),
        ToolSource::Dynamic => {
            if builtin_default(name).is_some() {
                "built-in".to_string()
            } else {
                "plugin".to_string()
            }
        }
        ToolSource::Mcp => {
            let server = name
                .strip_prefix("mcp__")
                .and_then(|rest| rest.split("__").next())
                .unwrap_or("");
            format!("mcp:{server}")
        }
    }
}

/// Every tool the registry knows right now with its resolved category,
/// sorted by source (built-in, plugin, mcp) then name.
pub fn list_entries(registry: &ToolRegistry, overrides: &SafetyOverrides) -> Vec<ToolSafetyEntry> {
    let mut out: Vec<ToolSafetyEntry> = registry
        .list_with_source()
        .into_iter()
        .map(|(source, tool)| {
            let name = tool.name().to_string();
            let default_category = default_category(tool.as_ref());
            let (category, override_pattern) = match overrides.lookup(&name) {
                Some((c, pattern)) => (c, Some(pattern.to_string())),
                None => (default_category, None),
            };
            ToolSafetyEntry {
                source: source_label(source, &name),
                name,
                category,
                default_category,
                is_override: override_pattern.is_some(),
                override_pattern,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        source_rank(&a.source)
            .cmp(&source_rank(&b.source))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn source_rank(source: &str) -> u8 {
    match source {
        "built-in" => 0,
        "plugin" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use bebok_tools::{ToolCtx, ToolOutput, builtin_tools};
    use std::sync::Arc;

    struct Fake {
        name: &'static str,
        read_only_hint: Option<bool>,
        destructive_hint: Option<bool>,
    }

    #[async_trait]
    impl Tool for Fake {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({})
        }
        async fn execute(&self, _ctx: ToolCtx, _args: Value) -> ToolOutput {
            ToolOutput::new("", self.name)
        }
        fn read_only_hint(&self) -> Option<bool> {
            self.read_only_hint
        }
        fn destructive_hint(&self) -> Option<bool> {
            self.destructive_hint
        }
    }

    fn fake(name: &'static str, ro: Option<bool>, destructive: Option<bool>) -> Arc<dyn Tool> {
        Arc::new(Fake {
            name,
            read_only_hint: ro,
            destructive_hint: destructive,
        })
    }

    /// Every built-in tool has an explicit default: none may fall through
    /// to `Uncategorized` by accident.
    #[test]
    fn every_builtin_tool_is_categorized() {
        for tool in builtin_tools() {
            assert!(
                builtin_default(tool.name()).is_some(),
                "built-in tool {:?} has no entry in BUILTIN_DEFAULTS",
                tool.name()
            );
        }
        // And the two core dynamic tools.
        assert_eq!(builtin_default("task"), Some(SafetyCategory::Caution));
        assert_eq!(builtin_default("fleet"), Some(SafetyCategory::Caution));
        // AST-aware search: read-only, opt-in.
        assert_eq!(builtin_default("code_ast"), Some(SafetyCategory::Safe));
    }

    #[test]
    fn default_table_matches_the_brief() {
        for name in [
            "read_file",
            "list_dir",
            "tree",
            "grep",
            "glob",
            "find",
            "stat",
            "browser_get_text",
            "code_ast",
        ] {
            assert_eq!(builtin_default(name), Some(SafetyCategory::Safe), "{name}");
        }
        for name in [
            "fetch",
            "task",
            "fleet",
            "browser_open",
            "browser_screenshot",
            "browser_click",
            "browser_type",
            "browser_eval",
        ] {
            assert_eq!(
                builtin_default(name),
                Some(SafetyCategory::Caution),
                "{name}"
            );
        }
        for name in [
            "write_file",
            "edit_file",
            "append_file",
            "mkdir",
            "rm",
            "bash",
        ] {
            assert_eq!(
                builtin_default(name),
                Some(SafetyCategory::Dangerous),
                "{name}"
            );
        }
    }

    #[test]
    fn mcp_annotations_drive_the_default_and_unknown_stays_gray() {
        let ro = fake("mcp__srv__list", Some(true), None);
        assert_eq!(default_category(ro.as_ref()), SafetyCategory::Caution);
        let destructive = fake("mcp__srv__drop", None, Some(true));
        assert_eq!(
            default_category(destructive.as_ref()),
            SafetyCategory::Dangerous
        );
        // destructive wins over read-only when a server sets both.
        let both = fake("mcp__srv__weird", Some(true), Some(true));
        assert_eq!(default_category(both.as_ref()), SafetyCategory::Dangerous);
        // No annotation at all: uncategorized, never inferred from is_read_only.
        let unknown = fake("mcp__srv__mystery", None, None);
        assert_eq!(
            default_category(unknown.as_ref()),
            SafetyCategory::Uncategorized
        );
        // An explicit `destructiveHint: false` is not a read-only claim.
        let non_destructive = fake("mcp__srv__ping", None, Some(false));
        assert_eq!(
            default_category(non_destructive.as_ref()),
            SafetyCategory::Uncategorized
        );
    }

    #[test]
    fn overrides_exact_beats_glob_and_longest_glob_wins() {
        let overrides = SafetyOverrides::parse(&serde_json::json!({
            "mcp__*": "caution",
            "mcp__github__*": "dangerous",
            "mcp__github__get_issue": "safe",
            "bash": "CAUTION",
            "nonsense": "extreme",
            "": "safe",
        }));
        assert_eq!(
            overrides.lookup("mcp__github__get_issue"),
            Some((SafetyCategory::Safe, "mcp__github__get_issue"))
        );
        assert_eq!(
            overrides.lookup("mcp__github__create_issue"),
            Some((SafetyCategory::Dangerous, "mcp__github__*"))
        );
        assert_eq!(
            overrides.lookup("mcp__jira__search"),
            Some((SafetyCategory::Caution, "mcp__*"))
        );
        assert_eq!(
            overrides.lookup("bash"),
            Some((SafetyCategory::Caution, "bash"))
        );
        assert_eq!(overrides.lookup("nonsense"), None);
        assert_eq!(overrides.lookup("read_file"), None);
        assert!(SafetyOverrides::parse(&serde_json::json!("nope")).is_empty());
    }

    #[test]
    fn categorize_by_name_uses_override_then_registry_then_table() {
        let registry = ToolRegistry::new(builtin_tools());
        registry.set_mcp_tools(vec![fake("mcp__srv__list", Some(true), None)]);
        let overrides = SafetyOverrides::parse(&serde_json::json!({ "bash": "caution" }));
        assert_eq!(
            categorize_by_name(&registry, "bash", &overrides),
            SafetyCategory::Caution
        );
        assert_eq!(
            categorize_by_name(&registry, "read_file", &overrides),
            SafetyCategory::Safe
        );
        assert_eq!(
            categorize_by_name(&registry, "mcp__srv__list", &overrides),
            SafetyCategory::Caution
        );
        // A historical call of a tool that no longer exists: table, else gray.
        assert_eq!(
            categorize_by_name(&registry, "task", &overrides),
            SafetyCategory::Caution
        );
        assert_eq!(
            categorize_by_name(&registry, "mcp__gone__thing", &overrides),
            SafetyCategory::Uncategorized
        );
    }

    #[test]
    fn list_entries_reports_source_default_and_override() {
        let registry = ToolRegistry::new(builtin_tools());
        registry.set_mcp_tools(vec![
            fake("mcp__srv__list", Some(true), None),
            fake("mcp__srv__drop", None, Some(true)),
            fake("mcp__srv__mystery", None, None),
        ]);
        registry.register_tool(fake("plugin_thing", None, None));
        let overrides = SafetyOverrides::parse(&serde_json::json!({
            "mcp__srv__*": "safe",
            "rm": "caution",
        }));
        let entries = list_entries(&registry, &overrides);
        let find = |n: &str| entries.iter().find(|e| e.name == n).cloned().unwrap();

        let rm = find("rm");
        assert_eq!(rm.source, "built-in");
        assert_eq!(rm.default_category, SafetyCategory::Dangerous);
        assert_eq!(rm.category, SafetyCategory::Caution);
        assert!(rm.is_override);
        assert_eq!(rm.override_pattern.as_deref(), Some("rm"));

        let drop = find("mcp__srv__drop");
        assert_eq!(drop.source, "mcp:srv");
        assert_eq!(drop.default_category, SafetyCategory::Dangerous);
        assert_eq!(drop.category, SafetyCategory::Safe);
        assert_eq!(drop.override_pattern.as_deref(), Some("mcp__srv__*"));

        let plugin = find("plugin_thing");
        assert_eq!(plugin.source, "plugin");
        assert_eq!(plugin.category, SafetyCategory::Uncategorized);
        assert!(!plugin.is_override);

        let read = find("read_file");
        assert_eq!(read.category, SafetyCategory::Safe);
        assert!(!read.is_override);
        assert!(read.override_pattern.is_none());

        // Sorted: built-ins first, then plugin, then mcp; names ascending.
        let sources: Vec<&str> = entries.iter().map(|e| e.source.as_str()).collect();
        let first_plugin = sources.iter().position(|s| *s == "plugin").unwrap();
        let first_mcp = sources.iter().position(|s| s.starts_with("mcp:")).unwrap();
        assert!(sources[..first_plugin].iter().all(|s| *s == "built-in"));
        assert!(first_plugin < first_mcp);
    }

    #[test]
    fn category_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&SafetyCategory::Uncategorized).unwrap(),
            "\"uncategorized\""
        );
        let parsed: SafetyCategory = serde_json::from_str("\"dangerous\"").unwrap();
        assert_eq!(parsed, SafetyCategory::Dangerous);
        assert_eq!(SafetyCategory::parse(" Safe "), Some(SafetyCategory::Safe));
        assert_eq!(SafetyCategory::parse("green"), None);
    }
}
