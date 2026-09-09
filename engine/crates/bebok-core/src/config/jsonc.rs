//! Tolerant JSONC (JSON with comments) handling.
//!
//! - `parse` strips `//` and `/* */` comments and parses with field order
//!   preserved (via `serde_json` `preserve_order`).
//! - `JsoncDocument` wraps the raw text and can patch a single top-level key
//!   while preserving every other comment and bit of formatting.

use serde_json::Value;

/// Parse JSONC text into an ordered `Value`.
pub fn parse(text: &str) -> Result<Value, String> {
    let stripped = strip_comments(text);
    serde_json::from_str(&stripped).map_err(|e| e.to_string())
}

/// Remove `//` and `/* */` comments, respecting string literals.
pub fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut in_str = false;
    let mut escaped = false;

    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            out.push(c as char);
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        match c {
            b'"' => {
                in_str = true;
                out.push(c as char);
                i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push(' ');
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' ');
            }
            _ => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// A JSONC document that keeps its raw source text so comments and formatting
/// survive edits to a single top-level key.
#[derive(Debug, Clone)]
pub struct JsoncDocument {
    raw: String,
    value: Value,
}

impl JsoncDocument {
    pub fn parse(text: &str) -> Result<Self, String> {
        let value = parse(text)?;
        Ok(Self {
            raw: text.to_string(),
            value,
        })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Return the patched raw text with top-level `key` set to `new_value`,
    /// preserving all comments and formatting elsewhere.
    pub fn with_set(&self, key: &str, new_value: &Value) -> String {
        match top_level_value_range(&self.raw, key) {
            Some((start, end)) => {
                let replacement = serde_json::to_string_pretty(new_value)
                    .unwrap_or_else(|_| "null".to_string());
                let mut out = String::with_capacity(self.raw.len() + replacement.len());
                out.push_str(&self.raw[..start]);
                out.push_str(&replacement);
                out.push_str(&self.raw[end..]);
                out
            }
            None => append_key(&self.raw, key, new_value),
        }
    }
}

/// Find the byte range `[start, end)` of the value for a top-level object key.
fn top_level_value_range(raw: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    let mut depth = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' => {
                let (s, end) = parse_string(raw, i);
                if depth == 1 && s == key {
                    let mut j = end;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b':' {
                        let mut k = j + 1;
                        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        let value_end = scan_value_end(raw, k);
                        return Some((k, value_end));
                    }
                }
                i = end;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a JSON string starting at `start` (which must be `"`).
/// Returns `(contents, index after the closing quote)`.
fn parse_string(raw: &str, start: usize) -> (String, usize) {
    let bytes = raw.as_bytes();
    let mut i = start + 1;
    let mut out = String::new();
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if escaped {
            out.push(c as char);
            escaped = false;
        } else if c == b'\\' {
            escaped = true;
        } else if c == b'"' {
            return (out, i + 1);
        } else {
            out.push(c as char);
        }
        i += 1;
    }
    (out, bytes.len())
}

/// Scan a JSON value starting at `start`, returning the exclusive end index.
fn scan_value_end(raw: &str, start: usize) -> usize {
    let bytes = raw.as_bytes();
    if start >= bytes.len() {
        return start;
    }
    match bytes[start] {
        b'"' => {
            let (_, end) = parse_string(raw, start);
            end
        }
        b'{' | b'[' => scan_balanced(raw, start),
        _ => {
            // number, true, false, null: scan until structural delimiter.
            let mut i = start;
            while i < bytes.len() {
                match bytes[i] {
                    b',' | b'}' | b']' | b'\n' | b'\r' | b' ' | b'\t' => break,
                    _ => i += 1,
                }
            }
            i
        }
    }
}

/// Scan a balanced `{...}` or `[...]` value starting at `start`.
fn scan_balanced(raw: &str, start: usize) -> usize {
    let bytes = raw.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' => {
                let (_, end) = parse_string(raw, i);
                i = end;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// Append `key` to a JSON object before its final closing brace.
fn append_key(raw: &str, key: &str, new_value: &Value) -> String {
    let replacement = serde_json::to_string_pretty(new_value).unwrap_or_else(|_| "null".to_string());

    // Find the last top-level `}`.
    let mut last_close = None;
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                if depth == 1 {
                    depth = 0;
                    last_close = Some(i);
                } else {
                    depth = depth.saturating_sub(1);
                }
            }
            _ => {}
        }
        i += 1;
    }

    let trimmed = raw.trim_end();
    match last_close {
        Some(pos) => {
            let mut out = String::with_capacity(raw.len() + replacement.len() + 16);
            out.push_str(&raw[..pos]);
            // Insert a comma if there is content before the brace.
            let before = raw[..pos].trim_end();
            if !before.is_empty() && !before.ends_with(',') && !before.ends_with('{') {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&format!(
                "  {}: {}",
                serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                replacement
            ));
            out.push('\n');
            out.push_str(&raw[pos..]);
            out
        }
        None => {
            if trimmed.is_empty() {
                format!(
                    "{{\n  {}: {}\n}}\n",
                    serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                    replacement
                )
            } else {
                format!("{trimmed}\n")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments() {
        let src = "{\n  // line comment\n  \"a\": 1, /* block */\n  \"b\": \"x//y\"\n}\n";
        let v = parse(src).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "x//y");
    }

    #[test]
    fn preserves_unrelated_comments_on_set() {
        let src = "{\n  // keep me\n  \"a\": 1,\n  \"b\": { \"c\": 2 }\n}\n";
        let doc = JsoncDocument::parse(src).unwrap();
        let patched = doc.with_set("a", &serde_json::json!(42));
        assert!(patched.contains("// keep me"));
        assert!(patched.contains("\"a\": 42"));
        assert!(patched.contains("\"c\": 2"));
        // Still valid JSONC.
        assert!(parse(&patched).is_ok());
    }
}
