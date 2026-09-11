use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Report disk usage of a file or directory.
///
/// Portable replacement for `du -sh` / `du -h --max-depth=1` via the `bash`
/// tool: for a directory it returns the total plus a per-child breakdown,
/// sorted largest first.
pub struct Du;

#[async_trait]
impl Tool for Du {
    fn name(&self) -> &str {
        "du"
    }

    fn description(&self) -> &str {
        "Report disk usage: for a directory, the total size plus a per-entry breakdown (largest first). Portable alternative to `du` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory to measure, relative to the project root. Defaults to '.'."
                },
                "depth": {
                    "type": "integer",
                    "description": "How many levels of the breakdown to show. Defaults to 1."
                }
            },
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let depth = args
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .clamp(1, 8) as usize;
        let full = ctx.root.join(path);

        if !full.exists() {
            return ToolOutput::new(format!("error: path not found: {path}"), "du");
        }

        let full_clone = full.clone();
        let result = tokio::task::spawn_blocking(move || {
            let total = dir_size(&full_clone);
            let mut rows = Vec::new();
            if full_clone.is_dir() {
                collect_rows(&full_clone, 1, depth, &mut rows);
            }
            (total, rows)
        })
        .await;

        let (total, mut rows) = match result {
            Ok(r) => r,
            Err(e) => return ToolOutput::new(format!("error: {e}"), "du"),
        };

        rows.sort_by(|a, b| b.1.cmp(&a.1));

        let mut out = String::new();
        out.push_str(&format!("{}  {path} (total)\n", human(total)));
        for (rel, size) in rows {
            let rel = rel.strip_prefix(&ctx.root).unwrap_or(&rel);
            out.push_str(&format!("{}  {}\n", human(size), rel.display()));
        }

        ToolOutput::new(out.trim_end().to_string(), format!("du {path}"))
    }
}

/// Recursive size of a path in bytes (files only; directories add their entries).
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| dir_size(&e.path()))
        .fold(0, u64::saturating_add)
}

/// Collect (path, size) rows up to `max_depth` levels below `dir`.
fn collect_rows(
    dir: &Path,
    level: usize,
    max_depth: usize,
    out: &mut Vec<(std::path::PathBuf, u64)>,
) {
    if level > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let size = dir_size(&path);
        out.push((path.clone(), size));
        if path.is_dir() {
            collect_rows(&path, level + 1, max_depth, out);
        }
    }
}

/// Render bytes as a roughly human-readable string (matches `du -h` shape).
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}
