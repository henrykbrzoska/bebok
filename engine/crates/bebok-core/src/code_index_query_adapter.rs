use std::sync::Arc;

use bebok_tools::CodeIndexHit as ToolHit;

use crate::index::{CodeIndexBackend, CodeIndexError as CoreCodeIndexError};

/// Adapter wrapping a core backend (the real tantivy one or the
/// disabled fallback) into the tool-facing [`bebok_tools::CodeIndexQuery`]
/// contract. Constructed where the tool is built (see
/// [`Instance::code_index_query_adapter`]); the backend behind it is
/// behind an `Arc<dyn CodeIndexBackend>` so it can be the disabled
/// fallback when the index plugin is absent or off.
pub struct CodeIndexQueryAdapter {
    backend: Arc<dyn CodeIndexBackend>,
}

impl CodeIndexQueryAdapter {
    /// Build the adapter for a backend (real or disabled fallback).
    pub fn new(backend: Arc<dyn CodeIndexBackend>) -> Self {
        Self { backend }
    }

    /// Build the adapter from a backend, boxed as a trait object so
    /// callers (e.g. `Instance`) can store it as a generic `Option<Arc<dyn ...>>`.
    pub fn from_backend(
        backend: Arc<dyn CodeIndexBackend>,
    ) -> Arc<dyn bebok_tools::CodeIndexQuery> {
        Arc::new(Self::new(backend))
    }
}

impl std::fmt::Debug for CodeIndexQueryAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeIndexQueryAdapter")
            .field("name", &self.backend.name())
            .field("enabled", &self.backend.enabled())
            .finish()
    }
}

#[async_trait::async_trait]
impl bebok_tools::CodeIndexQuery for CodeIndexQueryAdapter {
    async fn query(
        &self,
        q: &str,
        extension: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ToolHit>, bebok_tools::CodeIndexError> {
        match self.backend.query(q, extension, limit).await {
            Ok(hits) => Ok(hits
                .into_iter()
                .map(|h: crate::index::CodeIndexHit| ToolHit {
                    path: h.path,
                    score: h.score,
                })
                .collect()),
            Err(CoreCodeIndexError::Disabled) => Err(bebok_tools::CodeIndexError::Disabled),
            Err(CoreCodeIndexError::NotBuilt) => Err(bebok_tools::CodeIndexError::NotBuilt),
            Err(CoreCodeIndexError::Backend(s)) => Err(bebok_tools::CodeIndexError::Backend(s)),
        }
    }

    fn is_available(&self) -> bool {
        self.backend.enabled()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::index::{CodeIndexBackend, CodeIndexHit as CoreHit, CodeIndexStatusDto};

    use bebok_tools::code_search::{CodeIndexError, CodeIndexQuery};

    /// A stub backend: enabled or disabled at will, returns canned hits.
    struct StubBackend {
        enabled: bool,
        hits: Vec<CoreHit>,
    }

    #[async_trait::async_trait]
    impl CodeIndexBackend for StubBackend {
        fn name(&self) -> &str {
            "stub"
        }
        fn root(&self) -> &std::path::Path {
            std::path::Path::new("/tmp")
        }
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn status(&self) -> CodeIndexStatusDto {
            CodeIndexStatusDto {
                status: "ready".to_string(),
                files: self.hits.len(),
                symbols: 0,
            }
        }
        async fn query(
            &self,
            _q: &str,
            _extension: Option<&str>,
            limit: usize,
        ) -> Result<Vec<CoreHit>, crate::index::CodeIndexError> {
            if !self.enabled {
                return Err(crate::index::CodeIndexError::Disabled);
            }
            Ok(self.hits.iter().take(limit).cloned().collect())
        }
        fn notify_changed(&self, _rel_path: Option<&str>) {}
        fn rebuild(&self) {}
        fn ensure(&self) {}
        fn subscribe_status(&self, _cb: crate::index::StatusCallback) {}
    }

    #[tokio::test]
    async fn adapter_delegates_hits() {
        let backend = Arc::new(StubBackend {
            enabled: true,
            hits: vec![
                CoreHit {
                    path: "src/main.rs".into(),
                    score: 1.5,
                },
                CoreHit {
                    path: "src/lib.rs".into(),
                    score: 0.7,
                },
            ],
        });
        let adapter = super::CodeIndexQueryAdapter::new(backend);
        let hits = adapter.query("fn main", None, 10).await.expect("query");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/main.rs");
        assert!((hits[0].score - 1.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn adapter_translates_disabled() {
        let backend = Arc::new(StubBackend {
            enabled: false,
            hits: vec![],
        });
        let adapter = super::CodeIndexQueryAdapter::new(backend);
        assert!(matches!(
            adapter.query("q", None, 10).await,
            Err(CodeIndexError::Disabled)
        ));
        assert!(!adapter.is_available());
    }

    #[tokio::test]
    async fn adapter_translates_not_built() {
        let backend = Arc::new(StubBackend {
            enabled: true,
            hits: vec![],
        });
        let adapter = super::CodeIndexQueryAdapter::new(backend);
        let out = adapter.query("q", None, 10).await.unwrap();
        assert_eq!(out.len(), 0);
        assert!(adapter.is_available());
    }
}
