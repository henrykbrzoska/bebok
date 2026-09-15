//! Tantivy full-text index over file contents.

use std::path::{Path, PathBuf};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{STORED, Schema, TEXT, Value};
use tantivy::{Index, IndexWriter, doc};

/// One search hit.
#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub score: f32,
}

/// Full-text code index stored under `<index_dir>/tantivy`.
pub struct CodeIndex {
    dir: PathBuf,
    index: Index,
    path_field: tantivy::schema::Field,
    content_field: tantivy::schema::Field,
    extension_field: tantivy::schema::Field,
}

impl CodeIndex {
    /// Directory the index lives under.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn build_schema() -> (
    Schema,
    tantivy::schema::Field,
    tantivy::schema::Field,
    tantivy::schema::Field,
) {
    let mut builder = Schema::builder();
    let path_field = builder.add_text_field("path", TEXT | STORED);
    let content_field = builder.add_text_field("content", TEXT);
    let extension_field = builder.add_text_field("extension", TEXT | STORED);
    (builder.build(), path_field, content_field, extension_field)
}

fn extension_of(path: &str) -> &str {
    match path.rsplit('.').next() {
        Some(ext) if ext.len() < path.len() => ext,
        _ => "",
    }
}

fn matches_extension(path: &str, filter: &str) -> bool {
    let filter = filter.trim_start_matches('.');
    extension_of(path) == filter
}

impl CodeIndex {
    /// Open (creating) the index under `index_dir/tantivy`.
    pub fn open(index_dir: &Path) -> Result<Self, String> {
        let dir = index_dir.join("tantivy");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (schema, path_field, content_field, extension_field) = build_schema();
        let index = Index::create_in_dir(&dir, schema.clone())
            .or_else(|_| Index::open_in_dir(&dir))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            dir,
            index,
            path_field,
            content_field,
            extension_field,
        })
    }

    /// Delete everything and index `files` (path + content pairs) from scratch.
    pub fn rebuild(&self, files: &[(String, String)]) -> Result<(), String> {
        let mut writer: IndexWriter = self.index.writer(50_000_000).map_err(|e| e.to_string())?;
        writer.delete_all_documents().map_err(|e| e.to_string())?;
        for (path, content) in files {
            writer
                .add_document(doc!(
                    self.path_field => path.as_str(),
                    self.content_field => content.as_str(),
                    self.extension_field => extension_of(path),
                ))
                .map_err(|e| e.to_string())?;
        }
        writer.commit().map_err(|e| e.to_string())?;
        // Release the write lock before any reader touches the index.
        drop(writer);
        Ok(())
    }

    /// Full-text query over file contents, optionally restricted to one extension.
    pub fn query(
        &self,
        q: &str,
        extension_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Hit>, String> {
        let reader = self.index.reader().map_err(|e| e.to_string())?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.content_field]);
        let query = parser.parse_query(q).map_err(|e| e.to_string())?;
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit.max(1)))
            .map_err(|e| e.to_string())?;
        let mut hits = Vec::new();
        for (score, addr) in top {
            let doc: tantivy::TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
            let path = doc
                .get_first(self.path_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(f) = extension_filter
                && !matches_extension(&path, f)
            {
                continue;
            }
            hits.push(Hit { path, score });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_matches_by_path_segment() {
        assert!(matches_extension("src/main.rs", "rs"));
        assert!(matches_extension("src/main.rs", ".rs"));
        assert!(!matches_extension("src/main.ts", "rs"));
    }
}
