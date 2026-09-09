//! Permission engine (SPEC §3.6, milestone M2).
//!
//! Tool execution goes through an `allow / ask / deny` gate. Rules are glob
//! patterns over the canonical call string `tool(arg-text)`, e.g. `bash(git *)`,
//! `edit(*)`, or plain tool-name globs such as `mcp__github__*`.
//!
//! Resolution order: agent overrides → project rules → global rules → default
//! (`Allow` for read-only tools, `Ask` for mutating tools). Globs are compiled
//! once per instance (`globset::GlobSet`) and recompiled only when a rule is
//! added (`always allow`) or the config is reloaded.
//!
//! `ask` suspends the agent loop: a oneshot channel is registered per request,
//! a `permission.asked` event reaches the clients, and the loop awaits. The
//! decision endpoint answers through the oneshot; `always` persists a project
//! rule (`ask → allow`) to `<project>/.bebok/config.json`.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::jsonc::JsoncDocument;
use crate::error::{CoreError, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

/// The verdict of evaluating a call before any user interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask,
    Deny,
}

/// Result of evaluating one tool call against every rule layer.
#[derive(Debug, Clone)]
pub struct Evaluation {
    pub verdict: Verdict,
    /// The pattern that produced the verdict: the matched rule's pattern, or
    /// the canonical call string when the default `Ask` applies. Used as the
    /// session decision-cache key and as the basis for `always allow` rules.
    pub pattern: String,
}

/// Answer a user gives for one `permission.asked` request.
#[derive(Debug, Clone, Copy)]
pub struct PermissionAnswer {
    pub allow: bool,
    pub always: bool,
}

/// A per-session cached decision for one `(tool, pattern)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedDecision {
    Allow,
    Deny,
}

/// A unique key for the per-session decision cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DecisionKey(pub String, pub String);

/// Result of resolving a pending permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The request was found and the answer delivered.
    Resolved,
    /// No pending request with that id (already resolved or never existed).
    NotFound,
}

// ---------------------------------------------------------------------------
// Rules parsing (config shapes)
// ---------------------------------------------------------------------------

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

fn rule_from_object(v: &Value, tool: Option<&str>) -> Option<Rule> {
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

fn parse_action(s: &str) -> Option<Action> {
    match s {
        "allow" => Some(Action::Allow),
        "ask" => Some(Action::Ask),
        "deny" => Some(Action::Deny),
        _ => None,
    }
}

fn full_pattern(tool: &str, args_glob: &str) -> String {
    format!("{tool}({args_glob})")
}

/// Read the permission rules from a config file (missing/invalid -> empty).
pub fn read_rules_from(path: &Path) -> Vec<Rule> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match crate::config::jsonc::parse(&text) {
        Ok(v) => parse_rules(v.get("permission").unwrap_or(&Value::Null)),
        Err(e) => {
            tracing::warn!("invalid config {}: {e}", path.display());
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical call string
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Compiled layers
// ---------------------------------------------------------------------------

/// One rule layer (agent / project / global) compiled into a globset.
#[derive(Debug, Clone)]
pub struct CompiledLayer {
    patterns: Vec<String>,
    actions: Vec<Action>,
    set: GlobSet,
}

impl CompiledLayer {
    pub fn compile(rules: &[Rule]) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut patterns = Vec::new();
        let mut actions = Vec::new();
        for rule in rules {
            match build_glob(&rule.pattern) {
                Ok(glob) => {
                    builder.add(glob);
                    patterns.push(rule.pattern.clone());
                    actions.push(rule.action);
                }
                Err(e) => tracing::warn!("{e}; skipping rule {:?}", rule.pattern),
            }
        }
        let set = builder.build().unwrap_or_else(|e| {
            tracing::warn!("permission glob compile error: {e}");
            GlobSetBuilder::new().build().expect("empty globset builds")
        });
        Self {
            patterns,
            actions,
            set,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// The first rule (by config order) whose glob matches `call`.
    fn first_match(&self, call: &str) -> Option<(String, Action)> {
        let idx = self.set.matches(call).into_iter().next()?;
        Some((self.patterns[idx].clone(), self.actions[idx]))
    }
}

fn build_glob(pattern: &str) -> Result<Glob> {
    // literal_separator(false): `*` also matches `/` (arguments such as file
    // paths or shell commands contain separators).
    // backslash_escape(false): backslashes stay literal (Windows paths).
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .backslash_escape(false)
        .build()
        .map_err(|e| CoreError::Other(format!("invalid permission pattern {pattern:?}: {e}")))
}

/// Runtime layer state: the source rules plus their compiled globset.
#[derive(Debug, Clone)]
struct Layer {
    rules: Vec<Rule>,
    compiled: CompiledLayer,
}

impl Layer {
    fn new(rules: Vec<Rule>) -> Self {
        let compiled = CompiledLayer::compile(&rules);
        Self { rules, compiled }
    }

    /// Replace the action of an existing rule with the same pattern, or append
    /// the rule. Recompiles the layer.
    fn upsert(&mut self, rule: Rule) {
        match self.rules.iter_mut().find(|r| r.pattern == rule.pattern) {
            Some(existing) => existing.action = rule.action,
            None => self.rules.push(rule),
        }
        self.compiled = CompiledLayer::compile(&self.rules);
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Per-instance permission engine.
///
/// GlobSets are compiled once per instance and recompiled only on rule changes
/// (`always allow`) or an explicit [`PermissionEngine::reload`] (the hook a
/// future `config.changed` flow calls).
pub struct PermissionEngine {
    root: PathBuf,
    project_path: PathBuf,
    global_path: Option<PathBuf>,
    project: RwLock<Layer>,
    global: RwLock<Layer>,
    /// YOLO mode: when set, every tool call is auto-allowed without asking.
    yolo: AtomicBool,
}

impl PermissionEngine {
    /// Build the engine for a project directory using the platform global
    /// config file.
    pub fn load(root: &Path) -> Self {
        let global = dirs::config_dir().map(|d| d.join("bebok").join("config.json"));
        Self::load_with_global(root, global.as_deref())
    }

    /// Build the engine with an explicit global config file (`None` disables
    /// the global layer; used by tests for hermetic behaviour).
    pub fn load_with_global(root: &Path, global_path: Option<&Path>) -> Self {
        let project_path = root.join(".bebok").join("config.json");
        let project_rules = read_rules_from(&project_path);
        let global_rules = global_path.map(read_rules_from).unwrap_or_default();
        Self {
            root: root.to_path_buf(),
            project_path,
            global_path: global_path.map(Path::to_path_buf),
            project: RwLock::new(Layer::new(project_rules)),
            global: RwLock::new(Layer::new(global_rules)),
            yolo: AtomicBool::new(false),
        }
    }

    /// Project root the engine is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Enable/disable YOLO mode (auto-allow everything, no asks).
    pub fn set_yolo(&self, enabled: bool) {
        self.yolo.store(enabled, Ordering::Relaxed);
    }

    pub fn yolo(&self) -> bool {
        self.yolo.load(Ordering::Relaxed)
    }

    /// Recompile rules from the current config files (`config.changed` hook).
    pub fn reload(&self) {
        let project_rules = read_rules_from(&self.project_path);
        let global_rules = self
            .global_path
            .as_deref()
            .map(read_rules_from)
            .unwrap_or_default();
        *self.project.write().unwrap() = Layer::new(project_rules);
        *self.global.write().unwrap() = Layer::new(global_rules);
    }

    /// Evaluate one tool call. Resolution order: YOLO (auto-allow) → agent
    /// overrides → project → global → default (`Allow` read-only / `Ask` mutating).
    pub fn evaluate(
        &self,
        agent: Option<&CompiledLayer>,
        tool: &str,
        args: &Value,
        read_only: bool,
    ) -> Evaluation {
        let call = call_string(tool, args);
        if self.yolo.load(Ordering::Relaxed) {
            return Evaluation {
                verdict: Verdict::Allow,
                pattern: call,
            };
        }
        if let Some(layer) = agent
            && let Some((pattern, action)) = layer.first_match(&call)
        {
            return Evaluation {
                verdict: verdict(action),
                pattern,
            };
        }
        if let Some((pattern, action)) = self.project.read().unwrap().compiled.first_match(&call) {
            return Evaluation {
                verdict: verdict(action),
                pattern,
            };
        }
        if let Some((pattern, action)) = self.global.read().unwrap().compiled.first_match(&call) {
            return Evaluation {
                verdict: verdict(action),
                pattern,
            };
        }
        let verdict = if read_only {
            Verdict::Allow
        } else {
            Verdict::Ask
        };
        Evaluation {
            verdict,
            pattern: call,
        }
    }

    /// Persist `ask → allow` for `pattern` to the project config and update the
    /// in-memory project layer (no restart needed).
    pub fn always_allow(&self, pattern: &str) -> Result<()> {
        let rule = Rule {
            pattern: pattern.to_string(),
            action: Action::Allow,
        };
        persist_project_rule(&self.project_path, &rule)?;
        self.project.write().unwrap().upsert(rule);
        Ok(())
    }
}

fn verdict(action: Action) -> Verdict {
    match action {
        Action::Allow => Verdict::Allow,
        Action::Ask => Verdict::Ask,
        Action::Deny => Verdict::Deny,
    }
}

// ---------------------------------------------------------------------------
// Persistence of `always allow` (project config file)
// ---------------------------------------------------------------------------

/// Persist an upserted rule under `permission.rules` of the project config,
/// preserving every unrelated top-level key, comment and bit of formatting
/// (via `JsoncDocument::with_set`). Atomic write (tmp + rename).
pub fn persist_project_rule(project_config: &Path, rule: &Rule) -> Result<()> {
    let mut rules: Vec<Rule> = Vec::new();
    let existing: Option<JsoncDocument> = match std::fs::read_to_string(project_config) {
        Ok(text) => match JsoncDocument::parse(&text) {
            Ok(doc) => {
                let permission = doc.value().get("permission").unwrap_or(&Value::Null);
                rules = parse_rules(permission);
                Some(doc)
            }
            Err(e) => {
                // Never clobber a config we cannot parse.
                return Err(CoreError::Other(format!(
                    "cannot update invalid project config {}: {e}",
                    project_config.display()
                )));
            }
        },
        Err(_) => None, // file does not exist yet -> create it
    };

    // Upsert: replacing the same pattern keeps first-match ordering sane (a
    // stale `ask`/`deny` for the same pattern must not shadow the `allow`).
    match rules.iter_mut().find(|r| r.pattern == rule.pattern) {
        Some(existing_rule) => existing_rule.action = rule.action,
        None => rules.push(rule.clone()),
    }

    let permission_value = serde_json::json!({ "rules": rules });
    let output = match &existing {
        Some(doc) => doc.with_set("permission", &permission_value),
        None => serde_json::to_string_pretty(&serde_json::json!({
            "permission": permission_value
        }))
        .unwrap_or_default(),
    };

    if let Some(dir) = project_config.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(project_config, &output)?;
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.json".to_string());
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bebok-perm-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

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

    #[test]
    fn project_overrides_global() {
        let dir = tmp_dir("precedence");
        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "allow" } ] } }"#,
        )
        .unwrap();
        let global = dir.join("global.json");
        std::fs::write(
            &global,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();

        let engine = PermissionEngine::load_with_global(&dir, Some(&global));
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "rm -rf x" }), false);
        assert_eq!(eval.verdict, Verdict::Allow, "project rule must override global");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_rule_applies_when_no_project_rule() {
        let dir = tmp_dir("global-only");
        let global = dir.join("global.json");
        std::fs::write(
            &global,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();
        let engine = PermissionEngine::load_with_global(&dir, Some(&global));
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "rm -rf x" }), false);
        assert_eq!(eval.verdict, Verdict::Deny);
        // An unrelated call falls through to the default.
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "git status" }), false);
        assert_eq!(eval.verdict, Verdict::Ask);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_read_only_allow_mutating_ask() {
        let dir = tmp_dir("defaults");
        let engine = PermissionEngine::load_with_global(&dir, None);
        // read-only default: Allow
        let eval = engine.evaluate(None, "read_file", &serde_json::json!({ "path": "a.txt" }), true);
        assert_eq!(eval.verdict, Verdict::Allow);
        // mutating default: Ask with the canonical call as its pattern
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "git status" }), false);
        assert_eq!(eval.verdict, Verdict::Ask);
        assert_eq!(eval.pattern, "bash(git status)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn always_allow_persists_and_recompiles() {
        let dir = tmp_dir("always");
        let engine = PermissionEngine::load_with_global(&dir, None);
        engine.always_allow("bash(pwd)").unwrap();

        let path = dir.join(".bebok").join("config.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bash(pwd)"));
        assert!(text.contains("allow"));

        // In-memory layer updated -> no reload required.
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "pwd" }), false);
        assert_eq!(eval.verdict, Verdict::Allow);

        // Persisted file reloads cleanly into a fresh engine.
        let engine2 = PermissionEngine::load_with_global(&dir, None);
        let eval = engine2.evaluate(None, "bash", &serde_json::json!({ "command": "pwd" }), false);
        assert_eq!(eval.verdict, Verdict::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn always_allow_upserts_same_pattern() {
        let dir = tmp_dir("always-upsert");
        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [ { "pattern": "bash(pwd)", "action": "ask" } ] } }"#,
        )
        .unwrap();
        let engine = PermissionEngine::load_with_global(&dir, None);

        let before = engine.evaluate(None, "bash", &serde_json::json!({ "command": "pwd" }), false);
        assert_eq!(before.verdict, Verdict::Ask);

        engine.always_allow("bash(pwd)").unwrap();
        let after = engine.evaluate(None, "bash", &serde_json::json!({ "command": "pwd" }), false);
        assert_eq!(after.verdict, Verdict::Allow);

        // Only one rule remains in the file (upsert, not duplicate).
        let text = std::fs::read_to_string(&project_cfg).unwrap();
        let value: Value = crate::config::jsonc::parse(&text).unwrap();
        let rules = parse_rules(&value["permission"]);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, Action::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_picks_up_external_config_changes() {
        let dir = tmp_dir("reload");
        let engine = PermissionEngine::load_with_global(&dir, None);
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "rm -rf x" }), false);
        assert_eq!(eval.verdict, Verdict::Ask);

        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();
        engine.reload();
        let eval = engine.evaluate(None, "bash", &serde_json::json!({ "command": "rm -rf x" }), false);
        assert_eq!(eval.verdict, Verdict::Deny);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
