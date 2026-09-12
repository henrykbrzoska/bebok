//! Gitignore-aware filesystem traversal, shared by the `list_dir` / `tree`
//! tools and the HTTP `/fs/*` explorer endpoints.
//!
//! Uses the `ignore` crate so traversal honors `.gitignore` / `.ignore` (with
//! or without a `.git` directory present) while still exposing dotfiles.

use std::path::{Path, PathBuf};

use crate::pathguard::resolve_in_root;

use serde::Serialize;

/// One directory entry surfaced to tools / the explorer API.
#[derive(Debug, Clone, Serialize)]
pub struct FsEntry {
    pub name: String,
    /// Path relative to the project root.
    pub path: String,
    pub is_dir: bool,
}

/// Build a WalkBuilder configured for an explorer: gitignore-aware, dotfiles
/// visible, works without a `.git` directory.
pub fn explorer_walker(root: &Path) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .hidden(false)
        .require_git(false)
        .parents(true)
        .git_global(true);
    builder
}

/// Immediate children of `rel` (or the root when `rel` is empty/`.`).
/// Returns entries relative to `root`.
pub fn list_children(root: &Path, rel: &str) -> Vec<FsEntry> {
    let Ok(base) = normalize_rel(root, rel) else {
        return Vec::new();
    };
    let mut entries: Vec<FsEntry> = Vec::new();

    let walker = explorer_walker(&base).max_depth(Some(1)).build();
    for result in walker {
        let Ok(entry) = result else {
            continue;
        };
        // Depth 0 is `base` itself.
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push(FsEntry {
            name,
            path: rel_path,
            is_dir: path.is_dir(),
        });
    }
    entries.sort_by(|a, b| (b.is_dir, a.name.as_str()).cmp(&(a.is_dir, b.name.as_str())));
    entries
}

/// A recursive tree (up to `max_depth` levels beyond the base) rendered as text.
pub fn tree_text(root: &Path, rel: &str, max_depth: usize) -> String {
    let base = match normalize_rel(root, rel) {
        Ok(base) => base,
        Err(e) => return format!("error: {e}"),
    };
    let mut out = String::new();
    out.push_str(&base.to_string_lossy());
    out.push('\n');

    let walker = explorer_walker(&base).max_depth(Some(max_depth)).build();
    for result in walker {
        let Ok(entry) = result else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let indent = "  ".repeat(entry.depth());
        if entry.path().is_dir() {
            out.push_str(&format!("{indent}{name}/\n"));
        } else {
            out.push_str(&format!("{indent}{name}\n"));
        }
    }
    out
}

/// Resolve `rel` against `root`, guarding against escaping the root.
fn normalize_rel(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() || rel == "." {
        return Ok(root.to_path_buf());
    }
    resolve_in_root(root, rel)
}

/// Validate an explorer path before constructing a successful HTTP response.
pub fn validate_rel(root: &Path, rel: &str) -> Result<(), String> {
    normalize_rel(root, rel).map(|_| ())
}

/// Read a file's contents for the `/fs/file` viewer endpoint, guarding against
/// escaping the root. Returns `Ok(text)` for readable UTF-8 text, an error
/// string otherwise.
pub fn read_file_text(root: &Path, rel: &str) -> Result<String, String> {
    let path = normalize_rel(root, rel)?;
    if !path.is_file() {
        return Err(format!("not a file: {rel}"));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("failed to read {rel}: {e}"))
}

/// Write text content to a file (for the explorer's edit mode), guarding
/// against escaping the root. Creates parent directories as needed.
pub fn write_file_text(root: &Path, rel: &str, content: &str) -> Result<(), String> {
    let path = normalize_rel(root, rel)?;
    if path.is_dir() {
        return Err(format!("not a file: {rel}"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create parent: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("failed to write {rel}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_for_read_write_and_tree() {
        let base = std::env::temp_dir().join(format!("bebok-explorer-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let secret = base.join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        for rel in [
            "../secret.txt",
            r"..\secret.txt",
            "/tmp/secret.txt",
            r"C:\secret.txt",
        ] {
            assert!(validate_rel(&root, rel).is_err(), "{rel}");
            assert!(read_file_text(&root, rel).is_err(), "{rel}");
            assert!(write_file_text(&root, rel, "overwritten").is_err(), "{rel}");
            assert!(list_children(&root, rel).is_empty(), "{rel}");
            assert!(tree_text(&root, rel, 2).starts_with("error:"), "{rel}");
        }
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret");
        std::fs::remove_dir_all(base).unwrap();
    }
}
