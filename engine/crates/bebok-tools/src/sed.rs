use async_trait::async_trait;
use regex::RegexBuilder;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Regex substitution on a file (a small `sed s///` subset).
///
/// Complements `edit_file` (literal replacement) with regex support. Supports
/// `s/pattern/replacement/flags` where the delimiter is any non-alphanumeric
/// character, and the flags `g` (global) and `i` (case-insensitive). Backrefs
/// use `\1`..`\9` (translated to the regex crate's `$1`..`$9`).
pub struct Sed;

#[async_trait]
impl Tool for Sed {
    fn name(&self) -> &str {
        "sed"
    }

    fn description(&self) -> &str {
        "Regex substitution on a file, e.g. expression \"s/foo\\d+/bar/gi\". Flags: g (all), i (ignore case). Writes in place by default; set in_place=false to only print the result. Backrefs use \\1..\\9."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to edit, relative to the project root."
                },
                "expression": {
                    "type": "string",
                    "description": "Substitution expression, e.g. \"s/old/new/g\". Any non-alphanumeric character works as the delimiter."
                },
                "in_place": {
                    "type": "boolean",
                    "description": "Write the result back to the file (default true). Set false to return the transformed text without writing."
                }
            },
            "required": ["path", "expression"]
        })
    }

    fn is_read_only_for(&self, args: &Value) -> bool {
        // With `in_place: false` this is a pure read/transform.
        args.get("in_place").and_then(|v| v.as_bool()) == Some(false)
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "sed");
        };
        let Some(expression) = args.get("expression").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'expression'", "sed");
        };
        let in_place = args
            .get("in_place")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let (pattern, replacement, global, ignore_case) = match parse_substitution(expression) {
            Ok(v) => v,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "sed"),
        };

        let re = match RegexBuilder::new(&pattern).case_insensitive(ignore_case).build() {
            Ok(r) => r,
            Err(e) => return ToolOutput::new(format!("error: invalid regex: {e}"), "sed"),
        };

        let full = ctx.root.join(path.trim());
        let content = match tokio::fs::read_to_string(&full).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::new(format!("error: failed to read {path}: {e}"), "sed"),
        };

        let count = re.find_iter(&content).count();
        let new_content = if global {
            re.replace_all(&content, replacement.as_str()).into_owned()
        } else {
            re.replace(&content, replacement.as_str()).into_owned()
        };
        let title = format!("sed {path}");

        if !in_place {
            return ToolOutput::new(new_content, title);
        }

        if count == 0 {
            return ToolOutput::new(format!("no matches in {path}"), title);
        }

        let tmp = full.with_file_name(format!(
            "{}.tmp-{}",
            full.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        if let Err(e) = tokio::fs::write(&tmp, &new_content).await {
            return ToolOutput::new(format!("error: failed to write temp file: {e}"), title);
        }
        if let Err(e) = tokio::fs::rename(&tmp, &full).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return ToolOutput::new(format!("error: failed to replace {path}: {e}"), title);
        }

        let applied = if global { count } else { 1.min(count) };
        ToolOutput::new(
            format!("edited {path} ({applied} substitution{})", if applied == 1 { "" } else { "s" }),
            title,
        )
    }
}

/// Parse `s<delim>pattern<delim>replacement<delim>flags` into its parts.
/// The replacement is converted to the regex crate's syntax.
pub(crate) fn parse_substitution(expr: &str) -> Result<(String, String, bool, bool), String> {
    let expr = expr.trim();
    let chars: Vec<char> = expr.chars().collect();
    if chars.first() != Some(&'s') {
        return Err("only `s/pattern/replacement/flags` substitutions are supported".to_string());
    }
    let delimiter = *chars
        .get(1)
        .ok_or_else(|| "missing delimiter after `s`".to_string())?;
    if delimiter.is_ascii_alphanumeric() || delimiter == '\\' || delimiter.is_whitespace() {
        return Err("the substitution delimiter must be a non-alphanumeric character".to_string());
    }

    // Split the remainder into [pattern, replacement, flags] on unescaped
    // delimiters. Only two delimiters are required (`s/foo/bar` is valid); a
    // third starts the flags segment.
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for &c in &chars[2..] {
        if escaped {
            // Keep escapes for the regex, but unescape the delimiter itself.
            if c == delimiter {
                current.push(c);
            } else {
                current.push('\\');
                current.push(c);
            }
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == delimiter {
            segments.push(std::mem::take(&mut current));
            continue;
        }
        current.push(c);
    }
    if escaped {
        current.push('\\');
    }
    segments.push(current); // last segment: flags (possibly empty)
    if segments.len() == 2 {
        // `s/foo/bar` (no trailing delimiter) means empty flags.
        segments.push(String::new());
    }
    if segments.len() < 3 {
        return Err(format!(
            "malformed substitution (expected '{delimiter}pattern{delimiter}replacement{delimiter}flags')"
        ));
    }

    let pattern = segments[0].clone();
    if pattern.is_empty() {
        return Err("empty pattern".to_string());
    }
    let replacement = translate_replacement(&segments[1]);
    let flags = &segments[2];
    let global = flags.contains('g');
    let ignore_case = flags.contains('i');
    Ok((pattern, replacement, global, ignore_case))
}

/// Convert `\1` backrefs and C escapes to the regex crate's replacement syntax.
fn translate_replacement(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' => out.push_str("$$"), // literalize `$` in the replacement
            '\\' => match chars.next() {
                Some(d @ '1'..='9') => {
                    out.push('$');
                    out.push(d);
                }
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_substitution() {
        let (p, r, g, i) = parse_substitution("s/foo/bar/").unwrap();
        assert_eq!(p, "foo");
        assert_eq!(r, "bar");
        assert!(!g);
        assert!(!i);
    }

    #[test]
    fn parses_flags_and_escaped_delimiter() {
        let (p, r, g, i) = parse_substitution(r"s/a\/b/c/g").unwrap();
        assert_eq!(p, "a/b");
        assert_eq!(r, "c");
        assert!(g);
        assert!(!i);
        let (_, _, _, i) = parse_substitution("s/x/y/i").unwrap();
        assert!(i);
    }

    #[test]
    fn translates_backrefs() {
        let (p, r, _, _) = parse_substitution(r"s/(\w+)@(\w+)/\2.\1/").unwrap();
        assert_eq!(p, r"(\w+)@(\w+)");
        assert_eq!(r, "$2.$1");
    }

    #[test]
    fn rejects_non_substitution_expression() {
        assert!(parse_substitution("d").is_err());
        assert!(parse_substitution("s/only-pattern").is_err());
    }
}
