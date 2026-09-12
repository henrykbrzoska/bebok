//! Argument parsing / validation for the `browser_*` tools.
//!
//! Pure functions so they can be unit-tested without a browser. Every error
//! is a complete, model-facing sentence (the tool output is `error: <msg>`).

use serde_json::Value;

/// URL schemes `browser_open` accepts. `javascript:` and friends are refused
/// (use `browser_eval` for scripting); anything unknown is refused too so a
/// typo is reported instead of silently showing `chrome-error://`.
pub const ALLOWED_SCHEMES: &[&str] = &["http", "https", "file", "data", "about"];

/// Default cap for `browser_get_text` (characters).
pub const DEFAULT_TEXT_MAX_CHARS: usize = 20_000;
/// Hard cap for `browser_get_text`.
pub const TEXT_MAX_CHARS_LIMIT: usize = 200_000;
/// Default post-navigation settle time (ms) for `browser_open`.
pub const DEFAULT_WAIT_MS: u64 = 500;
/// Hard cap on any caller-supplied wait (ms).
pub const WAIT_MS_LIMIT: u64 = 30_000;

/// Validated `browser_open` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenArgs {
    pub url: String,
    pub wait_ms: u64,
}

/// Validated `browser_screenshot` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotArgs {
    pub full_page: bool,
    /// `png` or `jpeg`.
    pub format: ImageFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }
}

/// Where a `browser_click` lands.
#[derive(Debug, Clone, PartialEq)]
pub enum ClickTarget {
    Selector(String),
    Point { x: f64, y: f64 },
}

/// Validated `browser_type` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeArgs {
    pub selector: String,
    pub text: String,
    /// Clear the field before typing.
    pub clear: bool,
    /// Press Enter afterwards.
    pub submit: bool,
}

/// Validated `browser_get_text` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetTextArgs {
    pub selector: Option<String>,
    pub max_chars: usize,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    match str_arg(args, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        Some(_) => Err(format!("parameter '{key}' must not be empty")),
        None => Err(format!("missing required parameter '{key}'")),
    }
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Validate a URL for `browser_open`: absolute, allowed scheme, no control
/// characters. Returns the trimmed URL.
pub fn validate_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("parameter 'url' must not be empty".to_string());
    }
    if url.chars().any(|c| c.is_control()) {
        return Err("parameter 'url' contains control characters".to_string());
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err(format!(
            "'{url}' is not an absolute URL (expected a scheme such as https://)"
        ));
    };
    let scheme_lc = scheme.to_ascii_lowercase();
    if scheme_lc.is_empty()
        || !scheme_lc
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return Err(format!(
            "'{url}' is not an absolute URL (expected a scheme such as https://)"
        ));
    }
    if !ALLOWED_SCHEMES.contains(&scheme_lc.as_str()) {
        return Err(format!(
            "unsupported URL scheme '{scheme}' (allowed: {})",
            ALLOWED_SCHEMES.join(", ")
        ));
    }
    if matches!(scheme_lc.as_str(), "http" | "https") && !rest.starts_with("//") {
        return Err(format!("'{url}' is malformed: expected {scheme}://host"));
    }
    if matches!(scheme_lc.as_str(), "http" | "https") {
        let host = rest.trim_start_matches('/');
        let host = host.split(['/', '?', '#']).next().unwrap_or("");
        if host.is_empty() {
            return Err(format!("'{url}' has no host"));
        }
    }
    Ok(url.to_string())
}

/// A CSS selector must be non-empty and free of control characters. The
/// browser itself reports syntax errors; this only catches the obvious.
pub fn validate_selector(raw: &str) -> Result<String, String> {
    let sel = raw.trim();
    if sel.is_empty() {
        return Err("selector must not be empty".to_string());
    }
    if sel.chars().any(|c| c.is_control()) {
        return Err("selector contains control characters".to_string());
    }
    Ok(sel.to_string())
}

fn wait_ms(args: &Value, default: u64) -> u64 {
    args.get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .min(WAIT_MS_LIMIT)
}

pub fn parse_open(args: &Value) -> Result<OpenArgs, String> {
    let url = validate_url(required_str(args, "url")?)?;
    Ok(OpenArgs {
        url,
        wait_ms: wait_ms(args, DEFAULT_WAIT_MS),
    })
}

pub fn parse_screenshot(args: &Value) -> Result<ScreenshotArgs, String> {
    let format = match str_arg(args, "format").map(|s| s.trim().to_ascii_lowercase()) {
        None => ImageFormat::Png,
        Some(f) if f == "png" => ImageFormat::Png,
        Some(f) if f == "jpeg" || f == "jpg" => ImageFormat::Jpeg,
        Some(other) => {
            return Err(format!(
                "unsupported screenshot format '{other}' (expected png or jpeg)"
            ));
        }
    };
    Ok(ScreenshotArgs {
        full_page: bool_arg(args, "full_page", false),
        format,
    })
}

pub fn parse_click(args: &Value) -> Result<ClickTarget, String> {
    let selector = str_arg(args, "selector").filter(|s| !s.trim().is_empty());
    let x = args.get("x").and_then(Value::as_f64);
    let y = args.get("y").and_then(Value::as_f64);
    match (selector, x, y) {
        (Some(sel), None, None) => Ok(ClickTarget::Selector(validate_selector(sel)?)),
        (None, Some(x), Some(y)) => {
            if x < 0.0 || y < 0.0 || !x.is_finite() || !y.is_finite() {
                return Err("coordinates x and y must be non-negative numbers".to_string());
            }
            Ok(ClickTarget::Point { x, y })
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            Err("pass either 'selector' or 'x'+'y', not both".to_string())
        }
        (None, Some(_), None) | (None, None, Some(_)) => {
            Err("both 'x' and 'y' are required for a coordinate click".to_string())
        }
        (None, None, None) => Err(
            "missing target: pass a CSS 'selector' or viewport coordinates 'x' and 'y'".to_string(),
        ),
    }
}

pub fn parse_type(args: &Value) -> Result<TypeArgs, String> {
    let selector = validate_selector(required_str(args, "selector")?)?;
    let text = match str_arg(args, "text") {
        Some(t) => t.to_string(),
        None => return Err("missing required parameter 'text'".to_string()),
    };
    Ok(TypeArgs {
        selector,
        text,
        clear: bool_arg(args, "clear", false),
        submit: bool_arg(args, "submit", false),
    })
}

pub fn parse_get_text(args: &Value) -> Result<GetTextArgs, String> {
    let selector = match str_arg(args, "selector") {
        Some(s) if !s.trim().is_empty() => Some(validate_selector(s)?),
        _ => None,
    };
    let max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_TEXT_MAX_CHARS)
        .clamp(1, TEXT_MAX_CHARS_LIMIT);
    Ok(GetTextArgs {
        selector,
        max_chars,
    })
}

pub fn parse_eval(args: &Value) -> Result<String, String> {
    let js = required_str(args, "js")?;
    Ok(js.to_string())
}

/// Truncate `text` to `max_chars` characters with a visible marker.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str(&format!(
        "\n[browser: text truncated at {max_chars} of {total} characters]"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_requires_url() {
        let err = parse_open(&json!({})).unwrap_err();
        assert_eq!(err, "missing required parameter 'url'");
        let err = parse_open(&json!({ "url": "  " })).unwrap_err();
        assert_eq!(err, "parameter 'url' must not be empty");
    }

    #[test]
    fn open_accepts_http_https_data_file_about() {
        for u in [
            "https://example.com",
            "http://127.0.0.1:8798/x?y=1",
            "data:text/html,<h1>hi</h1>",
            "file:///tmp/x.html",
            "about:blank",
        ] {
            assert_eq!(validate_url(u).unwrap(), u, "{u}");
        }
    }

    #[test]
    fn open_rejects_bad_urls() {
        assert!(
            validate_url("example.com")
                .unwrap_err()
                .contains("absolute URL")
        );
        assert!(
            validate_url("javascript:alert(1)")
                .unwrap_err()
                .contains("unsupported URL scheme")
        );
        assert!(
            validate_url("ftp://x")
                .unwrap_err()
                .contains("unsupported URL scheme")
        );
        assert!(validate_url("http:/x").unwrap_err().contains("malformed"));
        assert!(
            validate_url("https:///path")
                .unwrap_err()
                .contains("no host")
        );
        assert!(
            validate_url("https://exa\nmple.com")
                .unwrap_err()
                .contains("control")
        );
    }

    #[test]
    fn open_wait_is_defaulted_and_capped() {
        assert_eq!(
            parse_open(&json!({ "url": "https://a.b" }))
                .unwrap()
                .wait_ms,
            DEFAULT_WAIT_MS
        );
        assert_eq!(
            parse_open(&json!({ "url": "https://a.b", "wait_ms": 10_000_000 }))
                .unwrap()
                .wait_ms,
            WAIT_MS_LIMIT
        );
    }

    #[test]
    fn screenshot_defaults_and_format() {
        let a = parse_screenshot(&json!({})).unwrap();
        assert_eq!(a.format, ImageFormat::Png);
        assert!(!a.full_page);
        let b = parse_screenshot(&json!({ "format": "JPG", "full_page": true })).unwrap();
        assert_eq!(b.format, ImageFormat::Jpeg);
        assert!(b.full_page);
        assert!(parse_screenshot(&json!({ "format": "gif" })).is_err());
        assert_eq!(ImageFormat::Jpeg.media_type(), "image/jpeg");
    }

    #[test]
    fn click_takes_selector_or_point_but_not_both() {
        assert_eq!(
            parse_click(&json!({ "selector": " a.btn " })).unwrap(),
            ClickTarget::Selector("a.btn".into())
        );
        assert_eq!(
            parse_click(&json!({ "x": 10, "y": 20.5 })).unwrap(),
            ClickTarget::Point { x: 10.0, y: 20.5 }
        );
        assert!(
            parse_click(&json!({}))
                .unwrap_err()
                .contains("missing target")
        );
        assert!(
            parse_click(&json!({ "x": 1 }))
                .unwrap_err()
                .contains("both 'x' and 'y'")
        );
        assert!(
            parse_click(&json!({ "selector": "a", "x": 1, "y": 2 }))
                .unwrap_err()
                .contains("not both")
        );
        assert!(
            parse_click(&json!({ "x": -1, "y": 2 }))
                .unwrap_err()
                .contains("non-negative")
        );
    }

    #[test]
    fn type_requires_selector_and_text() {
        assert_eq!(
            parse_type(&json!({ "text": "x" })).unwrap_err(),
            "missing required parameter 'selector'"
        );
        assert_eq!(
            parse_type(&json!({ "selector": "input" })).unwrap_err(),
            "missing required parameter 'text'"
        );
        let t = parse_type(&json!({ "selector": "input", "text": "", "submit": true })).unwrap();
        assert_eq!(t.text, "");
        assert!(t.submit);
        assert!(!t.clear);
    }

    #[test]
    fn selector_validation() {
        assert_eq!(validate_selector("  #id ").unwrap(), "#id");
        assert!(validate_selector("").unwrap_err().contains("empty"));
        assert!(validate_selector("a\tb").unwrap_err().contains("control"));
    }

    #[test]
    fn get_text_defaults_and_caps() {
        let a = parse_get_text(&json!({})).unwrap();
        assert_eq!(a.selector, None);
        assert_eq!(a.max_chars, DEFAULT_TEXT_MAX_CHARS);
        let b = parse_get_text(&json!({ "selector": "main", "max_chars": 10_000_000 })).unwrap();
        assert_eq!(b.selector.as_deref(), Some("main"));
        assert_eq!(b.max_chars, TEXT_MAX_CHARS_LIMIT);
        assert_eq!(
            parse_get_text(&json!({ "max_chars": 0 }))
                .unwrap()
                .max_chars,
            1
        );
    }

    #[test]
    fn eval_requires_js() {
        assert_eq!(
            parse_eval(&json!({})).unwrap_err(),
            "missing required parameter 'js'"
        );
        assert_eq!(parse_eval(&json!({ "js": "1+1" })).unwrap(), "1+1");
    }

    #[test]
    fn truncation_marker() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        let t = truncate_chars("abcdef", 3);
        assert!(t.starts_with("abc\n[browser: text truncated at 3 of 6 characters]"));
    }
}
