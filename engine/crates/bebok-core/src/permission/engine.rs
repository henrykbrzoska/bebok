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
    /// F9-5: the rule an "always allow" answer persists. When an explicit
    /// rule produced the verdict this is that rule's (already generic)
    /// pattern; when the *default* `Ask` applied it is the tool-level glob
    /// `tool(*)` — never the exact call string, which would only ever match
    /// the one path/command that was asked about.
    pub suggested_rule: String,
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
    /// WP-AUTOVERIFY (F8-1): `verify.frontend = auto` switches the *default*
    /// verdict of the `browser_*` family (all but `browser_eval`) to `Allow`
    /// so autonomous verification does not stall on prompts. Explicit
    /// project/global/agent rules are evaluated first and still win.
    browser_auto: AtomicBool,
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
            browser_auto: AtomicBool::new(false),
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

    /// Enable/disable the `browser_*` auto-allow default (WP-AUTOVERIFY /
    /// F8-1, driven by `verify.frontend = auto`).
    pub fn set_browser_auto(&self, enabled: bool) {
        self.browser_auto.store(enabled, Ordering::Relaxed);
    }

    pub fn browser_auto(&self) -> bool {
        self.browser_auto.load(Ordering::Relaxed)
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
                suggested_rule: tool_rule(tool),
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
                            suggested_rule: denied_pattern.clone(),
                            pattern: denied_pattern,
                        };
                    }
                }
            }
            return Evaluation {
                verdict: verdict(action),
                suggested_rule: pattern.clone(),
                pattern,
            };
        }
        if let Some((pattern, action)) = self.project.read().unwrap().compiled.first_match(&call) {
            return Evaluation {
                verdict: verdict(action),
                suggested_rule: pattern.clone(),
                pattern,
            };
        }
        if let Some((pattern, action)) = self.global.read().unwrap().compiled.first_match(&call) {
            return Evaluation {
                verdict: verdict(action),
                suggested_rule: pattern.clone(),
                pattern,
            };
        }
        // `fetch` and the `browser_*` family (WP-BROWSER / F6-18) are `Ask`
        // even when a call is read-only: reading a URL or a rendered page can
        // expose local services. Projects relax this with explicit rules
        // (e.g. `"browser_*": "allow"`).
        //
        // WP-AUTOVERIFY (F8-1): with `verify.frontend = auto` the family
        // defaults to `Allow` instead — except `browser_eval`, which runs
        // arbitrary JavaScript and keeps asking. Only the *default* arm is
        // affected: any explicit rule above already returned.
        let verdict = if self.browser_auto.load(Ordering::Relaxed) && browser_auto_allowed(tool) {
            Verdict::Allow
        } else if is_mutating(tool, read_only) {
            Verdict::Ask
        } else {
            Verdict::Allow
        };
        Evaluation {
            verdict,
            suggested_rule: tool_rule(tool),
            pattern: call,
        }
    }

    /// Whether `call` (a canonical `tool(args)` string) matches `rule`.
    pub fn rule_matches(rule: &str, call: &str) -> bool {
        build_glob(rule)
            .ok()
            .map(|g| g.compile_matcher().is_match(call))
            .unwrap_or(false)
    }

    /// Persist `ask → allow` for `pattern` to the project config and update the
    /// in-memory project layer (no restart needed). The engine is shared by
    /// every session of the directory (parent and sub-agent sessions alike),
    /// so the rule applies to all of them at once.
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

/// Whether a call should be flagged mutating/dangerous for display (F7-1)
/// and, by the same rule, is *not* eligible for the engine's auto-`Allow`
/// default: true for anything that isn't safely read-only, plus `fetch` and
/// the `browser_*` family, which stay `Ask` even when read-only (see
/// `evaluate`'s default arm) because they can reach local network services.
pub fn is_mutating(tool: &str, read_only: bool) -> bool {
    !read_only || tool == "fetch" || tool.starts_with("browser_")
}

/// The tool-level allow rule "always allow this tool" writes (F9-5):
/// `write_file(*)`, `bash(*)`, `mcp__github__*`-style names stay as given.
pub fn tool_rule(tool: &str) -> String {
    format!("{tool}(*)")
}

/// The `browser_*` tools whose default becomes `Allow` under
/// `verify.frontend = auto`: every member of the family except
/// `browser_eval` (arbitrary page JavaScript stays `Ask`).
pub fn browser_auto_allowed(tool: &str) -> bool {
    tool.starts_with("browser_") && tool != "browser_eval"
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

    /// WP-AUTOVERIFY (F8-1): `set_browser_auto(true)` flips the default of
    /// every `browser_*` tool except `browser_eval` to `Allow`; other
    /// defaults are untouched and the switch is reversible.
    #[test]
    fn browser_auto_switch_allows_the_family_except_eval() {
        let dir = tmp_dir("browser-auto");
        let engine = PermissionEngine::load_with_global(&dir, None);
        assert!(!engine.browser_auto());
        engine.set_browser_auto(true);
        assert!(engine.browser_auto());
        for tool in bebok_tools::browser::TOOL_NAMES {
            for read_only in [true, false] {
                let eval = engine.evaluate(
                    None,
                    tool,
                    &serde_json::json!({ "url": "http://localhost:4200/" }),
                    read_only,
                );
                let expected = if *tool == "browser_eval" {
                    Verdict::Ask
                } else {
                    Verdict::Allow
                };
                assert_eq!(eval.verdict, expected, "{tool} read_only={read_only}");
            }
        }
        // Unrelated defaults are unchanged: mutating tools still ask, fetch still asks.
        let eval = engine.evaluate(
            None,
            "write_file",
            &serde_json::json!({ "path": "x" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Ask);
        let eval = engine.evaluate(
            None,
            "fetch",
            &serde_json::json!({ "url": "http://x/" }),
            true,
        );
        assert_eq!(eval.verdict, Verdict::Ask);
        // Reversible.
        engine.set_browser_auto(false);
        let eval = engine.evaluate(
            None,
            "browser_open",
            &serde_json::json!({ "url": "http://x/" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Ask);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The user's explicit rules keep winning over the auto default: a
    /// project `deny`/`ask` on `browser_*` is honoured with the switch on.
    #[test]
    fn browser_auto_never_overrides_explicit_rules() {
        let dir = tmp_dir("browser-auto-rules");
        let project_cfg = dir.join(".bebok").join("config.json");
        std::fs::create_dir_all(project_cfg.parent().unwrap()).unwrap();
        std::fs::write(
            &project_cfg,
            r#"{ "permission": { "rules": [
                { "pattern": "browser_open(*)", "action": "deny" },
                { "pattern": "browser_screenshot*", "action": "ask" },
                { "pattern": "browser_eval*", "action": "allow" }
            ] } }"#,
        )
        .unwrap();
        let engine = PermissionEngine::load_with_global(&dir, None);
        engine.set_browser_auto(true);
        let eval = engine.evaluate(
            None,
            "browser_open",
            &serde_json::json!({ "url": "http://x/" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Deny);
        assert_eq!(eval.pattern, "browser_open(*)");
        let eval = engine.evaluate(None, "browser_screenshot", &serde_json::json!({}), true);
        assert_eq!(eval.verdict, Verdict::Ask);
        // ...and an explicit allow on eval beats the eval carve-out.
        let eval = engine.evaluate(
            None,
            "browser_eval",
            &serde_json::json!({ "js": "1" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Allow);
        // Tools without a rule get the auto default.
        let eval = engine.evaluate(
            None,
            "browser_click",
            &serde_json::json!({ "selector": "a" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Allow);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn browser_auto_allowed_excludes_eval_and_non_browser_tools() {
        assert!(browser_auto_allowed("browser_open"));
        assert!(browser_auto_allowed("browser_console"));
        assert!(browser_auto_allowed("browser_wait"));
        assert!(browser_auto_allowed("browser_find"));
        assert!(!browser_auto_allowed("browser_eval"));
        assert!(!browser_auto_allowed("bash"));
        assert!(!browser_auto_allowed("fetch"));
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

    /// F7-1: `is_mutating` backs both the engine's own auto-`Allow` default
    /// and the client-facing `mutating` flag - they must never disagree.
    #[test]
    fn is_mutating_matches_the_auto_allow_default() {
        assert!(!is_mutating("read_file", true), "plain read-only: safe");
        assert!(is_mutating("write_file", false), "mutating tool");
        assert!(is_mutating("bash", false), "mutating tool");
        // `fetch` and `browser_*` stay flagged even when classified read-only.
        assert!(is_mutating("fetch", true));
        assert!(is_mutating("browser_screenshot", true));
        assert!(is_mutating("browser_get_text", true));
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

    /// F9-5: a default `Ask` suggests the tool-level rule, an explicit rule
    /// suggests itself, and persisting the suggestion makes every later call
    /// of that tool pass (the old per-call pattern only matched one path).
    #[test]
    fn suggested_rule_is_tool_level_and_sticks_for_other_paths() {
        let dir = tmp_dir("suggested");
        let engine = PermissionEngine::load_with_global(&dir, None);
        let a = engine.evaluate(
            None,
            "write_file",
            &serde_json::json!({ "path": "src/a.ts" }),
            false,
        );
        assert_eq!(a.verdict, Verdict::Ask);
        assert_eq!(a.suggested_rule, "write_file(*)");
        engine.always_allow(&a.suggested_rule).unwrap();
        let b = engine.evaluate(
            None,
            "write_file",
            &serde_json::json!({ "path": "src/b.ts" }),
            false,
        );
        assert_eq!(b.verdict, Verdict::Allow);
        // Another tool is not covered.
        let c = engine.evaluate(None, "bash", &serde_json::json!({ "command": "ls" }), false);
        assert_eq!(c.verdict, Verdict::Ask);
        assert_eq!(c.suggested_rule, "bash(*)");

        // An explicit `ask` rule suggests its own (generic) pattern.
        let project = dir.join(".bebok/config.json");
        std::fs::write(
            &project,
            r#"{ "permission": { "rules": [ { "pattern": "bash(git *)", "action": "ask" } ] } }"#,
        )
        .unwrap();
        engine.reload();
        let d = engine.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "git push" }),
            false,
        );
        assert_eq!(d.verdict, Verdict::Ask);
        assert_eq!(d.suggested_rule, "bash(git *)");
        assert!(PermissionEngine::rule_matches(
            "write_file(*)",
            "write_file(x/y.ts)"
        ));
        assert!(!PermissionEngine::rule_matches("write_file(*)", "bash(ls)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
