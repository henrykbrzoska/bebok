//! Glob matching strategy for permission rules.
//!
//! Rules are glob patterns over the canonical call string
//! `tool(arg-text)`, e.g. `bash(git *)`, `edit(*)`, or plain tool-name
//! globs such as `mcp__github__*`.

use globset::{Glob, GlobBuilder};
use serde_json::Value;

use crate::error::{CoreError, Result};

pub(crate) fn build_glob(pattern: &str) -> Result<Glob> {
    // literal_separator(false): `*` also matches `/` (arguments such as file
    // paths or shell commands contain separators).
    // backslash_escape(false): backslashes stay literal (Windows paths).
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .backslash_escape(false)
        .build()
        .map_err(|e| CoreError::Other(format!("invalid permission pattern {pattern:?}: {e}")))
}

pub(crate) fn full_pattern(tool: &str, args_glob: &str) -> String {
    format!("{tool}({args_glob})")
}

/// The argument text used for glob matching, derived from a call's JSON args.
pub fn call_arg_text(args: &Value) -> String {
    if let Some(s) = args.as_str() {
        return s.to_string();
    }
    if let Some(map) = args.as_object() {
        // Prefer the single "interesting" string parameter when present so
        // patterns like `bash(git *)` or `read_file(src/*)` read naturally.
        for key in ["command", "path", "pattern"] {
            if let Some(s) = map.get(key).and_then(Value::as_str) {
                return s.to_string();
            }
        }
    }
    serde_json::to_string(args).unwrap_or_default()
}

/// The canonical call string matched against rule patterns: `tool(arg-text)`.
pub fn call_string(tool: &str, args: &Value) -> String {
    format!("{tool}({})", call_arg_text(args))
}

#[cfg(test)]
mod tests {
    use super::super::engine::CompiledLayer;
    use super::super::rule::{Action, Rule};

    #[test]
    fn matches_paren_patterns() {
        let layer = CompiledLayer::compile(&[Rule {
            pattern: "bash(rm *)".into(),
            action: Action::Deny,
        }]);
        assert_eq!(
            layer.first_match("bash(rm -rf /tmp/x)"),
            Some(("bash(rm *)".to_string(), Action::Deny))
        );
        assert_eq!(layer.first_match("bash(git status)"), None);
        assert!(layer.first_match("bash(rm -rf /tmp/a b)").is_some());

        let all = CompiledLayer::compile(&[Rule {
            pattern: "write_file(*)".into(),
            action: Action::Allow,
        }]);
        assert!(all.first_match("write_file(src/main.rs)").is_some());
    }

    #[test]
    fn matches_tool_name_globs() {
        let layer = CompiledLayer::compile(&[Rule {
            pattern: "mcp__github__*".into(),
            action: Action::Ask,
        }]);
        assert!(layer.first_match("mcp__github__create_issue({\"title\":\"x\"})").is_some());
        assert_eq!(layer.first_match("bash(git status)"), None);
    }

    #[test]
    fn first_rule_wins_within_a_layer() {
        let layer = CompiledLayer::compile(&[
            Rule { pattern: "bash(git *)".into(), action: Action::Deny },
            Rule { pattern: "bash(git status)".into(), action: Action::Allow },
        ]);
        // Deny comes first and must win.
        let (_, action) = layer.first_match("bash(git status)").unwrap();
        assert_eq!(action, Action::Deny);
    }
}
