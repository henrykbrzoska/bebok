//! `browser_*`: headless-browser automation tools (WP-BROWSER / F6-17).
//!
//! Nine tools — `browser_open`, `browser_screenshot`, `browser_click`,
//! `browser_type`, `browser_get_text`, `browser_eval` plus the verification
//! trio `browser_console`, `browser_wait`, `browser_find` (WP-AUTOVERIFY /
//! F8-1, see [`verify_tools`] and [`console`]) — that drive one headless
//! Chromium page per Bebok session over the Chrome DevTools Protocol (via
//! the `chromiumoxide` crate). The browser executable is an installed
//! Chrome / Edge / Chromium found by [`discovery`]; nothing is downloaded.
//!
//! Permissions: every `browser_*` call is `Ask` by default. The mutating
//! tools are `Ask` because they are not read-only; `browser_screenshot` and
//! `browser_get_text` are read-only but `bebok-core`'s permission engine
//! special-cases the `browser_` prefix (like `fetch`) so they still ask —
//! a page can expose local services. Projects can relax this with an
//! explicit `"browser_*": "allow"` rule. With `verify.frontend = auto`
//! (WP-AUTOVERIFY) `bebok-core` flips the family's *default* to `Allow`
//! (except `browser_eval`) so autonomous verification does not stall.
//!
//! Lifecycle: the browser is launched on the first call, reused across turns
//! of the same session, and closed on turn abort, session deletion, after
//! [`driver::IDLE_TIMEOUT`] of inactivity, or when the engine exits.
//!
//! Display (WP-BROWSER2 / F7-6): the `browser.display` config section picks
//! **headed** (visible window, default on desktop), **viewer** (headless +
//! live `browser.frame` stream for the Bebok viewer window) or **drawer**
//! (headless, thumbnail only). See [`settings`] and [`frames`]. `bebok-core`
//! feeds the section in through [`configure`] and installs the frame sink
//! through [`set_frame_sink`]; the HTTP layer drives user actions through
//! the very same tool objects (`tools_with`) plus [`BrowserDriver::navigate_history`].

pub mod args;
pub mod console;
pub mod discovery;
pub mod driver;
pub mod frames;
pub mod settings;
pub mod tools;
pub mod verify_tools;

use std::sync::Arc;

pub use driver::{
    ActivityGuard, BrowserDriver, BrowserInfo, HistoryAction, close_all, close_session, configure,
    resolve_executable, set_frame_sink,
};
pub use console::{ConsoleEntry, ConsoleLevel, ConsoleSource};
pub use frames::{Frame, FrameSink};
pub use settings::{BrowserDisplay, BrowserSettings};

use crate::tool::Tool;

/// Every tool name in the family (used by the permission engine tests).
pub const TOOL_NAMES: &[&str] = &[
    "browser_open",
    "browser_screenshot",
    "browser_click",
    "browser_type",
    "browser_get_text",
    "browser_eval",
    "browser_console",
    "browser_wait",
    "browser_find",
];

/// The `browser_*` family sharing the process-wide [`BrowserDriver`].
pub fn tools() -> Vec<Arc<dyn Tool>> {
    tools_with(BrowserDriver::global())
}

/// Same as [`tools`] but bound to an explicit driver (tests).
pub fn tools_with(driver: Arc<BrowserDriver>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(tools::BrowserOpen {
            driver: driver.clone(),
        }),
        Arc::new(tools::BrowserScreenshot {
            driver: driver.clone(),
        }),
        Arc::new(tools::BrowserClick {
            driver: driver.clone(),
        }),
        Arc::new(tools::BrowserType {
            driver: driver.clone(),
        }),
        Arc::new(tools::BrowserGetText {
            driver: driver.clone(),
        }),
        Arc::new(tools::BrowserEval {
            driver: driver.clone(),
        }),
        Arc::new(verify_tools::BrowserConsole {
            driver: driver.clone(),
        }),
        Arc::new(verify_tools::BrowserWait {
            driver: driver.clone(),
        }),
        Arc::new(verify_tools::BrowserFind { driver }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolCtx, tool_ctx};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn family_is_complete_and_named_browser_underscore() {
        let names: Vec<String> = tools_with(Arc::new(BrowserDriver::default()))
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        assert!(names.iter().all(|n| n.starts_with("browser_")));
    }

    /// Integration test against a real headless browser.
    ///
    /// SKIPPED (passes without doing anything, printing `SKIP: ...`) when:
    /// * no Chrome / Edge / Chromium executable can be found
    ///   (see `discovery::discover_binary` and `BEBOK_BROWSER`), or
    /// * `BEBOK_SKIP_BROWSER_TESTS` is set.
    ///
    /// On a CI image without a browser this test therefore never exercises
    /// the driver; check the test output for the `SKIP:` line to know whether
    /// it ran. `BEBOK_BROWSER_NO_SANDBOX=1` is needed when the runner is root.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn headless_roundtrip_open_click_type_text_screenshot() {
        if std::env::var_os("BEBOK_SKIP_BROWSER_TESTS").is_some() {
            eprintln!("SKIP: BEBOK_SKIP_BROWSER_TESTS is set");
            return;
        }
        let exe = match resolve_executable() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP: {e}");
                return;
            }
        };
        eprintln!("browser executable: {}", exe.display());

        let driver = Arc::new(BrowserDriver::default());
        let tools = tools_with(driver.clone());
        let by_name = |n: &str| tools.iter().find(|t| t.name() == n).unwrap().clone();
        let session = format!("it-{}", uuid::Uuid::new_v4());
        let ctx = || -> ToolCtx {
            tool_ctx(
                std::env::temp_dir(),
                session.clone(),
                CancellationToken::new(),
            )
        };

        let html = "<!doctype html><title>Bebok fixture</title>\
            <input id=q placeholder=q><button id=b onclick=\"document.getElementById('out').textContent='clicked:'+document.getElementById('q').value\">go</button>\
            <p id=out>idle</p>\
            <script>console.warn('fixture warning'); setTimeout(() => { throw new Error('fixture boom') }, 10);</script>";
        let url = format!("data:text/html,{}", urlencode(html));

        let out = by_name("browser_open")
            .execute(ctx(), json!({ "url": url, "wait_ms": 100 }))
            .await;
        assert!(out.text.starts_with("Opened data:"), "{}", out.text);
        assert_eq!(out.structured.as_ref().unwrap()["title"], "Bebok fixture");

        // WP-AUTOVERIFY (F8-1): the verification trio on the same page.
        let out = by_name("browser_wait")
            .execute(ctx(), json!({ "selector": "#b", "text": "idle", "network_idle": true }))
            .await;
        assert!(out.text.starts_with("Condition met"), "{}", out.text);
        assert_eq!(out.structured.as_ref().unwrap()["met"], true);
        let out = by_name("browser_wait")
            .execute(ctx(), json!({ "text": "never-there", "timeout_ms": 300 }))
            .await;
        assert!(out.text.starts_with("Timed out"), "{}", out.text);
        assert!(out.text.contains("not found"), "{}", out.text);

        let out = by_name("browser_find").execute(ctx(), json!({})).await;
        assert!(out.text.contains("button \"go\" -> #b"), "{}", out.text);
        assert!(out.text.contains("textbox \"q\" -> #q"), "{}", out.text);
        let out = by_name("browser_find")
            .execute(ctx(), json!({ "query": "go" }))
            .await;
        assert!(out.text.starts_with("1 interactive element(s)"), "{}", out.text);

        let out = by_name("browser_console")
            .execute(ctx(), json!({ "level": "warning" }))
            .await;
        assert!(out.text.contains("[warning] fixture warning"), "{}", out.text);
        assert!(out.text.contains("[error] uncaught"), "{}", out.text);
        assert!(out.text.contains("fixture boom"), "{}", out.text);
        assert_eq!(out.structured.as_ref().unwrap()["counts"]["errors"], 1);
        let out = by_name("browser_console")
            .execute(ctx(), json!({ "level": "error" }))
            .await;
        assert!(!out.text.contains("fixture warning"), "{}", out.text);

        let out = by_name("browser_type")
            .execute(ctx(), json!({ "selector": "#q", "text": "hello" }))
            .await;
        assert!(out.text.starts_with("Typed 5 characters"), "{}", out.text);

        let out = by_name("browser_click")
            .execute(ctx(), json!({ "selector": "#b", "wait_ms": 100 }))
            .await;
        assert!(out.text.starts_with("Clicked #b"), "{}", out.text);

        let out = by_name("browser_get_text")
            .execute(ctx(), json!({ "selector": "#out" }))
            .await;
        assert!(out.text.contains("clicked:hello"), "{}", out.text);

        let out = by_name("browser_eval")
            .execute(
                ctx(),
                json!({ "js": "document.getElementById('out').textContent.length" }),
            )
            .await;
        assert_eq!(out.text.trim(), "13", "{}", out.text);

        let out = by_name("browser_eval")
            .execute(
                ctx(),
                json!({ "js": "(() => { throw new Error('boom') })()" }),
            )
            .await;
        assert!(out.text.contains("JavaScript threw"), "{}", out.text);

        // Navigating again clears the console buffer ("since the last navigation").
        let out = by_name("browser_open")
            .execute(ctx(), json!({ "url": "about:blank", "wait_ms": 100 }))
            .await;
        assert!(out.text.starts_with("Opened about:blank"), "{}", out.text);
        let out = by_name("browser_console").execute(ctx(), json!({})).await;
        assert!(out.text.contains("(no console output)"), "{}", out.text);

        // A refused localhost connection is explained, not dumped as net::ERR_*.
        let out = by_name("browser_open")
            .execute(ctx(), json!({ "url": "http://127.0.0.1:1/", "wait_ms": 0 }))
            .await;
        assert!(out.text.contains("Start the dev server first"), "{}", out.text);
        let out = by_name("browser_open")
            .execute(ctx(), json!({ "url": url, "wait_ms": 100 }))
            .await;
        assert!(out.text.starts_with("Opened data:"), "{}", out.text);

        let out = by_name("browser_screenshot")
            .execute(ctx(), json!({}))
            .await;
        let img = out.image.as_ref().expect("screenshot carries an image");
        assert_eq!(img.media_type, "image/png");
        assert!(img.data.len() > 100, "png payload too small");
        assert_eq!(out.structured.as_ref().unwrap()["media_type"], "image/png");

        let out = by_name("browser_click")
            .execute(ctx(), json!({ "selector": "#does-not-exist" }))
            .await;
        assert!(
            out.text.contains("no element matches selector"),
            "{}",
            out.text
        );

        assert!(driver.has_session(&session).await);
        assert!(driver.close(&session).await);
        assert!(!driver.has_session(&session).await);
    }

    fn urlencode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
}
