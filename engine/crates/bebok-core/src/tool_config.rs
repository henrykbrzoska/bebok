//! Per-tool enable/disable + custom descriptions (`tool_config`).
//!
//! Shape (merged global + project, project wins per tool, deep-merged per
//! field):
//!
//! ```jsonc
//! {
//!   "tool_config": {
//!     "bash":     { "enabled": true,  "description": "Run a shell command" },
//!     "rm":       { "enabled": false },
//!     "web__fetch": { "description": "Fetch a URL (team wrapper)" }
//!   }
//! }
//! ```
//!
//! Missing tool / missing `enabled` = enabled; empty or missing `description`
//! = the tool's built-in description. The instance store applies this map to
//! its [`ToolRegistry`] (disabled names are hidden from every lookup, so a
//! disabled tool's schema never reaches the model) and turns `description`
//! into a registry description override.
//!
//! [`ToolRegistry`]: bebok_tools::ToolRegistry

use std::collections::BTreeMap;

use serde_json::Value;

/// One parsed per-tool entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolConfigEntry {
    /// Whether the tool is enabled (missing field defaults to `true`).
    pub enabled: bool,
    /// Custom description, `None`/empty meaning "use the built-in one".
    pub description: Option<String>,
}

impl ToolConfigEntry {
    fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        Some(Self {
            enabled,
            description,
        })
    }
}

/// The parsed `tool_config` map. Project layering already happened in
/// [`crate::config::loader::apply`], so this is just the merged view.
#[derive(Debug, Clone, Default)]
pub struct ToolConfigMap {
    entries: BTreeMap<String, ToolConfigEntry>,
}

impl ToolConfigMap {
    /// Parse a `tool_config` section. Non-object entries are skipped.
    pub fn parse(section: &Value) -> Self {
        let mut entries = BTreeMap::new();
        if let Some(map) = section.as_object() {
            for (name, value) in map {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if let Some(entry) = ToolConfigEntry::from_value(value) {
                    entries.insert(name.to_string(), entry);
                } else {
                    tracing::warn!("tool_config: ignoring {name:?} = {value} (not an object)");
                }
            }
        }
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(name, entry)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ToolConfigEntry)> {
        self.entries.iter()
    }

    /// The entry for a tool, if configured.
    pub fn entry(&self, name: &str) -> Option<&ToolConfigEntry> {
        self.entries.get(name)
    }

    /// Whether a tool is enabled. Unknown tool = enabled (default `true`).
    pub fn is_enabled(&self, name: &str) -> bool {
        self.entry(name).map(|e| e.enabled).unwrap_or(true)
    }

    /// The custom description for a tool, if one is configured.
    pub fn custom_description(&self, name: &str) -> Option<&str> {
        self.entry(name)
            .and_then(|e| e.description.as_deref())
            .filter(|d| !d.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_means_enabled_and_empty_description_is_none() {
        let map = ToolConfigMap::parse(&json!({
            "bash": { "enabled": false },
            "rm":   { "description": "delete things" },
            "grep": { "enabled": true, "description": "   " },
        }));
        assert!(!map.is_enabled("bash"));
        assert!(map.is_enabled("rm"), "missing enabled defaults to true");
        assert!(map.is_enabled("grep"));
        assert!(map.is_enabled("unknown"), "unknown tool is enabled");
        assert_eq!(map.custom_description("rm"), Some("delete things"));
        assert_eq!(map.custom_description("grep"), None, "blank description");
        assert_eq!(map.custom_description("bash"), None);
    }

    #[test]
    fn non_object_section_and_entries_are_ignored() {
        assert!(ToolConfigMap::parse(&json!("nope")).is_empty());
        let map = ToolConfigMap::parse(&json!({ "bash": "enabled", "grep": {} }));
        assert!(map.is_enabled("bash"), "invalid entry skipped -> default");
        assert!(map.is_enabled("grep"));
    }
}
