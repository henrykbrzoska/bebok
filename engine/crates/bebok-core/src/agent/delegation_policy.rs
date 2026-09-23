//! WP-DELEGATION (F8-2): the delegation policy section of the main agent's
//! system prompt.
//!
//! Lives in its own file on purpose: the prompt assembler
//! (`bebok-server/src/services/turn.rs::assemble_prompt`) only *calls*
//! [`delegation_policy_note`] and appends the result, so the policy text can be
//! tuned (or tested) without touching the assembler. Sub-agents never receive
//! this section (see [`subagent_note`]): a child that recursively decomposes
//! its brief is the failure mode the depth guard exists for.
//!
//! The text is deliberately operational rather than aspirational: it names the
//! tools (`task` with `background: true`, `task_wait`, `task_status`,
//! `task_cancel`, `fleet`), the triggers that must lead to decomposition, and
//! the supervision loop the main thread has to run. `off` yields no text at all
//! - the tools stay available, the model just is not pushed to use them.
//!
//! [`FleetContext`] keeps the `fleet` part honest. The roster lives in the
//! project/global config, so the static preset text could only describe it
//! generically and the model had no way of learning the real member labels -
//! the first `fleet` call then failed with `fleet: unknown member(s)`. The
//! section now states the *current* fleet: the configured members, or that the
//! fleet is disabled/unconfigured, or nothing at all when the agent does not
//! even carry the tool.

use crate::config::{DelegationConfig, ResolvedConfig};

/// The only preset that carries the `fleet` tool: `agent/request.rs` withholds
/// the fan-out tool from every other agent (and from every sub-agent). The
/// prompt section follows the same rule — for any other agent the tool is not
/// in the list, so the text must not name it.
pub const FLEET_AGENT: &str = "orchestrator";

/// How many member rows the `Fleet:` line lists before it summarises the rest.
pub const MAX_FLEET_ROSTER: usize = 20;

/// One configured fleet member as the prompt shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetMemberInfo {
    pub name: String,
    pub agent: String,
    pub model: String,
}

/// The fleet as it exists *right now* for the agent the prompt is built for,
/// plus whether THIS prompt asked for it.
///
/// Fleet-first: the orchestrator prefers `fleet` over solo whenever the fleet
/// is usable (tool + config + members). [`requested`] keeps the legacy
/// per-prompt flag for message wording/back-compat only — it never gates
/// fan-out. [`is_active`] == [`is_usable`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetContext {
    /// This agent actually has the `fleet` tool ([`FLEET_AGENT`] only).
    pub tool_available: bool,
    /// `fleet.enabled` in the resolved config.
    pub enabled: bool,
    /// Configured members, in config order.
    pub members: Vec<FleetMemberInfo>,
    /// Per-prompt "Run as fleet" flag (`fleet: true` on the prompt body).
    /// Back-compat only: usable fleets are preferred regardless of this flag.
    pub requested: bool,
}

impl FleetContext {
    /// Snapshot of the resolved config, seen from `agent_name`, for a prompt
    /// whose "Run as fleet" switch is `fleet_requested`.
    pub fn from_config(cfg: &ResolvedConfig, agent_name: &str, fleet_requested: bool) -> Self {
        Self {
            tool_available: agent_name == FLEET_AGENT,
            enabled: cfg.is_fleet_enabled(),
            members: cfg
                .fleet_members()
                .iter()
                .map(|m| FleetMemberInfo {
                    name: m.name.clone(),
                    agent: m.agent.clone(),
                    model: m.model.clone(),
                })
                .collect(),
            requested: fleet_requested,
        }
    }

    /// `fleet` would actually run something if the model called it now.
    pub fn is_usable(&self) -> bool {
        self.tool_available && self.enabled && !self.members.is_empty()
    }

    /// Fan-out is wanted for this prompt: the fleet is usable (tool +
    /// config + members). Fleet-first: the legacy per-prompt flag is not
    /// required.
    pub fn is_active(&self) -> bool {
        self.is_usable()
    }

    /// The `Fleet:` paragraph, or an empty string when this agent has no
    /// `fleet` tool at all (naming a tool that is not in its list only invites
    /// a hallucinated call).
    ///
    /// Fleet-first: when usable the paragraph advertises the roster and
    /// prefers `fleet`, regardless of the legacy per-prompt flag. When
    /// unusable it names the reason and falls back to `task`.
    fn note(&self) -> String {
        if !self.tool_available {
            return String::new();
        }
        if !self.is_usable() {
            let why = if !self.tool_available {
                "this agent does not carry the `fleet` tool"
            } else if !self.enabled {
                "`fleet.enabled` is false"
            } else {
                "`fleet.members` is empty"
            };
            return format!(
                "Fleet: NOT available in this project ({why}), so a `fleet` call \
                 would only return an error. Fall back to sequential or parallel `task` calls \
                 with `background: true` - never skip the work because the fleet is missing.\n"
            );
        }
        let mut note = format!(
            "Fleet: enabled here with {} configured member(s). `fleet` runs ONLY these labelled \
             members (it cannot create members, presets or counts), and this roster is \
             authoritative - it is this project's current config:\n",
            self.members.len()
        );
        for m in self.members.iter().take(MAX_FLEET_ROSTER) {
            let mut line = format!("- `{}`", m.name);
            if !m.agent.is_empty() {
                line.push_str(&format!(" - agent `{}`", m.agent));
            }
            if !m.model.is_empty() {
                line.push_str(&format!(", model `{}`", m.model));
            }
            note.push_str(&line);
            note.push('\n');
        }
        if self.members.len() > MAX_FLEET_ROSTER {
            note.push_str(&format!(
                "- ... and {} more member(s), same rules\n",
                self.members.len() - MAX_FLEET_ROSTER
            ));
        }
        note.push_str(
            "Copy labels from this list into `names` (a label outside it fails with `fleet: \
             unknown member(s)`); `agents` selects by agent type; `names` + `agents` intersect \
             (AND). Broadcast `prompt` sends one instruction to every selected member, \
             heterogeneous `tasks` runs different prompts concurrently - each in its own \
             isolated session. With both filters omitted EVERY one of the members above runs.\n",
        );
        note.push_str(
            "The fleet is available for this project, so prefer `fleet` for independent \
             parallel subtasks and use heterogeneous `tasks` when the \
             subtasks differ. Only fall back to `task` calls when the subtasks are sequential \
             (B needs A's output), when the selection would exceed the requested count, or when \
             no configured member fits the work.\n",
        );
        note
    }
}

/// Short delegation note for the orchestrator: the fleet roster, the spawn
/// rule (members from the fleet list only), the concurrency cap, and the
/// supervision tools. `None` for agents without the `fleet` tool (naming a
/// tool that is not in their list only invites a hallucinated call).
pub fn delegation_policy_note(cfg: &DelegationConfig, fleet: &FleetContext) -> Option<String> {
    if !fleet.tool_available {
        return None;
    }
    let max = cfg.effective_max_concurrent();
    Some(format!(
        "## Delegation (fleet only, max {max} parallel)\n\
         You are the MAIN THREAD: you own the conversation with the user, and you supervise \
         sub-agents that do the hands-on work.\n\
         - Spawn work ONLY through `fleet`, choosing members by `names` from the roster \
         below. A name outside the roster fails — never invent member names, never pick a \
         preset or model directly.\n\
         - At most {max} sub-agents run at once; extra ones queue automatically.\n\
         - Supervise with `task_status` (what each child is doing), `task_wait` (collect \
         results; never end your turn with children still running), and `task_cancel` \
         (stop a child that loops or wanders, then re-delegate with a tighter brief).\n\
         {supervision}\
         {fleet_line}",
        supervision = supervision_note(),
        fleet_line = fleet.note(),
    ))
}

/// Supervision cadence block: how the main thread keeps children on-task
/// (bounded wait→status→act loop, looping/wandering flags, escalation).
fn supervision_note() -> String {
    "- Supervision cadence: keep children on-task with a bounded `task_wait` (60-150s) -> \
     `task_status` -> act loop; never block forever on one wait.\n\
     - Watch the flags: `task_status` marks a live child as looping (same tool/args again \
     and again) or wandering (tool calls drifting outside its brief); judge by agent type — \
     a `code` child repeating the same edit is looping, an `ask` child touching files is \
     wandering.\n\
     - Call it to order: `task_cancel` the flagged child with reason `looping` or \
     `wandering`, then immediately re-task the same part with a tighter brief (narrower \
     file ownership, explicit stop condition).\n\
     - Escalate: if the re-tasked child is flagged again, do not spawn a third time — \
     finish that part yourself.\n"
        .to_string()
}

/// Short note appended to every sub-agent's prompt so it behaves like a
/// worker: complete its own brief, respect the ownership boundaries it was
/// given, do not fan out further, and end with a report the parent can use.
pub fn subagent_note() -> &'static str {
    "You are a SUB-AGENT working on one delegated part of a larger task. Do exactly the brief \
     you were given, stay within the files/areas it assigns to you, do not delegate further \
     (do the work directly, including any workspace inspection you need), and finish with a \
     concise report that starts with `Status: PASS`, `Status: PASS WITH NOTES` or \
     `Status: FAIL`, then: what you changed (files), how you verified it (commands you actually \
     ran and their result — never claim a build or test you did not run), and anything the \
     coordinating agent still needs to do. With FAIL say what does not work and what you tried; \
     do not describe unfinished work as done."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DelegationConfig {
        DelegationConfig {
            max_concurrent: 3,
            ..DelegationConfig::default()
        }
    }

    /// Fleet as this project actually configures it: orchestrator + members.
    /// Fleet-first: tests pass the per-prompt flag explicitly, but the flag
    /// no longer gates anything.
    fn fleet(members: &[(&str, &str, &str)]) -> FleetContext {
        fleet_requested(members, true)
    }

    /// Same as [`fleet`] but with an explicit legacy per-prompt "Run as fleet"
    /// flag (back-compat only; it no longer gates fan-out).
    fn fleet_requested(members: &[(&str, &str, &str)], requested: bool) -> FleetContext {
        FleetContext {
            tool_available: true,
            enabled: true,
            members: members
                .iter()
                .map(|(name, agent, model)| FleetMemberInfo {
                    name: (*name).to_string(),
                    agent: (*agent).to_string(),
                    model: (*model).to_string(),
                })
                .collect(),
            requested,
        }
    }

    fn note_with(cfg: &DelegationConfig, fleet: &FleetContext) -> String {
        let mut fleet = fleet.clone();
        fleet.tool_available = true;
        delegation_policy_note(cfg, &fleet).expect("orchestrator fleet note is always Some")
    }

    #[test]
    fn note_names_fleet_only_spawn_and_supervision() {
        let note = note_with(&cfg(), &FleetContext::default());
        for kw in ["fleet", "names", "task_wait", "task_status", "task_cancel"] {
            assert!(note.contains(kw), "missing {kw}:\n{note}");
        }
        assert!(note.contains("At most 3 sub-agents"), "{note}");
    }

    /// The roster is the part the static preset text could not know: the
    /// prompt must carry the *configured* labels, types and models, and say
    /// that `names` has to be copied from there.
    #[test]
    fn fleet_roster_is_injected_and_authoritative() {
        let note = note_with(
            &cfg(),
            &fleet(&[
                ("ask-openai-gpt-4.1-nano", "ask", "openai/gpt-4.1-nano"),
                (
                    "code-deepseek-v4-flash",
                    "code",
                    "deepseek/deepseek-v4-flash",
                ),
            ]),
        );
        assert!(
            note.contains("enabled here with 2 configured member(s)"),
            "{note}"
        );
        assert!(
            note.contains("- `ask-openai-gpt-4.1-nano` - agent `ask`, model `openai/gpt-4.1-nano`"),
            "{note}"
        );
        assert!(note.contains("- `code-deepseek-v4-flash`"), "{note}");
        assert!(note.contains("unknown member(s)"), "{note}");
        assert!(
            note.contains("EVERY one of the members above runs"),
            "{note}"
        );
        // Requested + usable: the roster prefers `fleet` for this prompt.
        assert!(note.contains("prefer `fleet` for independent"), "{note}");
        assert!(note.contains("heterogeneous `tasks`"), "{note}");
    }

    #[test]
    fn fleet_roster_is_capped() {
        let many: Vec<(String, String, String)> = (0..MAX_FLEET_ROSTER + 3)
            .map(|i| (format!("m{i}"), "ask".to_string(), format!("p/m{i}")))
            .collect();
        let ctx = FleetContext {
            tool_available: true,
            enabled: true,
            members: many
                .iter()
                .map(|(name, agent, model)| FleetMemberInfo {
                    name: name.clone(),
                    agent: agent.clone(),
                    model: model.clone(),
                })
                .collect(),
            requested: true,
        };
        let note = note_with(&cfg(), &ctx);
        assert!(note.contains("member(s)"), "{note}");
        assert!(note.contains("and 3 more member(s), same rules"), "{note}");
        assert!(!note.contains("`m22`"), "{note}");
    }

    #[test]
    fn fleet_is_not_named_for_agents_without_the_tool() {
        let ctx = FleetContext {
            tool_available: false,
            enabled: true,
            members: vec![FleetMemberInfo {
                name: "ask-a".into(),
                agent: "ask".into(),
                model: "p/m".into(),
            }],
            requested: true,
        };
        assert!(
            delegation_policy_note(&cfg(), &ctx).is_none(),
            "non-orchestrator agents get no delegation note"
        );
    }

    #[test]
    fn fleet_context_comes_from_the_resolved_config() {
        let mut conf = ResolvedConfig::default();
        conf.fleet.enabled = true;
        conf.fleet.members.push(crate::config::FleetMember {
            name: "ask-openai-gpt-4.1-nano".into(),
            agent: "ask".into(),
            model: "openai/gpt-4.1-nano".into(),
        });

        // Fleet-first: usability alone decides. The legacy flag changes
        // nothing: usable means active, with or without it.
        let solo = FleetContext::from_config(&conf, "orchestrator", false);
        assert!(solo.tool_available);
        assert!(solo.is_usable());
        assert!(solo.is_active());
        assert!(note_with(&cfg(), &solo).contains("prefer `fleet` for independent"));

        let orchestrator = FleetContext::from_config(&conf, "orchestrator", true);
        assert!(orchestrator.tool_available);
        assert!(orchestrator.is_usable());
        assert!(orchestrator.is_active());
        assert_eq!(orchestrator.members.len(), 1);
        assert_eq!(orchestrator.members[0].name, "ask-openai-gpt-4.1-nano");

        // Same project, another main-thread agent: no `fleet` tool, no mention.
        let code = FleetContext::from_config(&conf, "code", true);
        assert!(!code.tool_available);
        assert!(!code.is_usable());
        assert!(!code.is_active());
        assert!(delegation_policy_note(&cfg(), &code).is_none());

        // Empty -> announced as unavailable, with fallback.
        let mut empty = ResolvedConfig::default();
        empty.fleet.enabled = true;
        let ctx = FleetContext::from_config(&empty, "orchestrator", true);
        assert!(!ctx.is_usable());
        assert!(!ctx.is_active());
        assert!(note_with(&cfg(), &ctx).contains("NOT available"));
    }

    #[test]
    fn delegation_note_header_and_cap() {
        let note = note_with(&cfg(), &FleetContext::default());
        assert!(
            note.starts_with("## Delegation (fleet only, max 3 parallel)"),
            "{note}"
        );
        assert!(note.contains("ONLY through `fleet`"), "{note}");
    }

    #[test]
    fn max_concurrent_is_stated() {
        let mut c = cfg();
        c.max_concurrent = 5;
        let note = note_with(&c, &FleetContext::default());
        assert!(note.contains("At most 5 sub-agents run at once"), "{note}");
        // No configured model override line anymore.
        assert!(!note.contains("configured override"), "{note}");
        // Clamped: 0 -> 1.
        let mut c = cfg();
        c.max_concurrent = 0;
        assert!(note_with(&c, &FleetContext::default()).contains("At most 1 sub-agents"));
    }

    #[test]
    fn subagent_note_shape() {
        let n = subagent_note();
        assert!(n.contains("`Status: PASS`") && n.contains("`Status: FAIL`"));
        assert!(n.contains("never claim a build or test you did not run"));
    }

    #[test]
    fn subagent_note_forbids_further_delegation() {
        let n = subagent_note();
        assert!(n.contains("SUB-AGENT"));
        assert!(n.contains("do not delegate further"));
        assert!(n.contains("report"));
    }
}
