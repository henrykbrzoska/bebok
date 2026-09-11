use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Sort the lines of a file.
///
/// Portable replacement for `sort` / `sort -u` via the `bash` tool. Returns the
/// sorted lines; combine with `uniq` for global deduplication.
pub struct Sort;

#[async_trait]
impl Tool for Sort {
    fn name(&self) -> &str {
        "sort"
    }

    fn description(&self) -> &str {
        "Sort the lines of a file and return them. Portable alternative to `sort` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to sort, relative to the project root."
                },
                "reverse": {
                    "type": "boolean",
                    "description": "Sort descending. Defaults to false."
                },
                "numeric": {
                    "type": "boolean",
                    "description": "Compare leading numeric values instead of text. Defaults to false."
                },
                "unique": {
                    "type": "boolean",
                    "description": "Drop duplicate lines (like `sort -u`). Defaults to false."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether text comparison is case-sensitive. Defaults to true."
                }
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path'", "sort");
        };
        let reverse = args
            .get("reverse")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let numeric = args
            .get("numeric")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let unique = args
            .get("unique")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let full = ctx.root.join(path);
        let text = match tokio::fs::read_to_string(&full).await {
            Ok(t) => t,
            Err(e) => {
                return ToolOutput::new(format!("error: failed to read {path}: {e}"), "sort");
            }
        };

        let mut lines: Vec<&str> = text.lines().collect();
        if numeric {
            lines.sort_by(|a, b| match (leading_number(a), leading_number(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                _ => a.cmp(b),
            });
        } else if case_sensitive {
            lines.sort();
        } else {
            lines.sort_by_key(|l| l.to_lowercase());
        }

        if unique {
            lines.dedup();
        }
        if reverse {
            lines.reverse();
        }

        let mut out = lines.join("\n");
        if out.is_empty() {
            out = "(empty file)".to_string();
        }
        ToolOutput::new(out, format!("sort {path}"))
    }
}

/// Parse the leading numeric value of a line (for `numeric` mode).
fn leading_number(line: &str) -> Option<f64> {
    let trimmed = line.trim_start();
    let end = trimmed
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.' && *c != '-' && *c != '+')
        .map(|(i, _)| i)
        .unwrap_or(trimmed.len());
    trimmed[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::leading_number;

    #[test]
    fn parses_leading_numbers() {
        assert_eq!(leading_number("42 items"), Some(42.0));
        assert_eq!(leading_number("  -3.5"), Some(-3.5));
        assert_eq!(leading_number("no digits"), None);
    }
}
