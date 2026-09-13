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
        "- Dev servers: start them with `bash` and `background: true` plus `ready_port` (the \
         port the server will listen on) and/or `ready_text` (its ready line) with \
         `ready_timeout` 120000-180000: the call blocks until the server answers and returns \
         `ready` with the log tail, or tells you the process exited / timed out (with the \
         log) so you can fix it — no polling, no guessing. You get an `id`, the `pid` and a \
         log path under `.bebok/run/` (read it with `tail`); stop the process later with \
         `bash_kill` (id). With `npm exec` / `npm run`, put server flags after `--` (`npm \
         exec nx serve app -- --port 4317`) or use `npx` directly; npm swallows a bare \
         `--port`. Never run a server in the foreground and never use \
         `start /B`, `nohup` or `&` tricks: a foreground `bash` call blocks until its timeout. \
         Before starting one, check whether the project's port already answers (`fetch` \
         http://localhost:<port>/) and that the response really is this project; reuse it only \
         then, otherwise start your own on a free port.\n\
         - A frontend usually depends on more than one server: check for a dev-server proxy \
         (`proxy.conf.json`, `proxy.conf.js`, `vite.config` `server.proxy`, `devServer.proxy`, \
         `apps/<app>/project.json` serve options, an `API_URL`/`apiBase` in `environment*.ts` or \
         `.env`) and start EVERY target the page calls (api, backend, mock server, database \
         container) before you open the page. A page stuck on a spinner / \"loading\" state with \
         its API down is a FAILED verification, not a note for the user.\n",
    );
    match mode {
        FrontendVerify::Auto => s.push_str(AUTO_POLICY),
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

/// The `auto` policy (F8-1, deepened in F9-8): every dependency started,
/// data loaded, the feature exercised, the API checked separately, failures
/// fixed and re-verified, and an explicit acceptance gate on the final
/// answer.
pub const AUTO_POLICY: &str = "- Policy (frontend verification: auto) — MANDATORY: whenever you modify frontend/web \
code (components, templates, styles, routes, client-side state) you must look at the result in \
the browser AND use it before you answer. A passing build, lint or unit test is NOT verification \
of frontend work; opening the route and seeing it load is only the start. The user must never \
have to check your frontend work by hand, and \"it builds\" or \"the page is still loading\" are \
not acceptable final states. Do this, in order, without asking:\n\
1. Map the dependencies: work out the frontend serve command (package.json scripts, \
project.json, README) AND every server the page talks to (dev-server proxy config, API base \
URL, environment files). Start the servers YOURSELF with `bash` `background: true` (api first, \
then the frontend). Frontend: ALWAYS pass an explicit, non-default port (pick one in \
4310-4390 that `fetch` reports as connection-refused first, e.g. `--port 4317`); never open the \
project's default port (4200/5173/3000...) unless the log of a server YOU started in this task \
says it listens there — a port that already answers belongs to another app (a different \
`<title>` in the `fetch` response is proof) and testing against it is a FAIL. API/backend: \
it must run on the port the frontend proxy targets; if that port is held by a foreign process, \
report that as a FAIL instead of testing a stranger's server. Readiness: use `ready_port` / \
`ready_text` with a `ready_timeout` of 120-180 s — a first `nx serve`/webpack/vite build can \
take 60-120 s; if it reports NOT READY, read the log and wait once more before touching \
anything; do not kill and restart a server that is still compiling. If the log shows the port \
is in use or a crash, fix that (other port, missing \
dependency, build error) and start it again.\n\
2. Verify the API on its own: `fetch` (or `bash` curl) the endpoint(s) the feature uses and check \
the JSON (status 200, expected fields, expected number of rows). If the API is wrong, fix it \
before touching the browser.\n\
3. `browser_open` the affected route, then `browser_wait` for the DATA, not the route: the table \
rows / list items / the text you changed (e.g. selector `table tbody tr` or the first row's \
text), with a timeout that gives the API time to answer. A spinner, skeleton or \"loading\" text \
still visible after the wait is a failure: read `browser_console` (level=\"error\"), read the \
server logs, find the cause (wrong proxy target, api not started, CORS, 404 route, exception), \
fix it, and re-verify.\n\
4. INTERACT with what you built: use `browser_find` to locate the new nav link, filter, sort \
control, search box, button or form and exercise each one with `browser_click` / `browser_type` \
(navigate to the page via its nav link, apply a filter and check the row count changes, sort a \
column and check the order, type a search term and check the results). `browser_screenshot` \
AFTER each interaction and `browser_console` (level=\"error\") after each step. Confirm exact \
values with `browser_get_text` / `browser_find` where a screenshot is ambiguous.\n\
5. Anything that does not work is yours to fix: change the code, rebuild if needed, and re-run \
steps 2-4 (at most 2 more rounds). Only report a failure you could not fix after that, saying \
precisely what you tried.\n\
6. Acceptance gate — the task is NOT done while verification found a functional failure. Your \
final answer MUST start with one line `Status: PASS`, `Status: PASS WITH NOTES` or \
`Status: FAIL`. PASS requires that you completed steps 1-5 on the real page: the data loaded, \
every new control was exercised, and a `browser_screenshot` AFTER the interactions shows the \
result. PASS WITH NOTES is PASS plus non-functional remarks only (a warning, a follow-up idea). \
ANY of the following is `Status: FAIL`, whatever else works: no post-interaction screenshot; a \
`browser_wait` that timed out; a page or port you could not reach; a wrong app on the port; a \
step you skipped or could not run; a failing build/test you did not fix. With FAIL, list what \
does not work and what you tried, and do NOT describe the feature as implemented/delivered. \
The answer MUST also contain one line `Verification: opened <url>, did <interactions>, saw \
<what, with counts/values>, console <clean | N errors>` — or `Verification: not possible \
because <reason>` (which is a FAIL) if the browser or a required server truly could not be \
used after you tried to fix it.\n\
7. If sub-agents did the work: a sub-agent's \"done\" is a claim, not a fact. Before integrating \
and before your final answer, re-check each report against reality yourself — run the build / \
tests the child says it ran, `fetch` the endpoint it says it added, open the page it says it \
changed and perform steps 2-6 on the combined result.\n";

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

    /// F9-8: the deepened policy — dependencies, data wait, interaction,
    /// API check, fix loop, acceptance gate, sub-agent claims.
    #[test]
    fn auto_mode_requires_depth_and_an_acceptance_gate() {
        let text = render(FrontendVerify::Auto, true);
        for needle in [
            "proxy",
            "every server",
            "`background: true`",
            "`bash_kill`",
            "non-default port",
            "do not kill and restart",
            "`ready_port`",
            "ANY of the following is `Status: FAIL`",
            "Verify the API on its own",
            "`browser_wait` for the DATA",
            "\"loading\" text",
            "INTERACT",
            "`browser_find`",
            "`browser_click` / `browser_type`",
            "AFTER each interaction",
            "Acceptance gate",
            "`Status: PASS`",
            "`Status: FAIL`",
            "do NOT describe the feature as implemented",
            "claim, not a fact",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
        // The old "just start /B it" recipe is gone.
        assert!(!text.contains("start \"dev\" /B"));
        assert!(!text.contains("nohup npm run dev"));
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
