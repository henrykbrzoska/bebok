//! `fetch`: a small HTTP client exposed as a tool.
//!
//! Why this exists: the `bash` tool runs `cmd /C` on Windows and `sh -c`
//! elsewhere, and neither offers a portable way to do HTTP. Without this tool
//! the model reaches for whatever interpreter happens to be installed
//! (usually `node`, so `*.js` probe scripts kept appearing in the repo) just to
//! call an endpoint. `fetch` makes HTTP a first-class tool: same behaviour on
//! every OS, no scratch files, no `curl`/`jq` dependency.
//!
//! It is intentionally unrestricted (any URL, any method) — the permission
//! engine is the control point: every request defaults to `Ask`, including
//! GET/HEAD, because reading local services can expose secrets. Projects may
//! explicitly allow trusted destinations with rules.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Default cap on the returned body (bytes).
const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Hard cap on `max_bytes` the caller may request.
const MAX_BYTES_LIMIT: usize = 1024 * 1024;
/// Per-request timeout.
const TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP methods that never mutate the remote state.
fn is_read_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
}

/// HTTP request tool.
pub struct Fetch;

impl Fetch {
    fn method(args: &Value) -> String {
        args.get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .trim()
            .to_uppercase()
    }

    fn headers(args: &Value) -> Vec<(String, String)> {
        args.get("headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The request body: `json` (serialized) wins over the raw `body` string.
    fn body(args: &Value) -> Option<(Option<String>, String)> {
        if let Some(v) = args.get("json") {
            let text = serde_json::to_string(v).unwrap_or_default();
            return Some((Some("application/json".to_string()), text));
        }
        args.get("body")
            .and_then(Value::as_str)
            .map(|s| (None, s.to_string()))
    }

    /// Render a response body for the model: pretty-printed when it is JSON,
    /// capped at `max_bytes` with a marker so the model knows more exists.
    fn render_body(bytes: &[u8], content_type: &str, max_bytes: usize) -> String {
        let truncated = bytes.len() > max_bytes;
        let sliced = &bytes[..bytes.len().min(max_bytes)];
        let text = String::from_utf8_lossy(sliced).to_string();

        let mut out = if content_type.to_lowercase().contains("json") {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(text),
                Err(_) => text,
            }
        } else {
            text
        };

        if truncated {
            out.push_str(&format!(
                "\n[fetch: body truncated at {max_bytes} bytes of {}]",
                bytes.len()
            ));
        }
        out
    }
}

#[async_trait]
impl Tool for Fetch {
    fn name(&self) -> &str {
        "fetch"
    }

    fn description(&self) -> &str {
        "Make an HTTP request and return the response body. Works identically on Windows, macOS and Linux — use it instead of writing curl/node/python scripts. JSON responses are pretty-printed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute URL, e.g. http://127.0.0.1:8787/session?directory=E:/bebok"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "HTTP method. Defaults to GET."
                },
                "headers": {
                    "type": "object",
                    "description": "Extra request headers as a string map.",
                    "additionalProperties": { "type": "string" }
                },
                "json": {
                    "description": "Request body sent as JSON (sets Content-Type: application/json). Use instead of `body` when sending structured data."
                },
                "body": {
                    "type": "string",
                    "description": "Raw request body sent as-is."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Cap on the returned body size in bytes. Defaults to 65536, maximum 1048576."
                }
            },
            "required": ["url"]
        })
    }

    /// The tool as a whole can mutate (POST/PUT/PATCH/DELETE), so the
    /// tool-wide answer is `false`; the per-call answer below is what the gate
    /// actually uses.
    fn is_read_only(&self) -> bool {
        false
    }

    fn is_read_only_for(&self, args: &Value) -> bool {
        is_read_method(&Self::method(args))
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolOutput::new("error: missing required parameter 'url'", "fetch");
        };
        let method = Self::method(&args);
        let title = format!("fetch {method} {url}");

        let Ok(parsed) = reqwest::Method::from_bytes(method.as_bytes()) else {
            return ToolOutput::new(format!("error: unsupported HTTP method {method}"), title);
        };
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_BYTES)
            .clamp(1, MAX_BYTES_LIMIT);

        let client = match reqwest::Client::builder().timeout(TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => return ToolOutput::new(format!("error: http client: {e}"), title),
        };
        let mut req = client.request(parsed, url);
        for (k, v) in Self::headers(&args) {
            req = req.header(k, v);
        }
        if let Some((content_type, text)) = Self::body(&args) {
            if let Some(ct) = content_type {
                req = req.header("content-type", ct);
            }
            req = req.body(text);
        }

        // Stream the body so an unbounded response cannot exhaust memory.
        let send = async {
            let mut res = req.send().await.map_err(|e| format!("{e}"))?;
            let status = res.status();
            let content_type = res
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = res.chunk().await.map_err(|e| format!("{e}"))? {
                buf.extend_from_slice(&chunk);
                if buf.len() >= max_bytes {
                    break;
                }
            }
            Ok::<_, String>((status, content_type, buf))
        };

        let result = tokio::select! {
            _ = ctx.abort.cancelled() => return ToolOutput::new("aborted", title),
            r = send => r,
        };

        match result {
            Ok((status, content_type, buf)) => {
                let mut text = format!("HTTP {status}\n");
                if !content_type.is_empty() {
                    text.push_str(&format!("content-type: {content_type}\n"));
                }
                text.push('\n');
                if buf.is_empty() {
                    text.push_str("(empty body)");
                } else {
                    text.push_str(&Self::render_body(&buf, &content_type, max_bytes));
                }
                ToolOutput::new(text, title)
            }
            Err(e) => ToolOutput::new(format!("error: request failed: {e}"), title),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_get_and_head_are_read_only() {
        assert!(Fetch.is_read_only_for(&json!({ "url": "http://x" })));
        assert!(Fetch.is_read_only_for(&json!({ "url": "http://x", "method": "get" })));
        assert!(Fetch.is_read_only_for(&json!({ "url": "http://x", "method": "HEAD" })));
        assert!(!Fetch.is_read_only_for(&json!({ "url": "http://x", "method": "post" })));
        assert!(!Fetch.is_read_only());
    }

    #[test]
    fn json_body_wins_over_raw_body() {
        let (ct, text) = Fetch::body(&json!({ "json": { "a": 1 }, "body": "raw" })).unwrap();
        assert_eq!(ct.as_deref(), Some("application/json"));
        assert_eq!(text, r#"{"a":1}"#);
        assert_eq!(Fetch::body(&json!({ "body": "raw" })).unwrap().1, "raw");
        assert!(Fetch::body(&json!({})).is_none());
    }

    #[test]
    fn json_bodies_are_pretty_printed_and_capped() {
        let out = Fetch::render_body(br#"{"a":1}"#, "application/json; charset=utf-8", 64);
        assert!(out.contains("\"a\": 1"), "{out}");

        let long = vec![b'x'; 200];
        let out = Fetch::render_body(&long, "text/plain", 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("truncated at 10 bytes of 200"));
    }

    #[test]
    fn headers_are_taken_as_strings_only() {
        let hs = Fetch::headers(&json!({ "headers": { "X-A": "1", "X-B": 2 } }));
        assert_eq!(hs, vec![("X-A".to_string(), "1".to_string())]);
    }
}
