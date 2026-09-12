//! The six `browser_*` tools (WP-BROWSER / F6-17).
//!
//! All share one [`BrowserDriver`] (per-session headless page). Every call:
//! * validates its arguments through [`super::args`] (errors are returned as
//!   `error: ...` text, never panics),
//! * is bounded by [`CALL_TIMEOUT`] and cancelled by the turn's abort token,
//! * reports the current URL/title as `structured` JSON so the client's
//!   Browser panel can render state without parsing text.
//!
//! `browser_screenshot` additionally returns the capture through
//! [`ToolOutput::image`], which `bebok-core` turns into an image part the
//! model can actually see.

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
use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Upper bound on one tool call (navigation included).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on waiting for the load event after `browser_open`.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(20);
/// JPEG quality when `format: "jpeg"` is requested.
const JPEG_QUALITY: i64 = 85;

/// Current `{url, title}` of a page, tolerant of transient CDP errors.
async fn page_state(page: &Page) -> (String, String) {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();
    (url, title)
}

fn state_json(url: &str, title: &str) -> Value {
    json!({ "url": url, "title": title })
}

/// Run `fut` under the call timeout and the turn's abort token.
async fn bounded<T, F>(ctx: &ToolCtx, title: &str, fut: F) -> Result<T, ToolOutput>
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

async fn page_for(driver: &Arc<BrowserDriver>, ctx: &ToolCtx) -> Result<Page, String> {
    driver.page(&ctx.session_id).await
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
        "Open a URL in a headless browser bound to this session (one page per session; later browser_* calls act on it). Returns the final URL and page title. Follow with browser_screenshot to see the page or browser_get_text to read it."
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
            let page = page_for(&driver, &ctx).await?;
            page.goto(url.as_str())
                .await
                .map_err(|e| format!("navigation to {url} failed: {e}"))?;
            // Best effort: the load event may already have fired.
            let _ = tokio::time::timeout(NAVIGATION_TIMEOUT, page.wait_for_navigation()).await;
            if wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
            Ok::<_, String>(page_state(&page).await)
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
        "Capture the current page of this session's headless browser as an image you can see (PNG by default). Use it after browser_open / browser_click / browser_type to check what the page looks like."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "full_page": {
                    "type": "boolean",
                    "description": "Capture the whole scrollable page instead of the 1280x800 viewport (default false)."
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
                return Err("no page is open in this session yet; call browser_open first".to_string());
            }
            let page = page_for(&driver, &ctx).await?;
            let mut params = ScreenshotParams::builder().full_page(full_page);
            params = match format {
                ImageFormat::Png => params.format(CaptureScreenshotFormat::Png),
                ImageFormat::Jpeg => params
                    .format(CaptureScreenshotFormat::Jpeg)
                    .quality(JPEG_QUALITY),
            };
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
        "Click an element in this session's headless browser, addressed by a CSS selector or by viewport coordinates (x, y in CSS pixels, as seen in the last screenshot)."
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
                return Err("no page is open in this session yet; call browser_open first".to_string());
            }
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
            Ok((url, page_title)) => ToolOutput::new(
                format!("Clicked {target_desc}\nnow at {url}"),
                title,
            )
            .with_structured(state_json(&url, &page_title)),
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
        "Type text into an input/textarea/contenteditable element of this session's headless browser (focuses it first). Set submit=true to press Enter afterwards."
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
                return Err("no page is open in this session yet; call browser_open first".to_string());
            }
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
        "Return the visible text (innerText) of the current page in this session's headless browser, or of one element when a selector is given. Cheaper than a screenshot when you only need the words."
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
                return Err("no page is open in this session yet; call browser_open first".to_string());
            }
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
        "Evaluate a JavaScript expression in the current page of this session's headless browser and return its JSON-serialised value (promises are awaited). Use it to read DOM state or trigger page code."
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
                return Err("no page is open in this session yet; call browser_open first".to_string());
            }
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
        assert!(out.text.starts_with("error: unsupported URL scheme"), "{}", out.text);
        assert!(out.image.is_none());
        assert!(!d.has_session("test-session").await);

        let out = BrowserClick { driver: d.clone() }.execute(ctx(), json!({})).await;
        assert!(out.text.starts_with("error: missing target"), "{}", out.text);

        let out = BrowserType { driver: d.clone() }
            .execute(ctx(), json!({ "text": "x" }))
            .await;
        assert_eq!(out.text, "error: missing required parameter 'selector'");

        let out = BrowserEval { driver: d.clone() }.execute(ctx(), json!({})).await;
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
