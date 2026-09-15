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

use crate::config::{DelegationConfig, DelegationMode, ResolvedConfig};

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

/// Prompt section for the main (user-facing) agent, or `None` in `off` mode.
///
/// `fleet` is the live fleet state for the agent being prompted
/// ([`FleetContext::from_config`]); it only affects the `fleet` sentence and the
/// trailing `Fleet:` paragraph, never the `task` policy.
pub fn delegation_policy_note(cfg: &DelegationConfig, fleet: &FleetContext) -> Option<String> {
    let trigger = match cfg.mode {
        DelegationMode::Off => return None,
        DelegationMode::Auto => AUTO_TRIGGER,
        DelegationMode::Always => ALWAYS_TRIGGER,
    };
    let max = cfg.effective_max_concurrent();
    let model_line = match cfg.model_override() {
        Some(model) => format!(
            "Sub-agents run on `{model}` (configured override); do not pass a `model` argument \
             unless the user asks for a specific one.\n"
        ),
        None => String::new(),
    };
    // Offer the fleet as a spawn route whenever it can actually run.
    // Fleet-first: usable fleets are preferred, no per-prompt opt-in needed.
    let fleet_alt = if fleet.is_active() {
        " (prefer one `fleet` call with `tasks` - see the `Fleet:` line below)"
    } else {
        ""
    };
    Some(format!(
        "## Delegation policy (mode: {mode})\n\
         You are the MAIN THREAD: you own the conversation with the user, and you supervise \
         sub-agents that do the hands-on work.\n\
         {trigger}\n\
         How to delegate:\n\
         - Decompose first: list the parts, and for each part say which files/directories it \
         OWNS. Two sub-agents must never edit the same file; put shared edits (e.g. one README \
         or one routes file) into a single part or do them yourself after the others finish.\n\
         - Spawn every independent part in ONE assistant turn with parallel `task` calls using \
         `background: true`{fleet_alt}. Give each a short kebab-case `name`, the right preset \
         (`code` for edits, `ask` for research, `plan` for design, `debug` for failures) and a \
         crisp, standalone brief: goal, exact file ownership, acceptance criteria, what NOT to \
         touch, and the instruction to finish with a short report of what changed. The \
         sub-agent cannot see this conversation.\n\
         - At most {max} sub-agents run at once; extra ones queue automatically, so spawn all \
         parts up front and let the engine schedule them.\n\
         - Supervise: call `task_wait` (mode `all`, or `any` when you can act on partial \
         results) to collect results; use `task_status` to see what each child is doing \
         (last tool, last line, tokens) and `task_cancel` to stop one that went off-track. \
         Never end your turn with children still running - always `task_wait` first.\n\
         - Never spawn the same audit twice: before delegating, check `task_status` for a \
         live child covering the same scope — spawning a duplicate is refused, collect the \
         existing task with `task_wait` instead.\n\
         - Integrate: read the reports, resolve overlaps, run the project's build/tests once \
         if that is cheap, and give the user one consolidated summary (what each sub-agent \
         did, files touched, anything left open). NEVER redo work a sub-agent already \
         completed successfully; if one failed, either re-spawn it with a sharper brief or \
         finish that part yourself.\n\
         - Sequential dependencies (B needs A's output) are spawned after A returns, not \
         guessed.\n\
         - Narrate as you go (the user watches this chat, not the sub-agents): write ONE short \
         line of plain text before every new phase — when you delegate (\"Delegating: \
         api-orders (endpoint + tests), frontend-orders (page + nav), docs\"), before you \
         integrate, before you verify, before you fix something — and a 1-2 line summary right \
         after each sub-agent completes (what it changed, what it verified, anything open). No \
         filler, no restating the brief; a line per phase is enough.\n\
         - Verify claims: a sub-agent's report is a claim, not a fact. Before integrating, \
         re-check what matters yourself — run the build/tests it says it ran, `fetch` the \
         endpoint it says it added, open the page it says it changed — and only then tell the \
         user it is done. If the check fails, fix it (or re-spawn with a sharper brief); never \
         summarise a feature as implemented while something it needs is broken.\n\
         {model_line}\
         {heavy_line}\
         {fleet_line}\
         Do not delegate trivial single-file edits or lookups you can do in one or two tool \
         calls yourself.",
        mode = cfg.mode.as_str(),
        heavy_line = heavy_model_line(cfg),
        fleet_line = fleet.note(),
    ))
}

/// How to ask for the parent's (heavier) model for one sub-task under an
/// explicit `cheaper` policy; empty for the other policies (under the default
/// policy sub-agents already run on what the config says).
fn heavy_model_line(cfg: &DelegationConfig) -> String {
    match cfg.effective_model_policy() {
        crate::config::DelegationModelPolicy::Cheaper => "- Sub-agents run on a cheaper sibling \
of your model by default (policy `cheaper`). For a HEAVY part — a large refactor across many \
files, architecture/design decisions, debugging a failure that spans several files or layers, \
anything where a weaker model would likely get lost — pass `model: \"heavy\"` in that `task` \
call to give it your own model. Keep routine edits, tests, docs and research on the default.\n"
            .to_string(),
        _ => String::new(),
    }
}

const AUTO_TRIGGER: &str = "Delegate (rather than doing everything inline) whenever ANY of these \
holds:\n\
- the request has 2 or more independent parts (e.g. tests AND a new page AND docs);\n\
- it touches 2 or more areas of the project (e.g. api + frontend + docs, or several \
packages/apps);\n\
- it combines research/investigation with implementation.\n\
Otherwise do the work yourself with the normal tools.";

const ALWAYS_TRIGGER: &str = "Delegate EVERY non-trivial task: anything that needs more than a \
handful of tool calls, touches more than one file, or has more than one part is decomposed \
and handed to sub-agents. You do the planning, supervision and integration; sub-agents do the \
implementation. Only trivial one-shot answers and single small edits are done inline.";

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

    fn cfg(mode: DelegationMode) -> DelegationConfig {
        DelegationConfig {
            mode,
            max_concurrent: 3,
            model: None,
            model_policy: crate::config::DelegationModelPolicy::Inherit,
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
        delegation_policy_note(cfg, fleet).unwrap()
    }

    #[test]
    fn off_mode_has_no_policy_text() {
        assert!(
            delegation_policy_note(&cfg(DelegationMode::Off), &FleetContext::default()).is_none()
        );
        assert!(
            delegation_policy_note(&cfg(DelegationMode::Off), &fleet(&[("a", "ask", "m")]))
                .is_none()
        );
    }

    #[test]
    fn auto_mode_names_the_three_triggers_and_the_tools() {
        let note = note_with(
            &cfg(DelegationMode::Auto),
            &fleet(&[("ask-zai-glm-flash", "ask", "zai/glm-5.3-flash")]),
        );
        assert!(
            note.starts_with("## Delegation policy (mode: auto)"),
            "{note}"
        );
        assert!(note.contains("2 or more independent parts"), "{note}");
        assert!(note.contains("2 or more areas"), "{note}");
        assert!(
            note.contains("research/investigation with implementation"),
            "{note}"
        );
        for tool in [
            "`task`",
            "`background: true`",
            "`task_wait`",
            "`task_status`",
            "`task_cancel`",
            "`fleet`",
        ] {
            assert!(note.contains(tool), "missing {tool} in:\n{note}");
        }
        assert!(note.contains("OWNS"), "file ownership boundaries: {note}");
        assert!(note.contains("NEVER redo work"), "{note}");
        assert!(note.contains("Otherwise do the work yourself"), "{note}");
    }

    /// The roster is the part the static preset text could not know: the
    /// prompt must carry the *configured* labels, types and models, and say
    /// that `names` has to be copied from there.
    #[test]
    fn fleet_roster_is_injected_and_authoritative() {
        let note = note_with(
            &cfg(DelegationMode::Auto),
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
        assert!(note.contains("prefer one `fleet` call"), "{note}");
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
        let note = note_with(&cfg(DelegationMode::Auto), &ctx);
        assert!(note.contains("member(s)"), "{note}");
        assert!(note.contains("and 3 more member(s), same rules"), "{note}");
        assert!(!note.contains("`m22`"), "{note}");
    }

    #[test]
    fn usable_fleet_prefers_fleet_even_when_not_requested() {
        // Fleet-first: usability alone decides. Usable + requested still
        // prefers `fleet` ...
        let active = note_with(
            &cfg(DelegationMode::Auto),
            &fleet(&[("ask-a", "ask", "p/m")]),
        );
        assert!(active.contains("prefer one `fleet` call"), "{active}");
        assert!(
            active.contains("enabled here with 1 configured member(s)"),
            "{active}"
        );
        // ... and usable + NOT requested prefers `fleet` too.
        let unrequested = note_with(
            &cfg(DelegationMode::Auto),
            &fleet_requested(&[("ask-a", "ask", "p/m")], false),
        );
        assert!(
            unrequested.contains("enabled here with 1 configured member(s)"),
            "{unrequested}"
        );
        assert!(
            unrequested.contains("prefer one `fleet` call"),
            "{unrequested}"
        );
        assert!(
            unrequested.contains("prefer `fleet` for independent"),
            "{unrequested}"
        );
        assert!(
            !unrequested.contains("do NOT call `fleet`"),
            "{unrequested}"
        );
    }

    #[test]
    fn requested_but_unusable_fleet_falls_back_to_task() {
        let disabled = FleetContext {
            tool_available: true,
            enabled: false,
            members: vec![],
            requested: true,
        };
        let note = note_with(&cfg(DelegationMode::Auto), &disabled);
        assert!(note.contains("Fleet: NOT available"), "{note}");
        assert!(note.contains("`fleet.enabled` is false"), "{note}");
        assert!(
            note.contains("Fall back to sequential or parallel `task` calls"),
            "{note}"
        );
        assert!(!note.contains("see the `Fleet:` line below"), "{note}");
        assert!(!note.contains("prefer one `fleet` call"), "{note}");
    }

    #[test]
    fn disabled_or_empty_fleet_is_announced_instead_of_advertised() {
        // Unusable regardless of the legacy request flag: announced with a
        // reason, never advertised, and never offered as a spawn route.
        let disabled = FleetContext {
            tool_available: true,
            enabled: false,
            members: vec![],
            requested: false,
        };
        let note = note_with(&cfg(DelegationMode::Auto), &disabled);
        assert!(note.contains("Fleet: NOT available"), "{note}");
        assert!(note.contains("`fleet.enabled` is false"), "{note}");
        assert!(!note.contains("see the `Fleet:` line below"), "{note}");

        let empty = FleetContext {
            tool_available: true,
            enabled: true,
            members: vec![],
            requested: false,
        };
        let note = note_with(&cfg(DelegationMode::Auto), &empty);
        assert!(note.contains("Fleet: NOT available"), "{note}");
        assert!(note.contains("`fleet.members` is empty"), "{note}");
        assert!(!note.contains("see the `Fleet:` line below"), "{note}");
    }

    /// `fleet` is orchestrator-only (`request.rs` filters the tool out
    /// everywhere else), so the prompt of any other agent must not mention it.
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
        let note = note_with(&cfg(DelegationMode::Auto), &ctx);
        assert!(
            !note.contains("fleet"),
            "no fleet mention expected:\n{note}"
        );
        assert!(!note.contains("`fleet`"), "{note}");
        assert!(note.contains("`task`"), "{note}");
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
        assert!(note_with(&cfg(DelegationMode::Auto), &solo).contains("prefer one `fleet` call"));

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
        assert!(!note_with(&cfg(DelegationMode::Auto), &code).contains("fleet"));

        // Empty -> announced as unavailable, with fallback.
        let mut empty = ResolvedConfig::default();
        empty.fleet.enabled = true;
        let ctx = FleetContext::from_config(&empty, "orchestrator", true);
        assert!(!ctx.is_usable());
        assert!(!ctx.is_active());
        assert!(note_with(&cfg(DelegationMode::Auto), &ctx).contains("NOT available"));
    }

    #[test]
    fn always_mode_decomposes_every_non_trivial_task() {
        let note = note_with(&cfg(DelegationMode::Always), &FleetContext::default());
        assert!(
            note.starts_with("## Delegation policy (mode: always)"),
            "{note}"
        );
        assert!(note.contains("Delegate EVERY non-trivial task"), "{note}");
        assert!(!note.contains("Otherwise do the work yourself"), "{note}");
    }

    #[test]
    fn max_concurrent_and_model_override_are_stated() {
        let mut c = cfg(DelegationMode::Auto);
        c.max_concurrent = 5;
        c.model = Some("openai/gpt-5.6-terra".into());
        let note = note_with(&c, &FleetContext::default());
        assert!(note.contains("At most 5 sub-agents run at once"), "{note}");
        assert!(note.contains("`openai/gpt-5.6-terra`"), "{note}");

        // No override -> no model line at all.
        let note = note_with(&cfg(DelegationMode::Auto), &FleetContext::default());
        assert!(!note.contains("configured override"), "{note}");
        // Clamped: 0 -> 1.
        let mut c = cfg(DelegationMode::Always);
        c.max_concurrent = 0;
        assert!(note_with(&c, &FleetContext::default()).contains("At most 1 sub-agents"));
    }

    /// F9-7b / F9-10: narration per phase, verifying children's claims,
    /// and the `heavy` escape hatch under an explicit `cheaper` policy only
    /// (the default policy already runs children on the configured model).
    #[test]
    fn narration_claim_checks_and_heavy_hint() {
        let mut c = cfg(DelegationMode::Auto);
        c.model_policy = crate::config::DelegationModelPolicy::Cheaper;
        let note = note_with(&c, &FleetContext::default());
        assert!(note.contains("Narrate as you go"), "{note}");
        assert!(note.contains("before every new phase"), "{note}");
        assert!(note.contains("after each sub-agent completes"), "{note}");
        assert!(note.contains("claim, not a fact"), "{note}");
        assert!(note.contains("`model: \"heavy\"`"), "{note}");
        assert!(note.contains("policy `cheaper`"), "{note}");
        let note = note_with(&cfg(DelegationMode::Auto), &FleetContext::default());
        assert!(!note.contains("`model: \"heavy\"`"), "{note}");
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
