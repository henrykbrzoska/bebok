//! Compatibility shim: the code-index engine moved to the
//! [`bebok-code-index`](bebok_code_index) crate (Krok 2, verbatim move, zero
//! logic change). This module re-exports the engine's public surface under
//! the old `crate::index::` paths so the 27 use sites in `store/`, `agent/`
//! and the query adapter keep compiling untouched, plus the two `From`
//! conversions between the engine DTOs and the store's `CodeIndexStatus`
//! (which stays in `store::code_index` with the directory helpers).

pub use bebok_code_index::{
    BackendFactory, BackendRegistry, BackendRequest, CODE_INDEX_DISABLED, CODE_INDEX_INDEXING,
    CODE_INDEX_READY, CODE_INDEX_SCHEMA_VERSION, CodeIndex, CodeIndexBackend, CodeIndexError,
    CodeIndexHit, CodeIndexState, CodeIndexStatusDto, DEBOUNCE_MS, DEFAULT_EXCLUDES,
    DISABLED_BACKEND_NAME, DisabledBackend, Hit, INDEX_INVALIDATING_TOOLS, IndexOrchestrator,
    IndexStatus, IndexedFile, MAX_FILE_SIZE, ScannedFile, StatusCallback, file_fingerprint,
    indexing_disabled, is_excluded, needs_reindex, scan_project, spawn_watcher,
};

pub use bebok_code_index::factory;

use crate::store::code_index::CodeIndexStatus;

/// Invalidation callback after a successful file mutation: receives the
/// changed path relative to the instance root (`None` = unknown / rebuild
/// everything). Shared alias so the `turn` / `exec` / `delegation` seams do
/// not repeat the raw `Arc<dyn Fn(..)>` type (clippy `type_complexity`).
pub type CodeIndexChangedCallback = std::sync::Arc<dyn Fn(Option<&str>) + Send + Sync>;

impl From<CodeIndexStatus> for CodeIndexStatusDto {
    fn from(s: CodeIndexStatus) -> Self {
        Self {
            status: s.status,
            files: s.files,
            symbols: s.symbols,
        }
    }
}

impl From<CodeIndexStatusDto> for CodeIndexStatus {
    fn from(d: CodeIndexStatusDto) -> Self {
        Self {
            status: d.status,
            files: d.files,
            symbols: d.symbols,
        }
    }
}
