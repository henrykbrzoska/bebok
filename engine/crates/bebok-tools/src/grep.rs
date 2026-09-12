use std::path::{Path, PathBuf};

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Search files under the project root for lines matching a regex.
pub struct Grep;

const MAX_FILES: usize = 500;
const MAX_MATCHES: usize = 2000;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search files under the project root for lines matching a regular expression."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search, relative to the project root. Defaults to '.'."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether matching is case-sensitive. Defaults to false."
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'pattern'", "grep");
        };
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let re = if case_sensitive {
            Regex::new(pattern)
        } else {
            Regex::new(&format!("(?i){pattern}"))
        };
        let re = match re {
            Ok(r) => r,
            Err(e) => return ToolOutput::new(format!("error: bad pattern: {e}"), "grep"),
        };

        let start = ctx.root.join(path);
        let files = tokio::task::spawn_blocking(move || collect_files(&start)).await;

        let files = match files {
            Ok(f) => f,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "grep"),
        };

        let mut out = String::new();
        let mut matched = 0usize;

        for file in files.into_iter().take(MAX_FILES) {
            if matched >= MAX_MATCHES {
                break;
            }
            let Ok(text) = tokio::fs::read_to_string(&file).await else {
                continue;
            };
            let rel = file
                .strip_prefix(&ctx.root)
                .unwrap_or(&file)
                .to_string_lossy()
                .to_string();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    out.push_str(&format!("{}:{}:{}\n", rel, i + 1, line));
                    matched += 1;
                    if matched >= MAX_MATCHES {
                        break;
                    }
                }
            }
        }

        if out.is_empty() {
            out = "(no matches)".to_string();
        }
        ToolOutput::new(out, format!("grep {pattern}"))
    }
}

/// Recursively collect candidate text files, skipping common noise directories.
fn collect_files(start: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let _ = walk(start, 0, &mut out);
    out
}

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".idea", "dist", "build"];

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if depth > 24 || out.len() >= MAX_FILES {
        return Ok(());
    }
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();
        if path.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let _ = walk(&path, depth + 1, out);
        } else if path.is_file() {
            // Only consider plausible text files (skip binaries by extension heuristic).
            if is_likely_text(&name) {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn is_likely_text(name: &str) -> bool {
    let binary_exts = [
        "png", "jpg", "jpeg", "gif", "ico", "pdf", "zip", "gz", "tar", "exe", "dll", "so", "dylib",
        "class", "jar", "woff", "woff2", "ttf", "eot", "bin", "lock",
    ];
    let Some(ext) = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) else {
        return true;
    };
    !binary_exts.contains(&ext.as_str())
}
