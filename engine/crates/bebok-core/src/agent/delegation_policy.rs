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

use crate::config::{DelegationConfig, DelegationMode};

/// Prompt section for the main (user-facing) agent, or `None` in `off` mode.
pub fn delegation_policy_note(cfg: &DelegationConfig) -> Option<String> {
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
         `background: true` (or one `fleet` call with `tasks` when fleet members are \
         configured). Give each a short kebab-case `name`, the right preset (`code` for edits, \
         `ask` for research, `plan` for design, `debug` for failures) and a crisp, standalone \
         brief: goal, exact file ownership, acceptance criteria, what NOT to touch, and the \
         instruction to finish with a short report of what changed. The sub-agent cannot see \
         this conversation.\n\
         - At most {max} sub-agents run at once; extra ones queue automatically, so spawn all \
         parts up front and let the engine schedule them.\n\
         - Supervise: call `task_wait` (mode `all`, or `any` when you can act on partial \
         results) to collect results; use `task_status` to see what each child is doing \
         (last tool, last line, tokens) and `task_cancel` to stop one that went off-track. \
         Never end your turn with children still running - always `task_wait` first.\n\
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
         Do not delegate trivial single-file edits or lookups you can do in one or two tool \
         calls yourself.",
        mode = cfg.mode.as_str(),
        heavy_line = heavy_model_line(cfg),
    ))
}

/// F9-10: how to ask for the parent's (heavier) model for one sub-task
/// under the `cheaper` policy; empty for the other policies.
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
            model_policy: crate::config::DelegationModelPolicy::Cheaper,
        }
    }

    #[test]
    fn off_mode_has_no_policy_text() {
        assert!(delegation_policy_note(&cfg(DelegationMode::Off)).is_none());
    }

    #[test]
    fn auto_mode_names_the_three_triggers_and_the_tools() {
        let note = delegation_policy_note(&cfg(DelegationMode::Auto)).unwrap();
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

    #[test]
    fn always_mode_decomposes_every_non_trivial_task() {
        let note = delegation_policy_note(&cfg(DelegationMode::Always)).unwrap();
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
        let note = delegation_policy_note(&c).unwrap();
        assert!(note.contains("At most 5 sub-agents run at once"), "{note}");
        assert!(note.contains("`openai/gpt-5.6-terra`"), "{note}");

        // No override -> no model line at all.
        let note = delegation_policy_note(&cfg(DelegationMode::Auto)).unwrap();
        assert!(!note.contains("configured override"), "{note}");
        // Clamped: 0 -> 1.
        let mut c = cfg(DelegationMode::Always);
        c.max_concurrent = 0;
        assert!(
            delegation_policy_note(&c)
                .unwrap()
                .contains("At most 1 sub-agents")
        );
    }

    /// F9-7b / F9-10: narration per phase, verifying children's claims,
    /// and the `heavy` escape hatch under the `cheaper` policy only.
    #[test]
    fn narration_claim_checks_and_heavy_hint() {
        let note = delegation_policy_note(&cfg(DelegationMode::Auto)).unwrap();
        assert!(note.contains("Narrate as you go"), "{note}");
        assert!(note.contains("before every new phase"), "{note}");
        assert!(note.contains("after each sub-agent completes"), "{note}");
        assert!(note.contains("claim, not a fact"), "{note}");
        assert!(note.contains("`model: \"heavy\"`"), "{note}");
        assert!(note.contains("policy `cheaper`"), "{note}");
        let mut c = cfg(DelegationMode::Auto);
        c.model_policy = crate::config::DelegationModelPolicy::Inherit;
        let note = delegation_policy_note(&c).unwrap();
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
