//! Per-instance code index: scan, full-text search, watcher.
//!
//! This crate owns the indexing engine — status vocabulary, scan, schema,
//! tantivy search, the background orchestrator and the backend registry.
//! Directory layout helpers (`code_index_dir`, `ensure_code_index_dir`) are
//! provided by the host application; this crate never touches them.

pub mod backend;
pub mod backend_registry;
pub mod factory;
pub mod orchestrator;
pub mod scan;
pub mod schema;
pub mod search;
pub mod status;
pub mod watcher;

pub use backend::{
    CODE_INDEX_SCHEMA_VERSION, CodeIndexBackend, CodeIndexError, CodeIndexHit, CodeIndexState,
    CodeIndexStatusDto, INDEX_INVALIDATING_TOOLS, StatusCallback,
};
pub use backend_registry::{
    BackendFactory, BackendRegistry, BackendRequest, DISABLED_BACKEND_NAME, DisabledBackend,
};
pub use orchestrator::{DEBOUNCE_MS, IndexOrchestrator};
pub use scan::{MAX_FILE_SIZE, ScannedFile, file_fingerprint, needs_reindex, scan_project};
pub use schema::{DEFAULT_EXCLUDES, IndexedFile, is_excluded};
pub use search::{CodeIndex, Hit};
pub use status::CodeIndexStatus;
pub use watcher::{IndexStatus, spawn_watcher};

/// Status value: the index is usable.
pub const CODE_INDEX_READY: &str = "ready";
/// Status value: a scan is currently running.
pub const CODE_INDEX_INDEXING: &str = "indexing";
/// Status value: indexing is off (the default).
pub const CODE_INDEX_DISABLED: &str = "disabled";

/// True when indexing is disabled via `BEBOK_NO_INDEX=1`.
pub fn indexing_disabled() -> bool {
    std::env::var("BEBOK_NO_INDEX").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_respects_gitignore_and_excludes() {
        let dir = std::env::temp_dir().join(format!("bebok-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("ignored.txt"), "x").unwrap();
        std::fs::write(dir.join("target").join("a"), "x").unwrap();
        let files = scan_project(&dir, &[], 1000);
        let rels: Vec<_> = files.iter().map(|f| f.rel.as_str()).collect();
        assert!(rels.contains(&"src/main.rs"), "got {rels:?}");
        assert!(
            !rels.iter().any(|r| r.contains("ignored.txt")),
            "got {rels:?}"
        );
        assert!(
            !rels.iter().any(|r| r.starts_with("target")),
            "got {rels:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tantivy_rebuild_query_roundtrip() {
        let dir = std::env::temp_dir().join(format!("bebok-tant-{}", uuid::Uuid::new_v4()));
        let idx = CodeIndex::open(&dir).unwrap();
        idx.rebuild(&[
            ("src/main.rs".to_string(), "fn main hello world".to_string()),
            (
                "src/app.ts".to_string(),
                "completely different content here".to_string(),
            ),
        ])
        .unwrap();
        let hits = idx.query("hello", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/main.rs");
        let filtered = idx.query("hello", Some("ts"), 10).unwrap();
        assert!(filtered.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexing_disabled_reads_env() {
        unsafe { std::env::set_var("BEBOK_NO_INDEX", "1") };
        assert!(indexing_disabled());
        unsafe { std::env::remove_var("BEBOK_NO_INDEX") };
        assert!(!indexing_disabled());
    }
}
