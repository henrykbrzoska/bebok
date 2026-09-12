//! Permission engine: Chain-of-Responsibility over rule layers.
//!
//! Resolution order: YOLO (auto-allow) -> agent overrides -> project ->
//! global -> default (`Allow` for read-only tools, `Ask` for mutating
//! tools). A project/global Deny cannot be overridden by an agent Allow.
//! Globs are compiled once per instance and recompiled only on rule
//! changes (`always allow`) or an explicit [`PermissionEngine::reload`].

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use globset::{GlobSet, GlobSetBuilder};
use serde_json::Value;

use super::matcher::{build_glob, call_string};
use super::rule::{Action, Rule};
use super::store::persist_project_rule;
use crate::error::Result;

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
    pub(crate) fn first_match(&self, call: &str) -> Option<(String, Action)> {
        let idx = self.set.matches(call).into_iter().next()?;
        Some((self.patterns[idx].clone(), self.actions[idx]))
    }

    fn first_deny(&self, call: &str) -> Option<String> {
        self.set
            .matches(call)
            .into_iter()
            .find(|&idx| self.actions[idx] == Action::Deny)
            .map(|idx| self.patterns[idx].clone())
    }
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
        let project_rules = super::store::read_rules_from(&project_path);
        let global_rules = global_path
            .map(super::store::read_rules_from)
            .unwrap_or_default();
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
        let project_rules = super::store::read_rules_from(&self.project_path);
        let global_rules = self
            .global_path
            .as_deref()
            .map(super::store::read_rules_from)
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
            if action == Action::Allow {
                for configured in [&self.project, &self.global] {
                    if let Some(denied_pattern) =
                        configured.read().unwrap().compiled.first_deny(&call)
                    {
                        return Evaluation {
                            verdict: Verdict::Deny,
                            pattern: denied_pattern,
                        };
                    }
                }
            }
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
        // `fetch` and the `browser_*` family (WP-BROWSER / F6-18) are `Ask`
        // even when a call is read-only: reading a URL or a rendered page can
        // expose local services. Projects relax this with explicit rules
        // (e.g. `"browser_*": "allow"`).
        let verdict = if read_only && tool != "fetch" && !tool.starts_with("browser_") {
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

#[cfg(test)]
mod tests {
    use super::super::store::persist_project_rule;
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bebok-perm-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
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
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "rm -rf x" }),
            false,
        );
        assert_eq!(
            eval.verdict,
            Verdict::Allow,
            "project rule must override global"
        );

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
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "rm -rf x" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Deny);
        // An unrelated call falls through to the default.
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "git status" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Ask);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_read_only_allow_mutating_ask() {
        let dir = tmp_dir("defaults");
        let engine = PermissionEngine::load_with_global(&dir, None);
        // read-only default: Allow
        let eval = engine.evaluate(
            None,
            "read_file",
            &serde_json::json!({ "path": "a.txt" }),
            true,
        );
        assert_eq!(eval.verdict, Verdict::Allow);
        // mutating default: Ask with the canonical call as its pattern
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "git status" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Ask);
        assert_eq!(eval.pattern, "bash(git status)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_deny_wins_over_agent_allow() {
        let dir = tmp_dir("agent-deny-precedence");
        let agent = CompiledLayer::compile(&[Rule {
            pattern: "bash(rm *)".into(),
            action: Action::Allow,
        }]);
        let global = dir.join("global.json");
        std::fs::write(
            &global,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();
        let engine = PermissionEngine::load_with_global(&dir, Some(&global));
        let call = serde_json::json!({ "command": "rm -rf x" });
        assert_eq!(
            engine.evaluate(Some(&agent), "bash", &call, false).verdict,
            Verdict::Deny
        );

        std::fs::write(&global, "{}").unwrap();
        let project = dir.join(".bebok/config.json");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        std::fs::write(
            &project,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();
        engine.reload();
        assert_eq!(
            engine.evaluate(Some(&agent), "bash", &call, false).verdict,
            Verdict::Deny
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fetch_defaults_to_ask_even_for_get_and_head() {
        let dir = tmp_dir("fetch-default");
        let engine = PermissionEngine::load_with_global(&dir, None);
        for method in ["GET", "HEAD"] {
            let eval = engine.evaluate(
                None,
                "fetch",
                &serde_json::json!({"url": "http://127.0.0.1:8787/config", "method": method}),
                true,
            );
            assert_eq!(eval.verdict, Verdict::Ask, "{method}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// WP-BROWSER (F6-18): every `browser_*` tool is `Ask` in a fresh config,
    /// whether the call is classified read-only (screenshot, get_text) or not.
    #[test]
    fn browser_tools_default_to_ask_even_when_read_only() {
        let dir = tmp_dir("browser-default");
        let engine = PermissionEngine::load_with_global(&dir, None);
        for tool in bebok_tools::browser::TOOL_NAMES {
            for read_only in [true, false] {
                let eval = engine.evaluate(
                    None,
                    tool,
                    &serde_json::json!({ "url": "http://127.0.0.1:8787/" }),
                    read_only,
                );
                assert_eq!(eval.verdict, Verdict::Ask, "{tool} read_only={read_only}");
            }
        }
        // The read-only default for everything else is untouched.
        let eval = engine.evaluate(None, "read_file", &serde_json::json!({}), true);
        assert_eq!(eval.verdict, Verdict::Allow);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The config-only override: a `browser_*` glob rule beats the default.
    #[test]
    fn browser_glob_rule_can_allow_the_family() {
        let dir = tmp_dir("browser-rule");
        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [ { "pattern": "browser_*", "action": "allow" } ] } }"#,
        )
        .unwrap();
        let engine = PermissionEngine::load_with_global(&dir, None);
        let eval = engine.evaluate(None, "browser_screenshot", &serde_json::json!({}), true);
        assert_eq!(eval.verdict, Verdict::Allow);
        let _ = std::fs::remove_dir_all(dir);
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
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Allow);

        // Persisted file reloads cleanly into a fresh engine.
        let engine2 = PermissionEngine::load_with_global(&dir, None);
        let eval = engine2.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
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

        let before = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
        assert_eq!(before.verdict, Verdict::Ask);

        engine.always_allow("bash(pwd)").unwrap();
        let after = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
        assert_eq!(after.verdict, Verdict::Allow);

        // Only one rule remains in the file (upsert, not duplicate).
        let text = std::fs::read_to_string(&project_cfg).unwrap();
        let value: Value = crate::config::jsonc::parse(&text).unwrap();
        let rules = super::super::rule::parse_rules(&value["permission"]);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, Action::Allow);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_picks_up_external_config_changes() {
        let dir = tmp_dir("reload");
        let engine = PermissionEngine::load_with_global(&dir, None);
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "rm -rf x" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Ask);

        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] } }"#,
        )
        .unwrap();
        engine.reload();
        let eval = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "rm -rf x" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Deny);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_via_store_module_path() {
        // Guards the engine -> store persistence seam (atomic write).
        let dir = tmp_dir("seam");
        let path = dir.join(".bebok").join("config.json");
        persist_project_rule(
            &path,
            &Rule {
                pattern: "bash(pwd)".into(),
                action: Action::Allow,
            },
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("bash(pwd)")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
