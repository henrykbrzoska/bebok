//! `BrowserPage`: the operations every browser tool needs from a page,
//! whether the page is a local Chromium tab ([`LocalPage`]) or a tab in the
//! user's own browser driven through the Bebok Companion Chrome extension
//! ([`RemotePage`], see [`super::remote`]).
//!
//! Phase 1 of the remote-browser plan
//! (`docs/chrome-extension-remote-browser.md`): this trait plus the local
//! implementation. `RemotePage` (phase 1.2) implements the same trait over
//! HTTP; the driver keeps returning [`LocalPage`] until the local/remote
//! routing lands (phase 3).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::layout::Point;
use chromiumoxide::page::{Page, ScreenshotParams};
use serde_json::Value;

use super::args::{ClickTarget, ImageFormat};
use super::frames;

/// JPEG quality for local screenshots (mirrors `tools::JPEG_QUALITY`).
const JPEG_QUALITY: i64 = 85;

/// One browser page, local or remote.
#[async_trait]
pub trait BrowserPage: Send + Sync {
    /// Navigate to `url` and wait for the load event (best effort).
    async fn goto(&self, url: &str) -> Result<(), String>;
    /// Wait for the next navigation's load event (best effort).
    async fn wait_for_navigation(&self) -> Result<(), String>;
    /// Current URL (empty string when unreadable).
    async fn url(&self) -> String;
    /// Current title (empty string when unreadable).
    async fn title(&self) -> String;
    /// `(url, title)`, tolerant of transient failures.
    async fn state(&self) -> (String, String) {
        (self.url().await, self.title().await)
    }
    /// Evaluate `js` and return its JSON value (`Value::Null` when the
    /// expression yields nothing serialisable).
    async fn evaluate(&self, js: &str) -> Result<Value, String>;
    /// Evaluate with promise-awaiting; `Err` when the script throws
    /// (the `browser_eval` semantics; defaults to [`evaluate`](Self::evaluate)).
    async fn eval_checked(&self, js: &str) -> Result<Value, String> {
        self.evaluate(js).await
    }
    /// Visible text of `selector` (or the whole body when `None`).
    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String>;
    /// Click a selector or viewport coordinates, then settle `wait_ms`.
    async fn click(&self, target: &ClickTarget, wait_ms: u64) -> Result<(), String>;
    /// Focus `selector`, optionally clear, type, optionally submit.
    async fn type_text(
        &self,
        selector: &str,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<usize, String>;
    /// Back / forward / reload.
    async fn history(&self, action: super::driver::HistoryAction) -> Result<(), String>;
    /// Viewport screenshot as raw bytes (`png` or `jpeg`).
    async fn screenshot(&self, full_page: bool, format: ImageFormat) -> Result<Vec<u8>, String>;
    /// `[innerWidth, innerHeight, devicePixelRatio]`, or `None` unreadable.
    async fn metrics(&self) -> Option<(f64, f64, f64)>;
    /// `true` when the page renders in a visible OS window (DPR scaling).
    fn is_headed(&self) -> bool;
}

/// A page of the locally launched Chromium (thin wrapper over `chromiumoxide`).
#[derive(Clone)]
pub struct LocalPage {
    page: Page,
    headed: bool,
}

impl LocalPage {
    pub fn new(page: Page, headed: bool) -> Self {
        Self { page, headed }
    }

    /// The wrapped `chromiumoxide` page (frame streamer, console, history).
    pub fn inner(&self) -> &Page {
        &self.page
    }

    async fn find(&self, selector: &str) -> Result<chromiumoxide::Element, String> {
        self.page
            .find_element(selector)
            .await
            .map_err(|e| format!("no element matches selector '{selector}': {e}"))
    }
}

/// Upper bound on waiting for the load event after navigation.
const NAVIGATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[async_trait]
impl BrowserPage for LocalPage {
    async fn goto(&self, url: &str) -> Result<(), String> {
        self.page
            .goto(url)
            .await
            .map_err(|e| super::tools::explain_navigation_error(url, &e.to_string()))?;
        let _ = tokio::time::timeout(NAVIGATION_TIMEOUT, self.page.wait_for_navigation()).await;
        Ok(())
    }

    async fn wait_for_navigation(&self) -> Result<(), String> {
        let _ = tokio::time::timeout(NAVIGATION_TIMEOUT, self.page.wait_for_navigation()).await;
        Ok(())
    }

    async fn url(&self) -> String {
        self.page.url().await.ok().flatten().unwrap_or_default()
    }

    async fn title(&self) -> String {
        self.page
            .get_title()
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    async fn evaluate(&self, js: &str) -> Result<Value, String> {
        let res = self
            .page
            .evaluate(js)
            .await
            .map_err(|e| format!("evaluation failed: {e}"))?;
        Ok(res.into_value().unwrap_or(Value::Null))
    }

    async fn eval_checked(&self, js: &str) -> Result<Value, String> {
        evaluate_checked(&self.page, js).await
    }

    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String> {
        match selector {
            Some(sel) => Ok(self
                .find(sel)
                .await?
                .inner_text()
                .await
                .map_err(|e| format!("reading text of '{sel}' failed: {e}"))?
                .unwrap_or_default()),
            None => Ok(self
                .page
                .evaluate("document.body ? document.body.innerText : ''")
                .await
                .map_err(|e| format!("reading page text failed: {e}"))?
                .into_value::<String>()
                .unwrap_or_default()),
        }
    }

    async fn click(&self, target: &ClickTarget, wait_ms: u64) -> Result<(), String> {
        match target {
            ClickTarget::Selector(sel) => {
                self.find(sel)
                    .await?
                    .click()
                    .await
                    .map_err(|e| format!("click on '{sel}' failed: {e}"))?;
            }
            ClickTarget::Point { x, y } => {
                self.page
                    .click(Point::new(*x, *y))
                    .await
                    .map_err(|e| format!("click at ({x}, {y}) failed: {e}"))?;
            }
        }
        if wait_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        }
        Ok(())
    }

    async fn type_text(
        &self,
        selector: &str,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<usize, String> {
        let el = self.find(selector).await?;
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
        el.type_str(text)
            .await
            .map_err(|e| format!("typing into '{selector}' failed: {e}"))?;
        if submit {
            el.press_key("Enter")
                .await
                .map_err(|e| format!("pressing Enter failed: {e}"))?;
            tokio::time::sleep(std::time::Duration::from_millis(
                super::args::DEFAULT_WAIT_MS,
            ))
            .await;
        }
        Ok(text.chars().count())
    }

    async fn history(&self, action: super::driver::HistoryAction) -> Result<(), String> {
        super::driver::navigate_page_history(&self.page, action).await
    }

    async fn screenshot(&self, full_page: bool, format: ImageFormat) -> Result<Vec<u8>, String> {
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
        let metrics = frames::page_metrics(&self.page).await;
        if full_page && self.headed {
            // chromiumoxide's full-page path installs a device-metrics
            // override, which a visible window never recovers from
            // cleanly; capture beyond the viewport instead (no override).
            let content = self
                .page
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
        self.page
            .screenshot(params.build())
            .await
            .map_err(|e| format!("screenshot failed: {e}"))
    }

    async fn metrics(&self) -> Option<(f64, f64, f64)> {
        frames::page_metrics(&self.page).await
    }

    fn is_headed(&self) -> bool {
        self.headed
    }
}

/// Evaluate with promise-awaiting and by-value return (the `browser_eval`
/// semantics); `Err` when the script throws.
pub async fn evaluate_checked(page: &Page, js: &str) -> Result<Value, String> {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    let params = EvaluateParams::builder()
        .expression(js.to_string())
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
    Ok(eval_result_value(res))
}

fn eval_result_value(res: &chromiumoxide::cdp::js_protocol::runtime::EvaluateReturns) -> Value {
    match &res.result.value {
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
    }
}

/// Screenshot bytes as base64 (the `browser_screenshot` payload shape).
pub fn encode_image(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Pages handed out by the driver (today always local; remote in phase 3).
pub type AnyPage = Arc<dyn BrowserPage>;

#[async_trait]
impl<T: BrowserPage + ?Sized> BrowserPage for Arc<T> {
    async fn goto(&self, url: &str) -> Result<(), String> {
        (**self).goto(url).await
    }
    async fn wait_for_navigation(&self) -> Result<(), String> {
        (**self).wait_for_navigation().await
    }
    async fn url(&self) -> String {
        (**self).url().await
    }
    async fn title(&self) -> String {
        (**self).title().await
    }
    async fn evaluate(&self, js: &str) -> Result<Value, String> {
        (**self).evaluate(js).await
    }
    async fn eval_checked(&self, js: &str) -> Result<Value, String> {
        (**self).eval_checked(js).await
    }
    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String> {
        (**self).inner_text(selector).await
    }
    async fn click(&self, target: &ClickTarget, wait_ms: u64) -> Result<(), String> {
        (**self).click(target, wait_ms).await
    }
    async fn type_text(
        &self,
        selector: &str,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<usize, String> {
        (**self).type_text(selector, text, clear, submit).await
    }
    async fn history(&self, action: super::driver::HistoryAction) -> Result<(), String> {
        (**self).history(action).await
    }
    async fn screenshot(&self, full_page: bool, format: ImageFormat) -> Result<Vec<u8>, String> {
        (**self).screenshot(full_page, format).await
    }
    async fn metrics(&self) -> Option<(f64, f64, f64)> {
        (**self).metrics().await
    }
    fn is_headed(&self) -> bool {
        (**self).is_headed()
    }
}
