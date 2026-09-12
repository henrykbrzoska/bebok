//! `browser_*`: headless-browser automation tools (WP-BROWSER / F6-17).
//!
//! Six tools — `browser_open`, `browser_screenshot`, `browser_click`,
//! `browser_type`, `browser_get_text`, `browser_eval` — that drive one
//! headless Chromium page per Bebok session over the Chrome DevTools
//! Protocol (via the `chromiumoxide` crate). The browser executable is an
//! installed Chrome / Edge / Chromium found by [`discovery`]; nothing is
//! downloaded.
//!
//! Permissions: every `browser_*` call is `Ask` by default. The mutating
//! tools are `Ask` because they are not read-only; `browser_screenshot` and
//! `browser_get_text` are read-only but `bebok-core`'s permission engine
//! special-cases the `browser_` prefix (like `fetch`) so they still ask —
//! a page can expose local services. Projects can relax this with an
//! explicit `"browser_*": "allow"` rule.
//!
//! Lifecycle: the browser is launched on the first call, reused across turns
//! of the same session, and closed on turn abort, session deletion, after
//! [`driver::IDLE_TIMEOUT`] of inactivity, or when the engine exits.

pub mod args;
pub mod discovery;
pub mod driver;
pub mod tools;

use std::sync::Arc;

pub use driver::{BrowserDriver, close_all, close_session, resolve_executable};

use crate::tool::Tool;

/// Every tool name in the family (used by the permission engine tests).
pub const TOOL_NAMES: &[&str] = &[
    "browser_open",
    "browser_screenshot",
    "browser_click",
    "browser_type",
    "browser_get_text",
    "browser_eval",
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
        Arc::new(tools::BrowserEval { driver }),
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
            <p id=out>idle</p>";
        let url = format!("data:text/html,{}", urlencode(html));

        let out = by_name("browser_open")
            .execute(ctx(), json!({ "url": url, "wait_ms": 100 }))
            .await;
        assert!(out.text.starts_with("Opened data:"), "{}", out.text);
        assert_eq!(out.structured.as_ref().unwrap()["title"], "Bebok fixture");

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
            .execute(ctx(), json!({ "js": "document.getElementById('out').textContent.length" }))
            .await;
        assert_eq!(out.text.trim(), "13", "{}", out.text);

        let out = by_name("browser_eval")
            .execute(ctx(), json!({ "js": "(() => { throw new Error('boom') })()" }))
            .await;
        assert!(out.text.contains("JavaScript threw"), "{}", out.text);

        let out = by_name("browser_screenshot").execute(ctx(), json!({})).await;
        let img = out.image.as_ref().expect("screenshot carries an image");
        assert_eq!(img.media_type, "image/png");
        assert!(img.data.len() > 100, "png payload too small");
        assert_eq!(out.structured.as_ref().unwrap()["media_type"], "image/png");

        let out = by_name("browser_click")
            .execute(ctx(), json!({ "selector": "#does-not-exist" }))
            .await;
        assert!(out.text.contains("no element matches selector"), "{}", out.text);

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
