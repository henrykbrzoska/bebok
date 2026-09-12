//! Tolerant YAML-ish frontmatter parsing for markdown files (agents, skills).
//!
//! A file may start with a frontmatter block delimited by `---` lines:
//!
//! ```text
//! ---
//! name: my-agent
//! description: does things
//! model: zai/glm-4.6
//! tools: ["read_file", "grep"]
//! permissions:
//!   - pattern: "bash(git *)"
//!     action: "allow"
//! ---
//! <body (used as the prompt when no `prompt` key is present)>
//! ```
//!
//! The parser supports scalar values (`key: value`), JSON values (arrays,
//! objects, quoted strings), YAML block lists (`key:` followed by `- item`
//! lines), and literal string blocks (`key: |`). It is deliberately small and
//! tolerant: an unparseable value degrades to a plain string rather than
//! failing the whole file.

use serde_json::{Map, Value};

/// Parse a markdown file into `(frontmatter, body)`.
///
/// Returns `(empty map, full text)` when the file has no `---` frontmatter.
pub fn parse(text: &str) -> (Map<String, Value>, String) {
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = body.lines();

    // First non-empty line must be exactly `---` for there to be frontmatter.
    let Some(first) = lines.next() else {
        return (Map::new(), text.to_string());
    };
    if first.trim_end() != "---" {
        return (Map::new(), text.to_string());
    }

    let mut block = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }
        block.push(line.to_string());
    }

    if !closed {
        // Unterminated frontmatter: treat the whole file as body.
        return (Map::new(), text.to_string());
    }

    let rest: Vec<&str> = lines.collect();
    let body = rest.join("\n");

    (parse_block(&block), body)
}

/// Parse the lines between the `---` fences into a string map.
fn parse_block(lines: &[String]) -> Map<String, Value> {
    let mut map = Map::new();
    // Track a key that is accumulating a block list (`key:` with no value).
    let mut list_key: Option<String> = None;

    for raw in lines {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indented = line.starts_with(' ') || line.starts_with('\t');

        // Skip blank lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // A list item belongs to the currently open `key:`.
        if trimmed.starts_with("- ") || trimmed == "-" {
            if let Some(key) = list_key.as_ref() {
                let item = trimmed[1..].trim();
                let Some(entry) = map.get_mut(key) else {
                    continue;
                };
                let array = match entry {
                    Value::Array(a) => a,
                    other => {
                        *other = Value::Array(Vec::new());
                        match other {
                            Value::Array(a) => a,
                            _ => unreachable!(),
                        }
                    }
                };
                if let Some((k, v)) = split_kv(item) {
                    let mut obj = Map::new();
                    obj.insert(k.trim().to_string(), parse_scalar(v.trim()));
                    array.push(Value::Object(obj));
                } else {
                    array.push(parse_scalar(item));
                }
            }
            continue;
        }

        // An indented line inside a list-of-mappings extends the last item.
        if indented {
            if let Some(key) = list_key.as_ref()
                && let Some((k, v)) = split_kv(trimmed)
                && let Some(Value::Array(arr)) = map.get_mut(key)
                && let Some(Value::Object(obj)) = arr.last_mut()
            {
                obj.insert(k.trim().to_string(), parse_scalar(v.trim()));
            }
            continue;
        }

        // Otherwise a `key: value` line closes any open list.
        list_key = None;

        let Some((key, value)) = split_kv(line) else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();

        if value.is_empty() {
            // `key:` -> open a block list.
            map.insert(key.clone(), Value::Array(Vec::new()));
            list_key = Some(key);
        } else {
            map.insert(key, parse_scalar(value));
        }
    }

    map
}

/// Split `key: value` on the first colon outside quotes.
fn split_kv(line: &str) -> Option<(&str, &str)> {
    let mut in_quote = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => in_quote = !in_quote,
            ':' if !in_quote => return Some((&line[..i], &line[i + 1..])),
            _ => {}
        }
    }
    None
}

/// Parse a scalar/JSON value from a frontmatter value string.
fn parse_scalar(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Null;
    }
    // JSON-ish values: quoted strings, arrays, objects, bools, numbers.
    if (t.starts_with('"')
        || t.starts_with('[')
        || t.starts_with('{')
        || t == "true"
        || t == "false"
        || t == "null"
        || t.parse::<f64>().is_ok())
        && let Ok(v) = serde_json::from_str(t)
    {
        return v;
    }
    Value::String(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_json() {
        let (fm, body) = parse(
            "---\nname: my\ndescription: \"hello world\"\ncount: 3\nflag: true\nnothing: null\ntools: [\"read_file\", \"grep\"]\n---\nThis is the body.\n",
        );
        assert_eq!(fm["name"], "my");
        assert_eq!(fm["description"], "hello world");
        assert_eq!(fm["count"], 3);
        assert_eq!(fm["flag"], true);
        assert_eq!(fm["nothing"], Value::Null);
        assert_eq!(fm["tools"], serde_json::json!(["read_file", "grep"]));
        assert_eq!(body, "This is the body.");
    }

    #[test]
    fn parses_block_list_of_mappings() {
        let (fm, _) = parse(
            "---\nname: x\npermissions:\n  - pattern: \"bash(git *)\"\n    action: \"allow\"\n  - pattern: \"rm *\"\n    action: \"deny\"\n---\n",
        );
        let perms = fm["permissions"].as_array().unwrap();
        assert_eq!(perms.len(), 2);
        assert_eq!(perms[0]["pattern"], "bash(git *)");
        assert_eq!(perms[0]["action"], "allow");
        assert_eq!(perms[1]["action"], "deny");
    }

    #[test]
    fn no_frontmatter_returns_whole_body() {
        let (fm, body) = parse("just a plain text file\nno frontmatter\n");
        assert!(fm.is_empty());
        assert_eq!(body, "just a plain text file\nno frontmatter\n");
    }

    #[test]
    fn unterminated_frontmatter_is_ignored() {
        let (fm, body) = parse("---\nname: x\nnever closed\n");
        assert!(fm.is_empty());
        assert!(body.contains("never closed"));
    }
}
