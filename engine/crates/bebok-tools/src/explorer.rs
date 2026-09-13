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
        // Always `/`-separated: the walker appends the child with the OS
        // separator to the `/`-separated `rel` the client sent, which on
        // Windows produced `apps/frontend\\file.ts` (E2E R9). The client
        // splits and joins explorer paths on `/`.
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
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

/// Maximum file size for binary reads: 10 MiB.
pub const MAX_BINARY_READ_BYTES: usize = 10 * 1024 * 1024;

/// Read a file's raw bytes for the `/fs/file?binary=true` endpoint, guarding
/// against escaping the root. Returns `Ok(bytes)` for readable files up to
/// [`MAX_BINARY_READ_BYTES`], an error string otherwise.
pub fn read_file_bytes(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let path = normalize_rel(root, rel)?;
    if !path.is_file() {
        return Err(format!("not a file: {rel}"));
    }
    let meta = std::fs::metadata(&path)
        .map_err(|e| format!("failed to stat {rel}: {e}"))?;
    if meta.len() > MAX_BINARY_READ_BYTES as u64 {
        return Err(format!(
            "file too large for binary read: {} bytes (max {MAX_BINARY_READ_BYTES})",
            meta.len()
        ));
    }
    std::fs::read(&path).map_err(|e| format!("failed to read {rel}: {e}"))
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
    fn read_file_bytes_works_and_rejects_traversal() {
        let base = std::env::temp_dir().join(format!("bebok-explorer-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();

        // Write a binary file (non-UTF8 bytes).
        let bin_path = root.join("image.png");
        let data: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00];
        std::fs::write(&bin_path, &data).unwrap();

        let got = read_file_bytes(&root, "image.png").unwrap();
        assert_eq!(got, data);

        // Not a file.
        assert!(read_file_bytes(&root, ".").is_err());

        // Traversal.
        assert!(read_file_bytes(&root, "../secret.txt").is_err());

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn read_file_bytes_rejects_oversized() {
        let base = std::env::temp_dir().join(format!("bebok-explorer-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();

        // Create a file exceeding the cap.
        let big = root.join("big.bin");
        let payload = vec![0u8; MAX_BINARY_READ_BYTES + 1];
        std::fs::write(&big, &payload).unwrap();

        let err = read_file_bytes(&root, "big.bin").unwrap_err();
        assert!(err.contains("too large"), "{err}");

        std::fs::remove_dir_all(base).unwrap();
    }

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

    /// E2E R9: nested entry paths never mix separators, whatever the OS.
    #[test]
    fn child_paths_are_slash_separated() {
        let root = std::env::temp_dir().join(format!("bebok-explorer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("apps/frontend/src")).unwrap();
        std::fs::write(root.join("apps/frontend/src/main.ts"), "x").unwrap();

        let top = list_children(&root, "");
        assert_eq!(
            top.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            vec!["apps"]
        );
        let nested = list_children(&root, "apps/frontend");
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].path, "apps/frontend/src");
        let files = list_children(&root, "apps/frontend/src");
        assert_eq!(files[0].path, "apps/frontend/src/main.ts");
        assert!(!files[0].path.contains('\\'));
        std::fs::remove_dir_all(root).unwrap();
    }
}
