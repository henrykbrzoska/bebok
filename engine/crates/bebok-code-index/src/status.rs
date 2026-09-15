//! Light status snapshot of an instance's code index.
//!
//! The status vocabulary lives with the indexing engine. The directory
//! helpers (`code_index_dir`, `ensure_code_index_dir`) are provided by the
//! host application — this crate never touches the filesystem layout.

/// Light status snapshot of an instance's code index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CodeIndexStatus {
    pub status: String,
    pub files: usize,
    pub symbols: usize,
}

impl Default for CodeIndexStatus {
    fn default() -> Self {
        Self {
            status: crate::CODE_INDEX_DISABLED.to_string(),
            files: 0,
            symbols: 0,
        }
    }
}
