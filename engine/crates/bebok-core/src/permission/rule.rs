//! Permission rules (config shapes).
//!
//! Parses the `permission` config section into [`Rule`]s.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::matcher::full_pattern;

/// The action a rule prescribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    #[default]
    Allow,
    Ask,
    Deny,
}

/// A permission rule: a glob pattern plus an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub pattern: String,
    #[serde(default)]
    pub action: Action,
}

/// Parse a `permission` config section into rules.
///
/// Tolerates three shapes:
///
/// ```jsonc
/// { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] }        // canonical
/// { "pattern": "bash(rm *)", "action": "deny" }                         // single rule
/// { "bash": "ask", "edit": "allow",                                     // SPEC §6 per-tool
///   "bash2": [ { "pattern": "git *", "action": "allow" } ] }
/// [ { "pattern": "bash(rm *)", "action": "deny" } ]                     // bare array
/// ```
pub fn parse_rules(value: &Value) -> Vec<Rule> {
    let mut out = Vec::new();
    match value {
        Value::Array(items) => {
            for item in items {
                if let Some(rule) = rule_from_object(item, None) {
                    out.push(rule);
                }
            }
        }
        Value::Object(map) => {
            // A single rule placed directly as the whole permission section.
            if map.contains_key("pattern") {
                if let Some(rule) = rule_from_object(value, None) {
                    out.push(rule);
                }
                return out;
            }
            if let Some(rules) = map.get("rules").and_then(Value::as_array) {
                for item in rules {
                    if let Some(rule) = rule_from_object(item, None) {
                        out.push(rule);
                    }
                }
            }
            // SPEC §6 per-tool shorthand: tool -> action string or rule list.
            for (tool, v) in map {
                if tool == "rules" {
                    continue;
                }
                match v {
                    Value::String(action) => {
                        if let Some(action) = parse_action(action) {
                            out.push(Rule {
                                pattern: full_pattern(tool, "*"),
                                action,
                            });
                        }
                    }
                    Value::Array(items) => {
                        for item in items {
                            if let Some(rule) = rule_from_object(item, Some(tool)) {
                                out.push(rule);
                            }
                        }
                    }
                    Value::Object(_) => {
                        if let Some(rule) = rule_from_object(v, Some(tool)) {
                            out.push(rule);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

pub(crate) fn rule_from_object(v: &Value, tool: Option<&str>) -> Option<Rule> {
    let o = v.as_object()?;
    let inner = o.get("pattern")?.as_str()?;
    let action = o.get("action").and_then(Value::as_str).and_then(parse_action)?;
    let pattern = match tool {
        // Per-tool shorthand stores argument globs; wrap them into the
        // canonical `tool(arg-glob)` form.
        Some(tool) => format!("{tool}({inner})"),
        None => inner.to_string(),
    };
    Some(Rule { pattern, action })
}

pub(crate) fn parse_action(s: &str) -> Option<Action> {
    match s {
        "allow" => Some(Action::Allow),
        "ask" => Some(Action::Ask),
        "deny" => Some(Action::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_rules_list() {
        let v: Value = serde_json::from_str(
            r#"{ "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] }"#,
        )
        .unwrap();
        let rules = parse_rules(&v);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "bash(rm *)");
        assert_eq!(rules[0].action, Action::Deny);
    }

    #[test]
    fn parses_spec_per_tool_shorthand() {
        let v: Value = serde_json::from_str(
            r#"{
                "edit": "allow",
                "bash": [ { "pattern": "git *", "action": "allow" }, { "pattern": "rm *", "action": "deny" } ],
                "webfetch": "ask"
            }"#,
        )
        .unwrap();
        let rules = parse_rules(&v);
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0], Rule { pattern: "edit(*)".into(), action: Action::Allow });
        assert_eq!(rules[1], Rule { pattern: "bash(git *)".into(), action: Action::Allow });
        assert_eq!(rules[2], Rule { pattern: "bash(rm *)".into(), action: Action::Deny });
        assert_eq!(rules[3], Rule { pattern: "webfetch(*)".into(), action: Action::Ask });
    }

    #[test]
    fn parses_single_rule_and_bare_array() {
        let single: Value = serde_json::from_str(r#"{ "pattern": "bash(rm *)", "action": "deny" }"#).unwrap();
        assert_eq!(parse_rules(&single)[0].pattern, "bash(rm *)");

        let arr: Value = serde_json::from_str(
            r#"[ { "pattern": "bash(git *)", "action": "allow" } ]"#,
        )
        .unwrap();
        assert_eq!(parse_rules(&arr).len(), 1);
    }
}
