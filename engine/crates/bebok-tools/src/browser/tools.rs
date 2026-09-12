//! The six core `browser_*` tools (WP-BROWSER / F6-17); the verification
//! trio lives in [`super::verify_tools`] (WP-AUTOVERIFY / F8-1) and reuses
//! the `pub(super)` helpers below.
//!
//! All share one [`BrowserDriver`] (per-session page, headless or headed). Every call:
//! * validates its arguments through [`super::args`] (errors are returned as
//!   `error: ...` text, never panics),
//! * is bounded by [`CALL_TIMEOUT`] and cancelled by the turn's abort token,
//! * reports the current URL/title as `structured` JSON so the client's
//!   Browser panel can render state without parsing text.
//!
//! `browser_screenshot` additionally returns the capture through
//! [`ToolOutput::image`], which `bebok-core` turns into an image part the
//! model can actually see.
//!
//! Every call holds a [`BrowserDriver::activity`] guard for its duration so
//! the viewer window's frame stream runs while the page is being driven
//! (WP-BROWSER2 / F7-6).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::layout::Point;
use chromiumoxide::page::{Page, ScreenshotParams};
use serde_json::{Value, json};

use super::args::{
    self, ClickTarget, GetTextArgs, ImageFormat, OpenArgs, ScreenshotArgs, TypeArgs,
};
use super::driver::BrowserDriver;
use super::frames;
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Upper bound on one tool call (navigation included).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on waiting for the load event after `browser_open`.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(20);
/// JPEG quality when `format: "jpeg"` is requested.
const JPEG_QUALITY: i64 = 85;

/// Current `{url, title}` of a page, tolerant of transient CDP errors.
pub(super) async fn page_state(page: &Page) -> (String, String) {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    (url, title)
}

pub(super) fn state_json(url: &str, title: &str) -> Value {
    json!({ "url": url, "title": title })
}

/// Run `fut` under the call timeout and the turn's abort token.
pub(super) async fn bounded<T, F>(ctx: &ToolCtx, title: &str, fut: F) -> Result<T, ToolOutput>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    tokio::select! {
        _ = ctx.abort.cancelled() => Err(ToolOutput::new("aborted", title)),
        r = tokio::time::timeout(CALL_TIMEOUT, fut) => match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ToolOutput::new(format!("error: {e}"), title)),
            Err(_) => Err(ToolOutput::new(
                format!("error: browser call timed out after {}s", CALL_TIMEOUT.as_secs()),
                title,
            )),
        },
    }
}

pub(super) async fn page_for(driver: &Arc<BrowserDriver>, ctx: &ToolCtx) -> Result<Page, String> {
    driver.page(&ctx.session_id, &ctx.root).await
}

/// Turn a Chrome navigation error (`net::ERR_*`) or a `chrome-error://`
/// landing into a clear, actionable message (WP-AUTOVERIFY / F8-1): a
/// refused localhost connection almost always means "the dev server is not
/// running yet", so say so and say what to do.
pub(super) fn explain_navigation_error(url: &str, raw: &str) -> String {
    let host_hint = |what: &str| {
        let local = url.contains("localhost") || url.contains("127.0.0.1") || url.contains("[::1]");
        if local {
            format!(
                "{what} at {url}: nothing is listening on that port. Start the dev server first \
                 (with `bash` in the background, output redirected to a log file), wait until its \
                 log prints the ready line / the URL, then retry browser_open. If a server is \
                 already running, check which port it printed."
            )
        } else {
            format!(
                "{what} at {url}: the host did not accept the connection. Check the URL, the \
                 port and that the service is up, then retry browser_open."
            )
        }
    };
    if raw.contains("ERR_CONNECTION_REFUSED") {
        return host_hint("connection refused");
    }
    if raw.contains("ERR_CONNECTION_RESET") || raw.contains("ERR_EMPTY_RESPONSE") {
        return host_hint("connection dropped");
    }
    if raw.contains("ERR_NAME_NOT_RESOLVED") {
        return format!(
            "cannot resolve the host of {url} (DNS lookup failed). Check the hostname; for a \
             local dev server use http://localhost:<port>/."
        );
    }
    if raw.contains("ERR_CONNECTION_TIMED_OUT") || raw.contains("ERR_TIMED_OUT") {
        return format!(
            "{url} did not answer in time (connection timed out). The server may still be \
             starting: wait for its ready line, then retry browser_open."
        );
    }
    if raw.contains("ERR_SSL") || raw.contains("ERR_CERT") {
        return format!(
            "TLS error opening {url} ({raw}). Try the http:// address of the dev server."
        );
    }
    if raw.contains("ERR_FILE_NOT_FOUND") {
        return format!("file not found: {url}");
    }
    if raw.contains("ERR_ABORTED") {
        return format!(
            "navigation to {url} was aborted (the page redirected or the request was cancelled); retry once"
        );
    }
    format!("navigation to {url} failed: {raw}")
}

async fn find(page: &Page, selector: &str) -> Result<chromiumoxide::Element, String> {
    page.find_element(selector)
        .await
        .map_err(|e| format!("no element matches selector '{selector}': {e}"))
}

// ── browser_open ────────────────────────────────────────────────────────

pub struct BrowserOpen {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserOpen {
    fn name(&self) -> &str {
        "browser_open"
    }

    fn description(&self) -> &str {
        "Open a URL in the real Chromium browser bound to this session (launched on first use; one page per session, headless or a visible window per the user's settings; later browser_* calls act on the same page). Works for any http(s) URL including local dev servers (http://localhost:<port>/route) — start the server first with bash in the background if nothing listens there; a refused connection is reported as such. Returns the final URL and title. Typical verification flow: browser_open -> browser_wait (selector/text) -> browser_screenshot -> browser_console(level=\"error\") -> browser_find/browser_get_text for exact values."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute URL to open (http, https, file, data or about)."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "Extra settle time after the load event, in milliseconds (default 500, max 30000). Raise it for pages that render client-side."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_open".to_string();
        let OpenArgs { url, wait_ms } = match args::parse_open(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let title = format!("browser_open {url}");
        let driver = self.driver.clone();
        let work = async {
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            page.goto(url.as_str())
                .await
                .map_err(|e| explain_navigation_error(&url, &e.to_string()))?;
            // Best effort: the load event may already have fired.
            let _ = tokio::time::timeout(NAVIGATION_TIMEOUT, page.wait_for_navigation()).await;
            if wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            let (final_url, page_title) = page_state(&page).await;
            // Chrome shows its own error page for some failures instead of
            // failing `Page.navigate`; treat that landing as the error it is.
            if final_url.starts_with("chrome-error://") {
                let code = error_code_on_page(&page).await;
                return Err(explain_navigation_error(&url, &code));
            }
            Ok::<_, String>((final_url, page_title))
        };
        match bounded(&ctx, &title, work).await {
            Ok((final_url, page_title)) => {
                let text = if page_title.is_empty() {
                    format!("Opened {final_url}")
                } else {
                    format!("Opened {final_url}\ntitle: {page_title}")
                };
                ToolOutput::new(text, title).with_structured(state_json(&final_url, &page_title))
            }
            Err(out) => out,
        }
    }
}

/// Read the `net::ERR_*` code Chrome prints on its error page (best effort).
async fn error_code_on_page(page: &Page) -> String {
    page.evaluate("(document.body && document.body.innerText) || ''")
        .await
        .ok()
        .and_then(|r| r.into_value::<String>().ok())
        .and_then(|t| {
            t.split_whitespace()
                .find(|w| w.starts_with("ERR_") || w.starts_with("net::ERR_"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "chrome-error page".to_string())
}

// ── browser_screenshot ──────────────────────────────────────────────────

pub struct BrowserScreenshot {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserScreenshot {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn description(&self) -> &str {
        "Capture the current page of this session's browser as an image you can actually see (PNG by default, viewport 1280x800 headless). This is how you verify frontend work visually: after browser_open (+ browser_wait for client-rendered UI) take a screenshot and check that the specific change is there — colours, badges, numbers, layout. full_page=true for long pages. Pair it with browser_console for errors the eye cannot see."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "full_page": {
                    "type": "boolean",
                    "description": "Capture the whole scrollable page instead of the visible viewport (default false)."
                },
                "format": {
                    "type": "string",
                    "enum": ["png", "jpeg"],
                    "description": "Image format (default png; jpeg is smaller for photo-heavy pages)."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_screenshot".to_string();
        let ScreenshotArgs { full_page, format } = match args::parse_screenshot(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let mut params = ScreenshotParams::builder().full_page(full_page);
            params = match format {
                ImageFormat::Png => params.format(CaptureScreenshotFormat::Png),
                ImageFormat::Jpeg => params
                    .format(CaptureScreenshotFormat::Jpeg)
                    .quality(JPEG_QUALITY),
            };
            // Headed windows on HiDPI screens render at DPR > 1: scale the
            // capture to CSS pixels so click coordinates read off the image
            // are right (WP-BROWSER2).
            let headed = driver.is_headed(&ctx.session_id).await.unwrap_or(false);
            let metrics = frames::page_metrics(&page).await;
            if full_page && headed {
                // chromiumoxide's full-page path installs a device-metrics
                // override, which a visible window never recovers from
                // cleanly; capture beyond the viewport instead (no override).
                let content = page
                    .layout_metrics()
                    .await
                    .map_err(|e| format!("layout metrics failed: {e}"))?
                    .css_content_size;
                params = params
                    .full_page(false)
                    .capture_beyond_viewport(true)
                    .clip(frames::content_clip(content.width, content.height, metrics));
            } else if !full_page && let Some(clip) = frames::css_pixel_clip(metrics) {
                params = params.clip(clip);
            }
            let bytes = page
                .screenshot(params.build())
                .await
                .map_err(|e| format!("screenshot failed: {e}"))?;
            let (url, page_title) = page_state(&page).await;
            Ok::<_, String>((bytes, url, page_title))
        };
        match bounded(&ctx, &title, work).await {
            Ok((bytes, url, page_title)) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let text = format!(
                    "Screenshot of {url} ({} bytes, {}{}). The image is attached to this result.",
                    bytes.len(),
                    format.media_type(),
                    if full_page { ", full page" } else { "" }
                );
                ToolOutput::new(text, title)
                    .with_structured(json!({
                        "url": url,
                        "title": page_title,
                        "media_type": format.media_type(),
                        "bytes": bytes.len(),
                        "full_page": full_page,
                    }))
                    .with_image(format.media_type(), data)
            }
            Err(out) => out,
        }
    }
}

// ── browser_click ───────────────────────────────────────────────────────

pub struct BrowserClick {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserClick {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Click an element in this session's browser, addressed by a CSS selector (get reliable ones from browser_find) or by viewport coordinates (x, y in CSS pixels as seen in the last screenshot). Use it to navigate (tabs, links, menu items) and to exercise the interaction you changed; follow with browser_wait / browser_screenshot to see the result."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector of the element to click (first match)." },
                "x": { "type": "number", "description": "Viewport x coordinate (use together with y instead of selector)." },
                "y": { "type": "number", "description": "Viewport y coordinate." },
                "wait_ms": { "type": "integer", "description": "Settle time after the click in milliseconds (default 500, max 30000)." }
            }
        })
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_click".to_string();
        let target = match args::parse_click(&raw) {
            Ok(t) => t,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let wait_ms = raw
            .get("wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(args::DEFAULT_WAIT_MS)
            .min(args::WAIT_MS_LIMIT);
        let driver = self.driver.clone();
        let target_desc = match &target {
            ClickTarget::Selector(s) => s.clone(),
            ClickTarget::Point { x, y } => format!("({x}, {y})"),
        };
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            match &target {
                ClickTarget::Selector(sel) => {
                    let el = find(&page, sel).await?;
                    el.click()
                        .await
                        .map_err(|e| format!("click on '{sel}' failed: {e}"))?;
                }
                ClickTarget::Point { x, y } => {
                    page.click(Point::new(*x, *y))
                        .await
                        .map_err(|e| format!("click at ({x}, {y}) failed: {e}"))?;
                }
            }
            if wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            Ok::<_, String>(page_state(&page).await)
        };
        match bounded(&ctx, &title, work).await {
            Ok((url, page_title)) => {
                ToolOutput::new(format!("Clicked {target_desc}\nnow at {url}"), title)
                    .with_structured(state_json(&url, &page_title))
            }
            Err(out) => out,
        }
    }
}

// ── browser_type ────────────────────────────────────────────────────────

pub struct BrowserType {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserType {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Type text into an input/textarea/contenteditable element of this session's browser (focuses it first; clear=true empties the field before typing, submit=true presses Enter afterwards). Get the selector from browser_find. Use it to fill search boxes and forms when verifying interactive behaviour."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "CSS selector of the element to type into (first match)." },
                "text": { "type": "string", "description": "Text to type (typed as key strokes, so listeners fire)." },
                "clear": { "type": "boolean", "description": "Clear the field before typing (default false)." },
                "submit": { "type": "boolean", "description": "Press Enter after typing (default false)." }
            },
            "required": ["selector", "text"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_type".to_string();
        let TypeArgs {
            selector,
            text,
            clear,
            submit,
        } = match args::parse_type(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let el = find(&page, &selector).await?;
            el.focus()
                .await
                .map_err(|e| format!("cannot focus '{selector}': {e}"))?;
            if clear {
                el.call_js_fn(
                    "function() { \
                        if ('value' in this) { this.value = ''; } \
                        else if (this.isContentEditable) { this.textContent = ''; } \
                        this.dispatchEvent(new Event('input', { bubbles: true })); \
                    }",
                    false,
                )
                .await
                .map_err(|e| format!("cannot clear '{selector}': {e}"))?;
            }
            el.type_str(&text)
                .await
                .map_err(|e| format!("typing into '{selector}' failed: {e}"))?;
            if submit {
                el.press_key("Enter")
                    .await
                    .map_err(|e| format!("pressing Enter failed: {e}"))?;
                tokio::time::sleep(Duration::from_millis(args::DEFAULT_WAIT_MS)).await;
            }
            Ok::<_, String>(page_state(&page).await)
        };
        match bounded(&ctx, &title, work).await {
            Ok((url, page_title)) => ToolOutput::new(
                format!(
                    "Typed {} characters into {selector}{}\nnow at {url}",
                    text.chars().count(),
                    if submit { " and pressed Enter" } else { "" }
                ),
                title,
            )
            .with_structured(state_json(&url, &page_title)),
            Err(out) => out,
        }
    }
}

// ── browser_get_text ────────────────────────────────────────────────────

pub struct BrowserGetText {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserGetText {
    fn name(&self) -> &str {
        "browser_get_text"
    }

    fn description(&self) -> &str {
        "Return the visible text (innerText) of the current page in this session's browser, or of one element when a selector is given. Cheaper than a screenshot when you only need the words — use it to check exact labels, numbers or percentages your change should display."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string", "description": "Optional CSS selector; defaults to the whole document body." },
                "max_chars": { "type": "integer", "description": "Truncate the text at this many characters (default 20000, max 200000)." }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_get_text".to_string();
        let GetTextArgs {
            selector,
            max_chars,
        } = match args::parse_get_text(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let text = match &selector {
                Some(sel) => find(&page, sel)
                    .await?
                    .inner_text()
                    .await
                    .map_err(|e| format!("reading text of '{sel}' failed: {e}"))?
                    .unwrap_or_default(),
                None => page
                    .evaluate("document.body ? document.body.innerText : ''")
                    .await
                    .map_err(|e| format!("reading page text failed: {e}"))?
                    .into_value::<String>()
                    .unwrap_or_default(),
            };
            Ok::<_, String>((text, page_state(&page).await))
        };
        match bounded(&ctx, &title, work).await {
            Ok((text, (url, page_title))) => {
                let body = if text.trim().is_empty() {
                    "(no visible text)".to_string()
                } else {
                    args::truncate_chars(&text, max_chars)
                };
                ToolOutput::new(format!("{url}\n\n{body}"), title)
                    .with_structured(state_json(&url, &page_title))
            }
            Err(out) => out,
        }
    }
}

// ── browser_eval ────────────────────────────────────────────────────────

pub struct BrowserEval {
    pub driver: Arc<BrowserDriver>,
}

#[async_trait]
impl Tool for BrowserEval {
    fn name(&self) -> &str {
        "browser_eval"
    }

    fn description(&self) -> &str {
        "Evaluate a JavaScript expression in the current page of this session's browser and return its JSON-serialised value (promises are awaited). Use it for checks the other tools cannot express (computed styles such as a badge's background colour, element counts, app state). Prefer browser_find / browser_get_text / browser_console for the common cases; this tool always asks the user for permission."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "js": { "type": "string", "description": "JavaScript expression, e.g. document.title or JSON.stringify([...document.querySelectorAll('a')].map(a => a.href))." }
            },
            "required": ["js"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, raw: Value) -> ToolOutput {
        let title = "browser_eval".to_string();
        let js = match args::parse_eval(&raw) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("error: {e}"), title),
        };
        let driver = self.driver.clone();
        let work = async {
            if !driver.has_session(&ctx.session_id).await {
                return Err(
                    "no page is open in this session yet; call browser_open first".to_string(),
                );
            }
            let _live = driver.activity(&ctx.session_id);
            let page = page_for(&driver, &ctx).await?;
            let params = EvaluateParams::builder()
                .expression(js.clone())
                .await_promise(true)
                .return_by_value(true)
                .build()
                .map_err(|e| format!("invalid evaluate params: {e}"))?;
            let res = page
                .execute(params)
                .await
                .map_err(|e| format!("evaluation failed: {e}"))?;
            let res = &res.result;
            if let Some(ex) = &res.exception_details {
                let detail = ex
                    .exception
                    .as_ref()
                    .and_then(|o| o.description.clone())
                    .unwrap_or_else(|| ex.text.clone());
                return Err(format!("JavaScript threw: {detail}"));
            }
            let value = match &res.result.value {
                Some(v) => v.clone(),
                None => match &res.result.unserializable_value {
                    Some(u) => Value::String(u.inner().to_string()),
                    None => Value::String(
                        res.result
                            .description
                            .clone()
                            .unwrap_or_else(|| format!("[{:?}]", res.result.r#type)),
                    ),
                },
            };
            Ok::<_, String>((value, page_state(&page).await))
        };
        match bounded(&ctx, &title, work).await {
            Ok((value, (url, page_title))) => {
                let rendered = match &value {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                let rendered = args::truncate_chars(&rendered, args::DEFAULT_TEXT_MAX_CHARS);
                ToolOutput::new(rendered, title).with_structured(json!({
                    "url": url,
                    "title": page_title,
                    "value": value,
                }))
            }
            Err(out) => out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tool_ctx;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        tool_ctx(
            std::env::temp_dir(),
            "test-session".to_string(),
            CancellationToken::new(),
        )
    }

    fn driver() -> Arc<BrowserDriver> {
        Arc::new(BrowserDriver::default())
    }

    /// Argument errors never touch the browser: no session is created.
    #[tokio::test]
    async fn invalid_args_are_reported_without_launching() {
        let d = driver();
        let out = BrowserOpen { driver: d.clone() }
            .execute(ctx(), json!({ "url": "javascript:alert(1)" }))
            .await;
        assert!(
            out.text.starts_with("error: unsupported URL scheme"),
            "{}",
            out.text
        );
        assert!(out.image.is_none());
        assert!(!d.has_session("test-session").await);

        let out = BrowserClick { driver: d.clone() }
            .execute(ctx(), json!({}))
            .await;
        assert!(
            out.text.starts_with("error: missing target"),
            "{}",
            out.text
        );

        let out = BrowserType { driver: d.clone() }
            .execute(ctx(), json!({ "text": "x" }))
            .await;
        assert_eq!(out.text, "error: missing required parameter 'selector'");

        let out = BrowserEval { driver: d.clone() }
            .execute(ctx(), json!({}))
            .await;
        assert_eq!(out.text, "error: missing required parameter 'js'");
        assert!(!d.has_session("test-session").await);
    }

    /// Everything except `browser_open` refuses to launch a browser on its
    /// own: without a page the call is an error, not a silent blank tab.
    #[tokio::test]
    async fn tools_other_than_open_require_an_open_page() {
        let d = driver();
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(BrowserScreenshot { driver: d.clone() }),
            Arc::new(BrowserClick { driver: d.clone() }),
            Arc::new(BrowserType { driver: d.clone() }),
            Arc::new(BrowserGetText { driver: d.clone() }),
            Arc::new(BrowserEval { driver: d.clone() }),
        ];
        let inputs = [
            json!({}),
            json!({ "selector": "a" }),
            json!({ "selector": "input", "text": "hi" }),
            json!({}),
            json!({ "js": "1" }),
        ];
        for (tool, input) in tools.iter().zip(inputs) {
            let out = tool.execute(ctx(), input).await;
            assert!(
                out.text.contains("call browser_open first"),
                "{}: {}",
                tool.name(),
                out.text
            );
        }
        assert!(!d.has_session("test-session").await);
    }

    #[test]
    fn read_only_classification() {
        let d = driver();
        assert!(BrowserScreenshot { driver: d.clone() }.is_read_only());
        assert!(BrowserGetText { driver: d.clone() }.is_read_only());
        assert!(!BrowserOpen { driver: d.clone() }.is_read_only());
        assert!(!BrowserClick { driver: d.clone() }.is_read_only());
        assert!(!BrowserType { driver: d.clone() }.is_read_only());
        assert!(!BrowserEval { driver: d }.is_read_only());
    }

    /// WP-AUTOVERIFY (F8-1): a refused localhost connection tells the model
    /// to start the dev server instead of surfacing a raw `net::ERR_*`.
    #[test]
    fn connection_refused_suggests_starting_the_dev_server() {
        let msg = explain_navigation_error(
            "http://localhost:4200/inventory",
            "net::ERR_CONNECTION_REFUSED",
        );
        assert!(
            msg.starts_with("connection refused at http://localhost:4200/inventory"),
            "{msg}"
        );
        assert!(msg.contains("Start the dev server first"), "{msg}");
        assert!(msg.contains("retry browser_open"), "{msg}");
        assert!(!msg.contains("net::"), "{msg}");

        let msg = explain_navigation_error("https://example.org/", "net::ERR_CONNECTION_REFUSED");
        assert!(msg.contains("did not accept the connection"), "{msg}");
        assert!(!msg.contains("dev server"), "{msg}");

        let msg = explain_navigation_error("http://nope.invalid/", "net::ERR_NAME_NOT_RESOLVED");
        assert!(msg.contains("cannot resolve the host"), "{msg}");
        let msg = explain_navigation_error("http://localhost:1/", "net::ERR_CONNECTION_TIMED_OUT");
        assert!(msg.contains("still be starting"), "{msg}");
        // Unknown codes keep the raw text so nothing is hidden.
        let msg = explain_navigation_error("http://x/", "net::ERR_SOMETHING_ODD");
        assert_eq!(
            msg,
            "navigation to http://x/ failed: net::ERR_SOMETHING_ODD"
        );
    }

    #[test]
    fn descriptions_teach_the_verification_flow() {
        let d = driver();
        let open = BrowserOpen { driver: d.clone() };
        assert!(open.description().contains("localhost"));
        assert!(open.description().contains("browser_wait"));
        assert!(open.description().contains("browser_console"));
        let shot = BrowserScreenshot { driver: d.clone() };
        assert!(shot.description().contains("verify"));
        let eval = BrowserEval { driver: d };
        assert!(eval.description().contains("always asks"));
    }

    #[test]
    fn schemas_declare_required_params() {
        let d = driver();
        let open = BrowserOpen { driver: d.clone() }.parameters_schema();
        assert_eq!(open["required"], json!(["url"]));
        let ty = BrowserType { driver: d.clone() }.parameters_schema();
        assert_eq!(ty["required"], json!(["selector", "text"]));
        let eval = BrowserEval { driver: d }.parameters_schema();
        assert_eq!(eval["required"], json!(["js"]));
    }
}
