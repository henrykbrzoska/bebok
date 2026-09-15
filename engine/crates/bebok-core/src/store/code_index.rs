//! Phase 0 wiring for the per-instance code index.
//!
//! This module is only the integration seam: the per-instance index
//! directory (`<data_dir>/instances/<hash>/index`), a light status snapshot
//! and idempotent directory creation. The actual indexing engine lives
//! elsewhere (owned by another work package); nothing here scans or parses
//! code.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::session::persist;

/// Status value: the index is usable.
pub const CODE_INDEX_READY: &str = "ready";
/// Status value: a scan is currently running.
pub const CODE_INDEX_INDEXING: &str = "indexing";
/// Status value: indexing is off (the Phase 0 default).
pub const CODE_INDEX_DISABLED: &str = "disabled";

/// Light status snapshot of an instance's code index.
///
/// Phase 0 carries no engine yet, so `symbols` stays 0 and the default status
/// is `disabled`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CodeIndexStatus {
    pub status: String,
    pub files: usize,
    pub symbols: usize,
}

impl Default for CodeIndexStatus {
    fn default() -> Self {
        Self {
            status: CODE_INDEX_DISABLED.to_string(),
            files: 0,
            symbols: 0,
        }
    }
}

/// Thin per-instance handle: where the index lives plus its last known status.
#[derive(Debug, Clone)]
pub struct InstanceCodeIndex {
    pub index_dir: PathBuf,
    pub status: CodeIndexStatus,
}

impl InstanceCodeIndex {
    pub fn new(index_dir: PathBuf) -> Self {
        Self {
            index_dir,
            status: CodeIndexStatus::default(),
        }
    }

    /// Snapshot of the last known index status.
    pub fn status_snapshot(&self) -> CodeIndexStatus {
        self.status.clone()
    }
}

/// Index directory for an instance: `<data_dir>/instances/<hash>/index`.
/// Uses the same hash/data_dir mechanism as session persistence.
pub fn code_index_dir(data_dir: &Path, directory: &str) -> PathBuf {
    persist::instance_dir(data_dir, directory).join("index")
}

/// Idempotently create the index directory (`create_dir_all`).
pub fn ensure_code_index_dir(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(CoreError::Io)?;
    Ok(dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_index_dir_path_contains_instances_hash_index() {
        let data_dir = PathBuf::from("/data/bebok");
        let dir = code_index_dir(&data_dir, "/projects/acme");
        let expected = data_dir
            .join("instances")
            .join(crate::util::hash_dir("/projects/acme"))
            .join("index");
        assert_eq!(dir, expected);
        assert!(dir.to_string_lossy().contains("instances"));
        assert!(dir.ends_with("index"));
    }

    #[test]
    fn ensure_code_index_dir_creates_dir_and_is_idempotent() {
        let base = std::env::temp_dir().join(format!("bebok-code-index-{}", uuid::Uuid::new_v4()));
        let dir = code_index_dir(&base, "/projects/acme");
        assert!(!dir.exists());
        let created = ensure_code_index_dir(&dir).unwrap();
        assert_eq!(created, dir);
        assert!(dir.is_dir());
        // Second call is a no-op success.
        ensure_code_index_dir(&dir).unwrap();
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_status_is_disabled_with_zero_counts() {
        let status = CodeIndexStatus::default();
        assert_eq!(status.status, CODE_INDEX_DISABLED);
        assert_eq!(status.files, 0);
        assert_eq!(status.symbols, 0);

        let handle = InstanceCodeIndex::new(PathBuf::from("/tmp/idx"));
        let snapshot = handle.status_snapshot();
        assert_eq!(snapshot.status, CODE_INDEX_DISABLED);
        assert_eq!(snapshot.files, 0);
        assert_eq!(snapshot.symbols, 0);
    }
}
