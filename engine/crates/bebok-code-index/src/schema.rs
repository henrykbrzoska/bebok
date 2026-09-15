//! File metadata schema for the per-instance code index.

use serde::{Deserialize, Serialize};

/// Metadata snapshot of one indexed file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexedFile {
    pub path_rel: String,
    pub mtime_ns: i64,
    pub size: u64,
    pub hash: String,
    pub updated_at: i64,
}

/// Directories / path prefixes never indexed.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".bebok",
    "__pycache__",
];

/// True when `rel` is excluded: first path segment or full prefix match
/// against the default excludes plus `user_excludes`.
pub fn is_excluded(rel: &str, user_excludes: &[String]) -> bool {
    let rel = rel.trim_start_matches("./").replace('\\', "/");
    let matches = |pat: &str| {
        let pat = pat.trim_end_matches('/');
        rel == pat || rel.starts_with(&format!("{pat}/")) || first_segment(&rel) == pat
    };
    DEFAULT_EXCLUDES.iter().any(|e| matches(e)) || user_excludes.iter().any(|e| matches(e))
}

fn first_segment(rel: &str) -> &str {
    rel.split('/').next().unwrap_or(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_by_first_segment() {
        assert!(is_excluded("target/foo.rs", &[]));
        assert!(is_excluded(".git/objects/x", &[]));
    }

    #[test]
    fn excluded_by_user_prefix() {
        let user = vec!["vendor".to_string()];
        assert!(is_excluded("vendor/lib/a.js", &user));
        assert!(!is_excluded("src/main.rs", &user));
    }

    #[test]
    fn not_excluded_normal_path() {
        assert!(!is_excluded("src/main.rs", &[]));
    }
}
