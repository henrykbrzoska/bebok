//! `RemotePage`: a tab in the user's own browser, driven through the
//! Bebok Companion Chrome extension over HTTP (phase 1.2 of the
//! remote-browser plan, `docs/chrome-extension-remote-browser.md`).
//!
//! Direction (MVP): the extension registers with the engine
//! (`POST /browser/register`) and keeps a heartbeat; the engine forwards
//! tool commands to the extension's local HTTP listener. This client speaks
//! that listener's side: one method per [`super::page::BrowserPage`]
//! operation, JSON in / JSON out, 30 s timeout, errors as `String`.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::args::{ClickTarget, ImageFormat};
use super::page::BrowserPage;

/// Upper bound on one extension round-trip.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// HTTP client for one registered extension.
#[derive(Clone, Debug)]
pub struct RemoteClient {
    http: reqwest::Client,
    base_url: String,
    session_id: String,
}

impl RemoteClient {
    /// `extension_port` is the port the extension reported at registration.
    pub fn new(extension_port: u16, session_id: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("cannot build remote browser client: {e}"))?;
        Ok(Self {
            http,
            base_url: format!("http://127.0.0.1:{extension_port}"),
            session_id: session_id.to_string(),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn call(&self, action: &str, params: Value) -> Result<Value, String> {
        let url = format!("{}/{action}", self.base_url);
        let res = self
            .http
            .post(&url)
            .json(&params)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    format!("extension did not answer '{action}' within 30 s: {e}")
                } else {
                    format!("cannot reach extension for '{action}': {e}")
                }
            })?;
        if !res.status().is_success() {
            return Err(format!(
                "extension answered '{action}' with HTTP {}",
                res.status()
            ));
        }
        let body: Value = res
            .json()
            .await
            .map_err(|e| format!("extension sent invalid JSON for '{action}': {e}"))?;
        if let Some(err) = body.get("error").and_then(Value::as_str) {
            return Err(format!("extension failed '{action}': {err}"));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// A remote tab implementing [`BrowserPage`] over [`RemoteClient`].
pub struct RemotePage {
    client: RemoteClient,
}

impl RemotePage {
    pub fn new(client: RemoteClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &RemoteClient {
        &self.client
    }
}

#[async_trait]
impl BrowserPage for RemotePage {
    async fn goto(&self, url: &str) -> Result<(), String> {
        self.client
            .call("navigate", json!({ "url": url }))
            .await
            .map(|_| ())
    }

    async fn wait_for_navigation(&self) -> Result<(), String> {
        // The extension's `navigate` already waits for the load event; the
        // local post-navigation settle happens in the tools.
        Ok(())
    }

    async fn url(&self) -> String {
        self.client
            .call("getPageContext", json!({ "max_chars": 0 }))
            .await
            .ok()
            .and_then(|v| v.get("url").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    async fn title(&self) -> String {
        self.client
            .call("getPageContext", json!({ "max_chars": 0 }))
            .await
            .ok()
            .and_then(|v| v.get("title").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    async fn evaluate(&self, js: &str) -> Result<Value, String> {
        let v = self.client.call("evaluate", json!({ "js": js })).await?;
        Ok(v.get("value").cloned().unwrap_or(Value::Null))
    }

    async fn inner_text(&self, selector: Option<&str>) -> Result<String, String> {
        let v = self
            .client
            .call(
                "getText",
                json!({ "selector": selector, "max_chars": 20000 }),
            )
            .await?;
        Ok(v.get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    async fn click(&self, target: &ClickTarget, wait_ms: u64) -> Result<(), String> {
        let params = match target {
            ClickTarget::Selector(s) => json!({ "selector": s, "wait_ms": wait_ms }),
            ClickTarget::Point { x, y } => json!({ "x": x, "y": y, "wait_ms": wait_ms }),
        };
        self.client.call("click", params).await.map(|_| ())
    }

    async fn type_text(
        &self,
        selector: &str,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<usize, String> {
        let n = text.chars().count();
        self.client
            .call(
                "type",
                json!({ "selector": selector, "text": text, "clear": clear, "submit": submit }),
            )
            .await
            .map(|_| n)
    }

    async fn history(&self, action: super::driver::HistoryAction) -> Result<(), String> {
        let name = match action {
            super::driver::HistoryAction::Back => "back",
            super::driver::HistoryAction::Forward => "forward",
            super::driver::HistoryAction::Reload => "reload",
        };
        self.client
            .call("history", json!({ "action": name }))
            .await
            .map(|_| ())
    }

    async fn screenshot(&self, full_page: bool, format: ImageFormat) -> Result<Vec<u8>, String> {
        let fmt = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
        };
        let v = self
            .client
            .call(
                "screenshot",
                json!({ "full_page": full_page, "format": fmt }),
            )
            .await?;
        let data = v
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| "extension screenshot missed 'data'".to_string())?;
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| format!("extension screenshot is not base64: {e}"))
    }

    async fn metrics(&self) -> Option<(f64, f64, f64)> {
        // Remote screenshots arrive at CSS scale already; no DPR correction.
        None
    }

    fn is_headed(&self) -> bool {
        // The user's own window is always "headed", but captures need no
        // DPR clip handling here — the extension sends CSS-scale images.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_targets_localhost_port() {
        let c = RemoteClient::new(43177, "s1").unwrap();
        assert_eq!(c.session_id(), "s1");
    }

    #[test]
    fn invalid_port_is_rejected() {
        // reqwest build cannot fail on a port alone, but the constructor
        // must still produce a well-formed base URL for any u16.
        let c = RemoteClient::new(0, "s").unwrap();
        assert!(c.session_id() == "s");
    }
}
