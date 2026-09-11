use std::path::Path as StdPath;

use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Recursive file search with glob, type and depth filters.
///
/// Portable replacement for `find` via the `bash` tool. Gitignore-aware (like
/// `glob`/`grep`), so build output and VCS noise are skipped by default.
pub struct Find;

#[async_trait]
impl Tool for Find {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Recursively list files under `path` (default \".\"). Filter by `name` (glob on the file name or relative path), `type` (file|dir|any) and `max_depth`. Gitignore-aware. Portable alternative to `find` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to search, relative to the project root. Defaults to \".\"."
                },
                "name": {
                    "type": "string",
                    "description": "Glob to match against the file name (e.g. \"*.rs\") or the relative path (e.g. \"src/**/*.ts\"). Optional."
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "dir", "any"],
                    "description": "Restrict to files, directories, or both. Defaults to any."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to descend. Optional."
                },
                "hidden": {
                    "type": "boolean",
                    "description": "Include hidden/dot files. Defaults to false."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 500."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .trim();
        let type_filter = args.get("type").and_then(|v| v.as_str()).unwrap_or("any");
        let hidden = args.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
        let max_depth = args
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(500);

        let name_pattern = match args.get("name").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => match glob::Pattern::new(p.trim()) {
                Ok(pat) => Some((pat, p.contains('/') || p.contains('\\'))),
                Err(e) => {
                    return ToolOutput::new(format!("error: bad name glob: {e}"), "find");
                }
            },
            _ => None,
        };

        let root = ctx.root.join(path);
        let title = format!("find {path}");

        let mut walker = WalkBuilder::new(&root);
        walker
            .hidden(!hidden)
            .follow_links(false)
            .max_depth(max_depth);

        let mut results: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in walker.build().flatten() {
            if results.len() >= limit {
                truncated = true;
                break;
            }
            // Skip the starting directory itself.
            if entry.path() == root {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            match type_filter {
                "file" if is_dir => continue,
                "dir" if !is_dir => continue,
                _ => {}
            }

            let rel = entry
                .path()
                .strip_prefix(&ctx.root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");

            if let Some((pat, match_rel)) = &name_pattern {
                let target: String = if *match_rel {
                    rel.clone()
                } else {
                    StdPath::new(&rel)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| rel.clone())
                };
                if !pat.matches(&target) {
                    continue;
                }
            }

            results.push(rel);
        }

        results.sort();
        if results.is_empty() {
            return ToolOutput::new("(no matches)", title);
        }
        let mut text = results.join("\n");
        if truncated {
            text.push_str(&format!("\n... (limited to {limit} results)"));
        }
        ToolOutput::new(text, title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn finds_by_name_and_type() {
        let base = std::env::temp_dir().join(format!("bebok-find-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(base.join("src"))
            .await
            .unwrap();
        tokio::fs::write(base.join("src/a.rs"), b"x").await.unwrap();
        tokio::fs::write(base.join("src/b.ts"), b"x").await.unwrap();
        let ctx = ToolCtx {
            root: base.clone(),
            session_id: "find-test".to_string(),
            abort: CancellationToken::new(),
        };

        let out = Find
            .execute(ctx.clone(), json!({ "name": "*.rs", "type": "file" }))
            .await;
        assert_eq!(out.text, "src/a.rs");

        let dirs = Find
            .execute(ctx, json!({ "type": "dir", "max_depth": 1 }))
            .await;
        assert!(dirs.text.contains("src"), "{}", dirs.text);
        let _ = std::fs::remove_dir_all(&base);
    }
}
