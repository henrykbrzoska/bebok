//! `browser_solve_captcha`: hand an anti-bot / CAPTCHA page to the user in a
//! **visible** browser window and wait until they have solved it.
//!
//! Why this exists: `fetch` and bridged MCP tools (a search server, for
//! example) have their own HTTP client and their own cookie jar. When an
//! upstream answers with a challenge page there is nothing a tool can do
//! about it — the human has to click the pictures. Solving it in the agent's
//! own Chromium keeps the resulting cookies in *that* browser, so the agent
//! continues there (`browser_get_text` / `browser_find`) instead of retrying
//! the blocked API in a loop.
//!
//! Behaviour:
//! * forces a visible window for this session (a headless browser already
//!   running is restarted headed — cookies included, see
//!   [`super::driver::BrowserDriver::page_with_display`]),
//! * navigates to the given URL and polls the page until no challenge marker
//!   is left for two consecutive polls (or `done_text` shows up),
//! * returns the page text so the agent can continue from the solved page,
//! * fails with a clear message when the host cannot open a window at all
//!   (`BEBOK_BROWSER_HEADLESS`, no `DISPLAY`) or when the URL never showed a
//!   challenge in the first place.
//!
//! `browser.remote.enabled` needs no special casing: the page already lives
//! in the user's own browser, which they can type into directly.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::args::{self, HandoffArgs};
use super::driver::BrowserDriver;
use super::page::BrowserPage;
use super::settings::BrowserDisplay;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// How often the page is re-checked while waiting for the user.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
/// Consecutive marker-free polls needed to call the challenge solved (the
/// first poll can land before the challenge has rendered).
const CLEAN_POLLS_NEEDED: u32 = 2;
/// A marker-free page with less text than this is a blank/loading page, not
/// a solved one.
const MIN_SOLVED_TEXT_CHARS: usize = 40;
/// How much of the solved page is handed back to the model.
const RESULT_TEXT_MAX_CHARS: usize = 8000;

/// Lower-case substrings that mark an anti-bot / CAPTCHA wall. Kept in sync
/// with the `MARKERS` array inside [`CHALLENGE_PROBE`].
pub const CHALLENGE_MARKERS: &[&str] = &[
    "captcha",
    "recaptcha",
    "hcaptcha",
    "cf-turnstile",
    "challenge-platform",
    "verify you are human",
    "verify you are a human",
    "are you a robot",
    "i'm not a robot",
    "unusual traffic",
    "automated queries",
    "just a moment...",
    "checking your browser",
    "checking if the site connection is secure",
    "enable javascript and cookies to continue",
    "press & hold",
    "/sorry/index",
    "prove you are not a bot",
    "complete the security check",
];

/// The first challenge marker contained in `text` (case-insensitive).
///
/// Also used by `bebok-core` to annotate the output of plain HTTP tools
/// (`fetch`, bridged MCP servers, `browser_open`) so the model points the
/// user at `browser_solve_captcha` instead of retrying a blocked request.
pub fn detect_challenge(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    CHALLENGE_MARKERS
        .iter()
        .copied()
        .find(|marker| lower.contains(marker))
}

/// One look at the page: what markers are present, and its text.
struct Probe {
    url: String,
    title: String,
    markers: Vec<String>,
    text: String,
}

impl Probe {
    fn solved(&self, done_text: Option<&str>) -> bool {
        if !self.markers.is_empty() {
            return false;
        }
        match done_text {
            Some(needle) => self
                .text
                .to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase()),
            None => self.text.trim().chars().count() >= MIN_SOLVED_TEXT_CHARS,
        }
    }
}

/// Looks for challenge widgets in the DOM *and* for challenge wording in the
/// visible text. Returns `{markers, url, title, text}`.
const CHALLENGE_PROBE: &str = r#"(() => {
  const MARKERS = [
    'captcha', 'recaptcha', 'hcaptcha', 'cf-turnstile', 'challenge-platform',
    'verify you are human', 'verify you are a human', 'are you a robot',
    "i'm not a robot", 'unusual traffic', 'automated queries', 'just a moment...',
    'checking your browser', 'checking if the site connection is secure',
    'enable javascript and cookies to continue', 'press & hold', '/sorry/index',
    'prove you are not a bot', 'complete the security check'
  ];
  const SELECTORS = [
    'iframe[src*="recaptcha"]', 'iframe[src*="hcaptcha"]',
    'iframe[src*="challenges.cloudflare.com"]', '.g-recaptcha', '.h-captcha',
    '.cf-turnstile', '#captcha', '#challenge-form', 'img[src*="captcha"]',
    'input[name*="captcha" i]', 'form[action*="sorry"]', 'div[class*="captcha"]',
    'div[id*="captcha"]'
  ];
  const text = ((document.body && document.body.innerText) || '').slice(0, 20000);
  const lower = text.toLowerCase();
  const markers = [];
  const push = (m) => { if (m && markers.indexOf(m) < 0) markers.push(m); };
  for (const m of MARKERS) { if (lower.includes(m)) push(m); }
  for (const s of SELECTORS) { try { if (document.querySelector(s)) push(s); } catch (e) {} }
  return { markers: markers, url: location.href, title: document.title, text: text.slice(0, 8000) };
})()"#;

fn as_text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read the page state through CDP, falling back to plain innerText when the
/// script cannot run (a sandboxed page, a remote extension that declined).
async fn probe(page: &(impl BrowserPage + ?Sized)) -> Result<Probe, String> {
    match page.evaluate(CHALLENGE_PROBE).await {
        Ok(value) if value.is_object() => Ok(Probe {
            url: as_text(&value, "url"),
            title: as_text(&value, "title"),
            markers: value
                .get("markers")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            text: as_text(&value, "text"),
        }),
        _ => {
            let (url, title) = page.state().await;
            let text = page.inner_text(None).await?;
            Ok(Probe {
                markers: detect_challenge(&text)
                    .map(|m| vec![m.to_string()])
                    .unwrap_or_default(),
                url,
                title,
                text,
            })
        }
    }
}

/// Short, quoted excerpt of the page for error messages.
fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    args::truncate_chars(&flat, 300)
}

pub struct BrowserSolveCaptcha {
    pub driver: std::sync::Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserSolveCaptcha {
    fn name(&self) -> &str {
        "browser_solve_captcha"
    }

    fn description(&self) -> &str {
        "Hand a CAPTCHA / anti-bot challenge page to the user in a VISIBLE browser window and wait until they solve it. Use it when `fetch`, an MCP tool (e.g. a search server) or `browser_get_text` returns a challenge page — \"verify you are human\", \"unusual traffic\", Cloudflare \"Just a moment...\", a recaptcha/hcaptcha/turnstile widget — instead of retrying the blocked request in a loop. The call blocks (default 300s, `timeout_secs` up to 900) while the human clicks; it returns the page text once no challenge marker is left (or `done_text` appears) so you can continue from the solved page. The cookies gained by solving stay in this session's browser: continue with `browser_get_text` / `browser_find` / `browser_click` on the current page rather than calling the blocked API again. If the URL turns out not to be a challenge, the call returns immediately with the page text and no waiting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute URL of the page showing the challenge (usually the one you just tried to fetch)."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "How long to wait for the user in seconds (default 300, clamped to 10-900)."
                },
                "done_text": {
                    "type": "string",
                    "description": "Optional text that must appear on the page to count the challenge as solved (e.g. part of the expected result). Without it any marker-free page with real content counts."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_solve_captcha".to_string();
        let HandoffArgs {
            url,
            timeout_secs,
            done_text,
        } = match args::parse_handoff(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let title = format!("browser_solve_captcha {url}");
        let driver = self.driver.clone();
        let settings = driver.settings_for(&ctx.root);

        // The whole point is that the user can type into the page: without a
        // window (or their own browser through the extension) there is
        // nothing to hand over.
        if !settings.remote.enabled
            && let Some(reason) = super::settings::headed_blocked_reason()
        {
            return ToolOutput::new(
                format!(
                    "error: cannot open a visible browser window ({reason}), so the user cannot \
                     solve the challenge. Ask the user to solve it themselves and paste the \
                     result, or run the engine on a machine with a desktop session \
                     (browser.display = \"headed\")."
                ),
                title,
            );
        }

        let started = Instant::now();
        let deadline = started + Duration::from_secs(timeout_secs);
        let work = async {
            let _live = driver.activity(&ctx.session_id);
            let page = driver
                .page_with_display(&ctx.session_id, &ctx.root, Some(BrowserDisplay::Headed))
                .await?;
            if !settings.remote.enabled && driver.is_headed(&ctx.session_id).await != Some(true) {
                return Err(
                    "the browser could not be launched with a visible window; the user has \
                     nothing to interact with"
                        .to_string(),
                );
            }
            page.goto(url.as_str()).await?;
            let _ = page.wait_for_navigation().await;

            let first = probe(&page).await?;
            if first.markers.is_empty() && !first.solved(done_text.as_deref()) {
                // Unknown to us: report it instead of blocking the turn.
                return Ok::<Outcome, String>(Outcome::NoChallenge(first));
            }
            if first.solved(done_text.as_deref()) {
                return Ok(Outcome::Solved(first, 0));
            }

            let mut clean = 0u32;
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let now = probe(&page).await?;
                if now.solved(done_text.as_deref()) {
                    clean += 1;
                    if clean >= CLEAN_POLLS_NEEDED {
                        let waited = started.elapsed().as_secs();
                        return Ok(Outcome::Solved(now, waited));
                    }
                } else {
                    clean = 0;
                }
                if Instant::now() >= deadline {
                    return Ok(Outcome::TimedOut(now));
                }
            }
        };

        let outcome = tokio::select! {
            _ = ctx.abort.cancelled() => return ToolOutput::new("aborted", title),
            r = work => match r {
                Ok(o) => o,
                Err(e) => return ToolOutput::new(format!("error: {e}"), title),
            },
        };

        match outcome {
            Outcome::Solved(page, waited) => {
                let body = args::truncate_chars(&page.text, RESULT_TEXT_MAX_CHARS);
                let text = format!(
                    "Challenge solved on {url} after {waited}s. The cookies from solving live in \
                     this session's browser, so continue on the page as it is now \
                     (browser_get_text / browser_find / browser_click); retrying the blocked \
                     API/fetch with its own cookies will hit the same wall.\n\n{}\ntitle: {}\n\n{}",
                    if page.url.is_empty() {
                        url.as_str()
                    } else {
                        page.url.as_str()
                    },
                    page.title,
                    if body.trim().is_empty() {
                        "(the page has no visible text; take a browser_screenshot)"
                    } else {
                        body.as_str()
                    }
                );
                ToolOutput::new(text, title).with_structured(json!({
                    "url": page.url,
                    "title": page.title,
                    "solved": true,
                    "waited_secs": waited,
                }))
            }
            Outcome::NoChallenge(page) => {
                let body = args::truncate_chars(&page.text, RESULT_TEXT_MAX_CHARS);
                let text = format!(
                    "No CAPTCHA or anti-bot challenge found on {url} ({}), so nothing was handed \
                     over and nothing was waited for. The page is open in this session's browser.\n\n\
                     {}",
                    if page.url.is_empty() {
                        url.as_str()
                    } else {
                        page.url.as_str()
                    },
                    if body.trim().is_empty() {
                        "(the page has no visible text)"
                    } else {
                        body.as_str()
                    }
                );
                ToolOutput::new(text, title).with_structured(json!({
                    "url": page.url,
                    "title": page.title,
                    "solved": false,
                    "challenge": false,
                }))
            }
            Outcome::TimedOut(page) => {
                let markers = if page.markers.is_empty() {
                    "no marker left, but the page still looks like a challenge".to_string()
                } else {
                    format!("still showing: {}", page.markers.join(", "))
                };
                ToolOutput::new(
                    format!(
                        "error: the challenge on {url} was not solved within {timeout_secs}s \
                         ({markers}). Page excerpt: \"{}\". Ask the user whether they finished \
                         (the window is still open — a later browser_solve_captcha call keeps \
                         the same cookies), or continue without that source.",
                        excerpt(&page.text)
                    ),
                    title,
                )
                .with_structured(json!({
                    "url": page.url,
                    "title": page.title,
                    "solved": false,
                    "markers": page.markers,
                }))
            }
        }
    }
}

/// What the wait loop ended with.
enum Outcome {
    /// The challenge disappeared (or was never there) — page is usable.
    Solved(Probe, u64),
    /// The URL showed no challenge at all.
    NoChallenge(Probe),
    /// The user did not finish in time.
    TimedOut(Probe),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tool_ctx;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        tool_ctx(
            std::env::temp_dir(),
            "handoff-test".to_string(),
            CancellationToken::new(),
        )
    }

    #[test]
    fn detects_common_challenge_wording() {
        assert_eq!(
            detect_challenge("Please verify you are human to continue"),
            Some("verify you are human")
        );
        assert_eq!(
            detect_challenge("Our systems detected UNUSUAL TRAFFIC"),
            Some("unusual traffic")
        );
        assert_eq!(
            detect_challenge("Just a moment..."),
            Some("just a moment...")
        );
        assert_eq!(
            detect_challenge("Enable JavaScript and cookies to continue"),
            Some("enable javascript and cookies to continue")
        );
        assert_eq!(detect_challenge("Search results for rust"), None);
    }

    #[test]
    fn probe_script_and_marker_table_agree() {
        // The JS probe duplicates the marker list for the DOM; a drift would
        // make the tool miss challenges the Rust fallback catches.
        for marker in CHALLENGE_MARKERS {
            assert!(
                CHALLENGE_PROBE.contains(marker),
                "CHALLENGE_PROBE is missing marker {marker:?}"
            );
        }
    }

    #[test]
    fn probe_solved_needs_markers_gone_and_content() {
        let mut p = Probe {
            url: "https://example.org/".into(),
            title: String::new(),
            markers: vec!["captcha".into()],
            text: "a long enough page body that would otherwise look solved".into(),
        };
        assert!(!p.solved(None));
        p.markers.clear();
        p.text = "too short".into();
        assert!(!p.solved(None));
        p.text = "a long enough page body with the results the agent wanted".into();
        assert!(p.solved(None));
        // `done_text` overrides the length heuristic.
        p.text = "x".into();
        assert!(!p.solved(Some("result")));
        p.text = "the result is here".into();
        assert!(p.solved(Some("RESULT")));
    }

    #[test]
    fn solved_text_is_empty_marker_list_only_with_content() {
        let p = Probe {
            url: String::new(),
            title: String::new(),
            markers: Vec::new(),
            text: "   ".into(),
        };
        assert!(!p.solved(Some("x")), "whitespace page is not solved");
    }

    #[test]
    fn arguments_are_validated_before_any_browser_call() {
        let tool = BrowserSolveCaptcha {
            driver: std::sync::Arc::new(BrowserDriver::default()),
        };
        let out = futures::executor::block_on(tool.execute(ctx(), json!({})));
        assert_eq!(out.text, "error: missing required parameter 'url'");

        let out = futures::executor::block_on(
            tool.execute(ctx(), json!({ "url": "javascript:alert(1)" })),
        );
        assert!(
            out.text.starts_with("error: unsupported URL scheme"),
            "{}",
            out.text
        );
    }

    /// The environment gate is checked before launching anything: a host
    /// without a display reports that instead of starting a hidden browser.
    #[test]
    fn missing_display_is_reported_not_swallowed() {
        if super::super::settings::headed_blocked_reason().is_none() {
            eprintln!("SKIP: this host can open a visible window");
            return;
        }
        let tool = BrowserSolveCaptcha {
            driver: std::sync::Arc::new(BrowserDriver::default()),
        };
        let out = futures::executor::block_on(
            tool.execute(ctx(), json!({ "url": "https://example.org/" })),
        );
        assert!(
            out.text.contains("cannot open a visible browser window"),
            "{}",
            out.text
        );
        assert!(out.text.contains("Ask the user"), "{}", out.text);
    }

    #[test]
    fn args_clamp_the_timeout() {
        let a = args::parse_handoff(&json!({ "url": "https://example.org/", "timeout_secs": 5 }))
            .unwrap();
        assert_eq!(a.timeout_secs, args::HANDOFF_TIMEOUT_MIN_SECS);
        let a = args::parse_handoff(&json!({
            "url": "https://example.org/",
            "timeout_secs": 100_000
        }))
        .unwrap();
        assert_eq!(a.timeout_secs, args::HANDOFF_TIMEOUT_LIMIT);
        let a = args::parse_handoff(&json!({ "url": "https://example.org/" })).unwrap();
        assert_eq!(a.timeout_secs, args::DEFAULT_HANDOFF_TIMEOUT_SECS);
        assert_eq!(a.done_text, None);
    }

    /// The handoff tool mutates the session's browser (it may restart a
    /// headless browser headed), so it must never be classified read-only -
    /// that would make the permission engine auto-allow it.
    #[test]
    fn handoff_is_not_read_only() {
        let tool = BrowserSolveCaptcha {
            driver: std::sync::Arc::new(BrowserDriver::default()),
        };
        assert!(!tool.is_read_only());
    }

    /// The DOM probe runs in a real browser: a challenge page is recognised,
    /// a results page is not. Skipped when no Chromium is installed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_reads_a_real_page() {
        if std::env::var("BEBOK_SKIP_BROWSER_TESTS").is_ok_and(|v| !v.trim().is_empty()) {
            eprintln!("SKIP: BEBOK_SKIP_BROWSER_TESTS is set");
            return;
        }
        if super::super::resolve_executable().is_err() {
            eprintln!("SKIP: no Chrome/Edge/Chromium executable");
            return;
        }
        let base = std::env::temp_dir().join(format!("bebok-handoff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let wall = base.join("wall.html");
        std::fs::write(
            &wall,
            "<html><body><h1>Please verify you are human</h1>\
             <form id=\"challenge-form\" action=\"/sorry/index\">\
             <input name=\"captcha\" type=\"text\"></form></body></html>",
        )
        .unwrap();
        let ok = base.join("ok.html");
        std::fs::write(
            &ok,
            "<html><body><h1>Search results</h1><p>Rust 1.97 released, see the announcement.</p></body></html>",
        )
        .unwrap();

        let driver = std::sync::Arc::new(BrowserDriver::default());
        // Headless for the test on every host: the probe does not need a window.
        driver.configure(
            &base,
            super::super::BrowserSettings {
                display: super::super::BrowserDisplay::Viewer,
                ..Default::default()
            },
        );
        let session = format!("handoff-{}", uuid::Uuid::new_v4());
        let page = match driver.page(&session, &base).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP: browser unavailable: {e}");
                let _ = std::fs::remove_dir_all(&base);
                return;
            }
        };

        page.goto(&format!("file://{}", wall.display()))
            .await
            .unwrap();
        let seen = probe(&page).await.unwrap();
        assert!(!seen.markers.is_empty(), "no marker on a challenge page");
        assert!(!seen.solved(None), "a challenge page is not solved");

        page.goto(&format!("file://{}", ok.display()))
            .await
            .unwrap();
        let seen = probe(&page).await.unwrap();
        assert!(seen.markers.is_empty(), "{:?}", seen.markers);
        assert!(
            seen.solved(None),
            "results page counts as solved: {}",
            seen.text
        );

        let _ = driver.close(&session).await;
        let _ = std::fs::remove_dir_all(&base);
    }
}
