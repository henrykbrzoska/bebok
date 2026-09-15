//! Project scan: enumerate indexable files with gitignore support.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::schema::{IndexedFile, is_excluded};

/// Cap for indexed file size (2 MiB).
pub const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
/// How many leading bytes feed the fingerprint hash.
pub const FINGERPRINT_BYTES: usize = 16384;

/// One file discovered by [`scan_project`].
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub rel: String,
    pub abs: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
}

/// Walk `root` (gitignore-aware) and collect files worth indexing.
pub fn scan_project(root: &Path, user_excludes: &[String], max_files: usize) -> Vec<ScannedFile> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .git_ignore(true)
        .hidden(false)
        .parents(true)
        .require_git(false)
        .build();
    for entry in walker.filter_map(|e| e.ok()) {
        if out.len() >= max_files {
            break;
        }
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_slash = rel.to_string_lossy().replace('\\', "/");
        if is_excluded(&rel_slash, user_excludes) {
            continue;
        }
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if ft.is_dir() {
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let (size, mtime_ns) = match entry.metadata() {
            Ok(m) => (m.len(), mtime_ns_of(&m)),
            Err(_) => continue,
        };
        if size >= MAX_FILE_SIZE {
            continue;
        }
        out.push(ScannedFile {
            rel: rel_slash,
            abs: path.to_path_buf(),
            size,
            mtime_ns,
        });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.truncate(max_files);
    out
}

fn mtime_ns_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| (d.as_nanos().min(i64::MAX as u128)) as i64)
        .unwrap_or(0)
}

/// blake3 hex of the first [`FINGERPRINT_BYTES`] bytes of the file.
pub fn file_fingerprint(abs: &Path) -> io::Result<String> {
    let mut f = File::open(abs)?;
    let mut buf = vec![0u8; FINGERPRINT_BYTES];
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if n < FINGERPRINT_BYTES {
            break;
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// True when the file must be (re)indexed: no metadata yet, or size/mtime drift.
pub fn needs_reindex(meta: Option<&IndexedFile>, size: u64, mtime_ns: i64) -> bool {
    match meta {
        None => true,
        Some(m) => m.size != size || m.mtime_ns != mtime_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_reindex_cases() {
        assert!(needs_reindex(None, 10, 20));
        let meta = IndexedFile {
            path_rel: "a.rs".into(),
            mtime_ns: 20,
            size: 10,
            hash: "h".into(),
            updated_at: 0,
        };
        assert!(!needs_reindex(Some(&meta), 10, 20));
        assert!(needs_reindex(Some(&meta), 11, 20));
        assert!(needs_reindex(Some(&meta), 10, 21));
    }

    #[test]
    fn fingerprint_is_stable_hex() {
        let dir = std::env::temp_dir().join(format!("bebok-fp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let a = file_fingerprint(&p).unwrap();
        let b = file_fingerprint(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
