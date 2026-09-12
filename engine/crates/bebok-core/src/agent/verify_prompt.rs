//! "Verification capabilities" system-prompt section (WP-AUTOVERIFY / F8-1).
//!
//! Agents only use the browser when they know it exists. This section tells
//! the model, on every assembled prompt, that a real browser is one tool
//! call away, that dev servers can be started from `bash`, and — depending
//! on the `verify.frontend` policy — that it is expected to verify frontend
//! changes on its own (`auto`), to ask once (`ask`), or nothing at all
//! (`off`, no section text beyond the capability facts).
//!
//! Kept in its own file so the prompt assemblers (`services/turn.rs`,
//! `agent/task_tool.rs`, `agent/fleet_tool.rs`) each add exactly one line
//! and concurrent work on those assemblers stays conflict-free.

use std::sync::OnceLock;

use crate::config::{FrontendVerify, ResolvedConfig};

use super::Agent;

/// The heading every mode starts with (tests and readers grep for it).
pub const SECTION_HEADING: &str = "Verification capabilities";

/// Whether a Chrome / Edge / Chromium executable is installed on this host.
/// Probed once per process (a handful of `stat` calls) and cached: the
/// answer does not change while the engine runs.
pub fn browser_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| bebok_tools::browser::resolve_executable().is_ok())
}

/// Whether `agent` can call the `browser_*` family at all (empty whitelist
/// = every registered tool).
pub fn agent_has_browser(agent: &Agent) -> bool {
    agent.tools.is_empty() || agent.tools.iter().any(|t| t == "browser_open")
}

/// The section for the resolved config and agent, or `None` when the agent
/// cannot use the browser (read-only presets such as `ask`/`plan`).
pub fn verification_section(cfg: &ResolvedConfig, agent: &Agent) -> Option<String> {
    if !agent_has_browser(agent) {
        return None;
    }
    Some(render(cfg.frontend_verify(), browser_available()))
}

/// Render the section for a policy. `browser_installed = false` keeps the
/// capability facts honest (the tools exist but every `browser_open` would
/// fail) and downgrades the policy to "verify by other means".
pub fn render(mode: FrontendVerify, browser_installed: bool) -> String {
    let mut s = String::new();
    s.push_str(SECTION_HEADING);
    s.push_str(":\n");
    if browser_installed {
        s.push_str(
            "- You have a real browser. `browser_open` starts a Chromium page bound to this \
             session (headless or a visible window per the user's settings); `browser_screenshot` \
             returns an image you can see; `browser_console` lists console messages and uncaught \
             errors since the last navigation; `browser_wait` waits for a selector, text or network \
             idle; `browser_find` lists interactive elements with reliable selectors for \
             `browser_click` / `browser_type`; `browser_get_text` reads the page text; \
             `browser_eval` runs JavaScript. Any http(s) URL works, including localhost dev servers.\n",
        );
    } else {
        s.push_str(
            "- The `browser_*` tools exist but no Chrome/Edge/Chromium is installed on this host, \
             so `browser_open` will fail; verify frontend work through builds, tests and `fetch` \
             instead, and tell the user a browser install would let you check pages visually.\n",
        );
    }
    s.push_str(
        "- Dev servers: start them with `bash` in the background with output redirected to a log \
         file, then poll the log (or `fetch` the URL) until the server prints its ready line. \
         Windows: `start \"dev\" /B cmd /C \"npm run dev > .bebok-dev.log 2>&1\"`; macOS/Linux: \
         `nohup npm run dev > .bebok-dev.log 2>&1 &`. Never run a server in the foreground: the \
         `bash` call would block until its timeout. Before starting one, check whether the \
         project's port already answers (`fetch` http://localhost:<port>/) and that the response \
         really is this project; reuse it only then.\n",
    );
    match mode {
        FrontendVerify::Auto => s.push_str(
            "- Policy (frontend verification: auto) — MANDATORY: whenever you modify frontend/web \
             code (components, templates, styles, routes, client-side state) you must look at the \
             result in the browser before you answer. A passing build, lint or unit test is NOT \
             verification of frontend work; only opening the page is. The user must never have to \
             check your frontend work by hand, and \"it builds\" is not an acceptable final state. \
             Do this, in order, without asking:\n\
             1. Find the dev server: work out the project's serve command and port (package.json \
             scripts, project.json, README), `fetch` that URL and confirm the response is THIS app \
             (title/markup) — another project may be using the port, in which case start your own \
             server on a free port. If nothing answers, start it with `bash` in the background as \
             described above and poll its log until it prints the ready line / URL.\n\
             2. `browser_open` the affected route (e.g. http://localhost:<port>/inventory).\n\
             3. `browser_wait` for the element or text you changed, then `browser_screenshot`.\n\
             4. `browser_console` with level=\"error\"; fix anything your change caused and \
             re-verify (at most 2 more rounds).\n\
             5. Confirm in the screenshot — and with `browser_find` / `browser_get_text` for exact \
             values — that the specific change is visible.\n\
             6. End your final answer with exactly one line: `Verification: opened <url>, saw \
             <what>, console <clean | N errors>` — or `Verification: not possible because <reason>` \
             if the browser or dev server truly could not be used.\n",
        ),
        FrontendVerify::Ask => s.push_str(
            "- Policy (frontend verification: ask): after modifying frontend/web code, ask the user \
             once whether you should verify the change in the browser (start/reuse the dev server, \
             open the route, screenshot, check the console). Only proceed with browser verification \
             after they agree; do it without asking again later in the same task.\n",
        ),
        FrontendVerify::Off => {}
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use serde_json::json;

    fn cfg(mode: &str) -> ResolvedConfig {
        ResolvedConfig::builder()
            .verify(json!({ "frontend": mode }))
            .build()
    }

    #[test]
    fn every_mode_advertises_the_browser_and_dev_servers() {
        for mode in [
            FrontendVerify::Auto,
            FrontendVerify::Ask,
            FrontendVerify::Off,
        ] {
            let text = render(mode, true);
            assert!(text.starts_with(SECTION_HEADING), "{text}");
            assert!(text.contains("`browser_open`"), "{mode:?}: {text}");
            assert!(text.contains("`browser_screenshot`"), "{mode:?}");
            assert!(text.contains("`browser_console`"), "{mode:?}");
            assert!(text.contains("`browser_wait`"), "{mode:?}");
            assert!(text.contains("`browser_find`"), "{mode:?}");
            assert!(text.contains("Dev servers"), "{mode:?}");
            assert!(text.contains("background"), "{mode:?}");
        }
    }

    #[test]
    fn auto_mode_carries_the_full_policy() {
        let text = render(FrontendVerify::Auto, true);
        assert!(text.contains("frontend verification: auto"));
        assert!(text.contains("MANDATORY"));
        assert!(
            text.contains(
                "is NOT \
             verification"
            ) || text.contains("is NOT verification")
        );
        assert!(text.contains("at most 2 more rounds"));
        assert!(text.contains("Verification: opened <url>"));
        assert!(text.contains("without asking"));
        assert!(!text.contains("ask the user once"));
    }

    #[test]
    fn ask_mode_asks_once_and_has_no_auto_policy() {
        let text = render(FrontendVerify::Ask, true);
        assert!(text.contains("frontend verification: ask"));
        assert!(text.contains("ask the user once"));
        assert!(!text.contains("MANDATORY"));
        assert!(!text.contains("Verification: opened"));
    }

    #[test]
    fn off_mode_has_capabilities_but_no_policy() {
        let text = render(FrontendVerify::Off, true);
        assert!(text.contains("`browser_open`"));
        assert!(!text.contains("Policy ("));
        assert!(!text.contains("ask the user once"));
        assert!(!text.contains("MANDATORY"));
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn missing_browser_is_stated_honestly() {
        let text = render(FrontendVerify::Auto, false);
        assert!(text.contains("no Chrome/Edge/Chromium is installed"));
        assert!(text.contains("`browser_open` will fail"));
        // Still tells the agent how to run a dev server (build/test checks).
        assert!(text.contains("Dev servers"));
    }

    #[test]
    fn section_follows_the_resolved_config() {
        let code = Agent::code();
        let auto = verification_section(&cfg("auto"), &code).unwrap();
        let ask = verification_section(&cfg("ask"), &code).unwrap();
        let off = verification_section(&cfg("off"), &code).unwrap();
        assert!(auto.contains("frontend verification: auto"));
        assert!(ask.contains("frontend verification: ask"));
        assert!(!off.contains("Policy ("));
        // Default (no section) is auto.
        let default = verification_section(&ResolvedConfig::default(), &code).unwrap();
        assert!(default.contains("frontend verification: auto"));
    }

    #[test]
    fn read_only_presets_get_no_section() {
        assert!(verification_section(&cfg("auto"), &Agent::ask()).is_none());
        assert!(verification_section(&cfg("auto"), &Agent::plan()).is_none());
        assert!(verification_section(&cfg("auto"), &Agent::code()).is_some());
        assert!(verification_section(&cfg("auto"), &Agent::debug()).is_some());
    }

    #[test]
    fn explicit_browser_whitelist_counts_as_having_the_browser() {
        let mut agent = Agent::ask();
        agent.tools.push("browser_open".to_string());
        assert!(agent_has_browser(&agent));
        assert!(verification_section(&cfg("auto"), &agent).is_some());
    }
}
