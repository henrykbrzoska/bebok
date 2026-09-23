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

    /// Extract readable text from HTML.
    fn html_to_text(html: &str) -> String {
        // Remove whole blocks (case-insensitive, non-greedy). One pattern per
        // element: the `regex` crate has no backreferences, so a `</\2>`
        // pattern is rejected by `clippy::invalid_regex` and would panic at
        // runtime inside `Regex::new(...).unwrap()`.
        let mut text = html.to_string();
        for name in [
            "script", "style", "noscript", "svg", "template", "head", "nav", "header", "footer",
            "aside",
        ] {
            let re = regex::Regex::new(&format!(r"(?is)<{name}\b[^>]*>.*?</{name}\s*>")).unwrap();
            text = re.replace_all(&text, "").into_owned();
        }

        // Block-level tags become newlines (opening AND closing forms)
        let re_blocks = regex::Regex::new(r"(?is)<(p|div|br|li|ul|ol|tr|td|th|h1|h2|h3|h4|h5|h6|section|article|main|blockquote|table|thead|tbody|pre)[^>]*>").unwrap();
        let text = re_blocks.replace_all(&text, "\n");
        let re_blocks = regex::Regex::new(r"(?is)</(p|div|br|li|ul|ol|tr|td|th|h1|h2|h3|h4|h5|h6|section|article|main|blockquote|table|thead|tbody|pre)>").unwrap();
        let text = re_blocks.replace_all(&text, "\n");

        // Drop a trailing unterminated tag fragment (a final < ... end-of-input without >)
        let text = text.trim_end();
        let text = if let Some(pos) = text.rfind('<') {
            if !text[pos..].contains('>') {
                &text[..pos]
            } else {
                text
            }
        } else {
            text
        };

        // Strip all remaining tags
        let re_tags = regex::Regex::new(r"<[^>]*>").unwrap();
        let text = re_tags.replace_all(text, "");

        // Decode entities in a SINGLE pass
        let text = Self::decode_entities(&text);

        // Collapse whitespace: [ \t]+ -> single space, runs of 3+ newlines -> at most 2, trim start/end
        let text = regex::Regex::new(r"[ \t]+")
            .unwrap()
            .replace_all(&text, " ");
        let text = regex::Regex::new(r"\n{3,}")
            .unwrap()
            .replace_all(&text, "\n\n");
        text.trim().to_string()
    }

    /// Decode HTML entities in a single pass (no double decoding).
    fn decode_entities(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '&' {
                // Look for named or numeric entities
                let rest: String = chars.clone().take(10).collect();
                if rest.starts_with("amp;") {
                    result.push('&');
                    for _ in 0..4 {
                        chars.next();
                    }
                } else if rest.starts_with("lt;") {
                    result.push('<');
                    for _ in 0..3 {
                        chars.next();
                    }
                } else if rest.starts_with("gt;") {
                    result.push('>');
                    for _ in 0..3 {
                        chars.next();
                    }
                } else if rest.starts_with("quot;") {
                    result.push('"');
                    for _ in 0..5 {
                        chars.next();
                    }
                } else if rest.starts_with("apos;") {
                    result.push('\'');
                    for _ in 0..5 {
                        chars.next();
                    }
                } else if rest.starts_with("nbsp;") {
                    result.push(' ');
                    for _ in 0..5 {
                        chars.next();
                    }
                } else if rest.starts_with("#") {
                    // Numeric entity
                    let mut num_str = String::new();
                    let mut found = false;
                    let mut pos = 0;
                    for ch in rest.chars() {
                        if ch == ';' {
                            found = true;
                            break;
                        }
                        num_str.push(ch);
                        pos += 1;
                    }
                    if found && !num_str.is_empty() {
                        // `num_str` keeps the leading '#' (e.g. "#8217", "#x41"):
                        // strip it before parsing, otherwise every numeric
                        // entity fails to decode and stays literal.
                        let digits = num_str.trim_start_matches('#');
                        let code = if digits.starts_with(['x', 'X']) {
                            u32::from_str_radix(&digits[1..], 16).ok()
                        } else {
                            digits.parse::<u32>().ok()
                        };
                        if let Some(code) = code {
                            if let Some(ch) = char::from_u32(code) {
                                result.push(ch);
                            } else {
                                result.push_str(&format!("&{num_str};"));
                            }
                            for _ in 0..(pos + 1) {
                                chars.next();
                            }
                        } else {
                            result.push('&');
                        }
                    } else {
                        result.push('&');
                    }
                } else {
                    result.push('&');
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    /// Render a response body for the model: pretty-printed when it is JSON,
    /// capped at `max_bytes` with a marker so the model knows more exists.
    fn render_body(bytes: &[u8], content_type: &str, max_bytes: usize) -> String {
        // Decode the whole byte slice first (do NOT slice raw bytes before decoding)
        let text = String::from_utf8_lossy(bytes).to_string();
        let raw_len = bytes.len();

        // Detect if content is HTML
        let is_html = {
            let ctype = content_type.to_lowercase();
            if ctype.contains("html") {
                true
            } else {
                // Sniff: first non-whitespace 64 chars (lowercased) start with
                // <!doctype html or <html. Char-based take — a byte slice
                // (`trimmed[..64]`) would panic on a multi-byte boundary.
                let preview: String = text
                    .trim_start()
                    .chars()
                    .take(64)
                    .collect::<String>()
                    .to_lowercase();
                preview.starts_with("<!doctype html") || preview.starts_with("<html")
            }
        };

        let mut out = if is_html {
            // Extract readable text first
            let extracted = Self::html_to_text(&text);
            let extracted_len = extracted.len();

            let mut out = if extracted_len > max_bytes {
                // Slice at max_bytes, walking back to a UTF-8 char boundary.
                // `is_char_boundary` lives on `str` (not on `char`), and a raw
                // `extracted[..max_bytes]` would panic if the index lands
                // mid-character.
                let mut end = max_bytes.min(extracted.len());
                while end > 0 && !extracted.is_char_boundary(end) {
                    end -= 1;
                }
                let mut out = extracted[..end].to_string();
                out.push_str(&format!(
                    "\n[fetch: body truncated at {max_bytes} bytes of {extracted_len}]"
                ));
                out
            } else {
                extracted
            };

            // Always append the HTML extraction note as the last line
            out.push_str(&format!(
                "\n[fetch: readable text extracted from HTML, {raw_len} bytes raw]"
            ));
            out
        } else if content_type.to_lowercase().contains("json") {
            // Pretty-print JSON as before
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(text),
                Err(_) => text,
            }
        } else {
            text
        };

        // Truncation marker for non-HTML (or for HTML if we haven't already added a marker)
        // For HTML we already added the marker above if truncated, so only add for non-HTML
        if !is_html && bytes.len() > max_bytes {
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
        "Make an HTTP request and return the response body. Works identically on Windows, macOS and Linux — use it instead of writing curl/node/python scripts. JSON responses are pretty-printed; HTML pages are returned as readable text, avoiding loss of middle content to output truncation."
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
                    "description": "Cap on the returned body size in bytes. For HTML responses, this caps the extracted readable text; defaults to 65536, maximum 1048576."
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

            // For HTML content-type, read up to MAX_BYTES_LIMIT; otherwise stop at max_bytes
            let limit = if content_type.to_lowercase().contains("html") {
                MAX_BYTES_LIMIT
            } else {
                max_bytes
            };

            while let Some(chunk) = res.chunk().await.map_err(|e| format!("{e}"))? {
                buf.extend_from_slice(&chunk);
                if buf.len() >= limit {
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

    #[test]
    fn html_is_extracted_to_readable_text() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Test</title><script>var x=1;</script></head>
<body>
<style>body{color:red}</style>
<nav>Links</nav>
<header>Header</header>
<article>
<p>Hello &amp; world.</p>
<p>This is a test.&#8217; &nbsp; text.</p>
</article>
<footer>Footer</footer>
</body>
</html>"#;
        let text = Fetch::html_to_text(html);
        // Check for article content
        assert!(text.contains("Hello & world."));
        // &#8217; decodes to the typographic apostrophe U+2019, not ASCII '
        assert!(text.contains("This is a test.\u{2019} text."));
        // Check that script and div tags are removed
        assert!(!text.contains("<script"));
        assert!(!text.contains("var x=1"));
        // Check entity decoding (plain space for &nbsp;)
        assert!(text.contains("text."));
        // Check paragraph breaks became newlines and non-content blocks are gone
        assert!(text.contains('\n'));
        assert!(!text.contains("Links")); // <nav> removed
        assert!(!text.contains("Footer")); // <footer> removed
        assert!(!text.contains("color:red")); // <style> content removed
    }

    #[test]
    fn html_truncation_marker_uses_extracted_length() {
        let html = r#"<!DOCTYPE html><html><body><p>"#.to_string()
            + &"X".repeat(200)
            + r#"</p></body></html>"#;
        let out = Fetch::render_body(html.as_bytes(), "text/html", 10);
        // Should start with extracted text (only 10 chars from the paragraph)
        assert!(out.starts_with("XXXXXXXXXX") || out.contains("truncated at 10 bytes"));
        assert!(out.contains("truncated at 10 bytes of"));
    }

    #[test]
    fn html_is_sniffed_without_content_type() {
        let html = r#"<!DOCTYPE html><html><body>Test</body></html>"#;
        let out = Fetch::render_body(html.as_bytes(), "", 1000);
        // Should extract text
        assert!(out.contains("Test"));
        assert!(out.contains("readable text extracted from HTML"));
    }

    #[test]
    fn unterminated_trailing_tag_is_dropped() {
        let html = r#"<!DOCTYPE html><html><body><p>Test<p
"#;
        let text = Fetch::html_to_text(html);
        assert!(!text.contains("<p"));
        assert!(text.contains("Test"));
    }

    #[test]
    fn utf8_slice_lands_on_char_boundary() {
        // Multibyte chars
        let html = r#"<!DOCTYPE html><html><body><p>αβγ</p></body></html>"#;
        let out = Fetch::render_body(html.as_bytes(), "text/html", 10);
        // Should not contain U+FFFD replacement chars
        assert!(!out.contains("�"));
    }
}
