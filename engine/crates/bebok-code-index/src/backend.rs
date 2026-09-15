//! Stable contract for the per-instance code index backend (PR1 foundation).
//!
//! The search/orchestrator stack underneath may evolve (tantivy today,
//! something else tomorrow); dependent packages (`bebok-server` routes,
//! `store`, agent tools) must program against [`CodeIndexBackend`] only.
//! Concrete wiring lives in [`crate::factory`] and is installed in
//! the single active slot of [`crate::backend_registry`]; the store
//! asks that registry for a backend (and gets a disabled fallback when the
//! slot is empty).

use std::path::Path;
use std::sync::Arc;

/// Coarse lifecycle state of the index, mapped onto the store status strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeIndexState {
    Disabled,
    Indexing,
    Ready,
}

impl CodeIndexState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => crate::CODE_INDEX_DISABLED,
            Self::Indexing => crate::CODE_INDEX_INDEXING,
            Self::Ready => crate::CODE_INDEX_READY,
        }
    }
}

/// Wire DTO for the index status snapshot (SSE + REST shape).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeIndexStatusDto {
    pub status: String,
    pub files: usize,
    #[serde(default)]
    pub symbols: usize,
}

/// Wire DTO for a single search hit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeIndexHit {
    pub path: String,
    pub score: f32,
}

/// Failures surfaced by [`CodeIndexBackend::query`].
#[derive(Debug, thiserror::Error)]
pub enum CodeIndexError {
    #[error("code index is disabled")]
    Disabled,
    #[error("index not built yet")]
    NotBuilt,
    #[error("index error: {0}")]
    Backend(String),
}

/// Callback invoked on every status transition (bus publishing seam).
/// Carries the wire DTO, so listeners do not translate: the orchestrator's
/// internal status *is* [`CodeIndexStatusDto`] and this is the only callback
/// type in the crate.
pub type StatusCallback = Arc<dyn Fn(CodeIndexStatusDto) + Send + Sync>;

/// Stable backend contract for the code index.
#[async_trait::async_trait]
pub trait CodeIndexBackend: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn root(&self) -> &Path;
    fn enabled(&self) -> bool;
    fn status(&self) -> CodeIndexStatusDto;
    async fn query(
        &self,
        q: &str,
        extension: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeIndexHit>, CodeIndexError>;
    fn notify_changed(&self, rel_path: Option<&str>);
    fn rebuild(&self);
    fn ensure(&self);
    fn subscribe_status(&self, cb: StatusCallback);
}

/// Tools whose file mutations invalidate (parts of) the code index.
pub const INDEX_INVALIDATING_TOOLS: &[&str] = &[
    "write_file",
    "append_file",
    "edit_file",
    "mv",
    "rm",
    "cp",
    "ln",
];

/// Version of the on-disk / wire code-index schema owned by this contract.
pub const CODE_INDEX_SCHEMA_VERSION: u32 = 1;

impl From<crate::search::Hit> for CodeIndexHit {
    fn from(h: crate::search::Hit) -> Self {
        Self {
            path: h.path,
            score: h.score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn dto_wire_shape() {
        let dto = CodeIndexStatusDto {
            status: crate::CODE_INDEX_READY.to_string(),
            files: 3,
            symbols: 0,
        };
        let v = serde_json::to_value(&dto).unwrap();
        let obj = v.as_object().unwrap();
        let keys: std::collections::BTreeSet<&String> = obj.keys().collect();
        let expected: std::collections::BTreeSet<String> = ["status", "files", "symbols"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let actual: std::collections::BTreeSet<String> = keys.into_iter().cloned().collect();
        assert_eq!(actual, expected);
        assert_eq!(
            obj["status"],
            serde_json::Value::String("ready".to_string())
        );
        // Unknown fields are ignored on deserialization.
        let back: CodeIndexStatusDto = serde_json::from_value(serde_json::json!({
            "status": "ready",
            "files": 3,
            "symbols": 0,
            "future_field": 123,
        }))
        .unwrap();
        assert_eq!(back.status, "ready");
        assert_eq!(back.files, 3);
        // `symbols` defaults to 0 when absent.
        let minimal: CodeIndexStatusDto = serde_json::from_value(serde_json::json!({
            "status": "indexing",
            "files": 0,
        }))
        .unwrap();
        assert_eq!(minimal.symbols, 0);
    }

    struct MockBackend {
        enabled: bool,
        status: Mutex<CodeIndexStatusDto>,
        callbacks: Mutex<Vec<StatusCallback>>,
    }

    impl MockBackend {
        fn set_status(&self, next: CodeIndexStatusDto) {
            *self.status.lock().unwrap() = next.clone();
            for cb in self.callbacks.lock().unwrap().iter() {
                cb(next.clone());
            }
        }
    }

    #[async_trait::async_trait]
    impl CodeIndexBackend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }
        fn root(&self) -> &Path {
            Path::new("/tmp")
        }
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn status(&self) -> CodeIndexStatusDto {
            self.status.lock().unwrap().clone()
        }
        async fn query(
            &self,
            _q: &str,
            _extension: Option<&str>,
            limit: usize,
        ) -> Result<Vec<CodeIndexHit>, CodeIndexError> {
            if !self.enabled {
                return Err(CodeIndexError::Disabled);
            }
            let limit = limit.clamp(1, 50);
            Ok((0..limit)
                .map(|i| CodeIndexHit {
                    path: format!("src/f{i}.rs"),
                    score: 1.0,
                })
                .collect())
        }
        fn notify_changed(&self, _rel_path: Option<&str>) {
            // Disabled backend: nothing to notify.
        }
        fn rebuild(&self) {}
        fn ensure(&self) {
            if !self.enabled {
                return;
            }
            self.set_status(CodeIndexStatusDto {
                status: crate::CODE_INDEX_INDEXING.to_string(),
                files: 0,
                symbols: 0,
            });
            self.set_status(CodeIndexStatusDto {
                status: crate::CODE_INDEX_READY.to_string(),
                files: 1,
                symbols: 0,
            });
        }
        fn subscribe_status(&self, cb: StatusCallback) {
            self.callbacks.lock().unwrap().push(cb);
        }
    }

    #[tokio::test]
    async fn mock_backend_implements_contract() {
        // Disabled backend: notify is a no-op, query fails with Disabled.
        let disabled = MockBackend {
            enabled: false,
            status: Mutex::new(CodeIndexStatusDto {
                status: crate::CODE_INDEX_DISABLED.to_string(),
                files: 0,
                symbols: 0,
            }),
            callbacks: Mutex::new(Vec::new()),
        };
        disabled.notify_changed(Some("src/main.rs"));
        assert!(matches!(
            disabled.query("x", None, 5).await,
            Err(CodeIndexError::Disabled)
        ));

        // Enabled backend: ensure drives indexing -> ready transitions.
        let backend = MockBackend {
            enabled: true,
            status: Mutex::new(CodeIndexStatusDto {
                status: crate::CODE_INDEX_INDEXING.to_string(),
                files: 0,
                symbols: 0,
            }),
            callbacks: Mutex::new(Vec::new()),
        };
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_c = seen.clone();
        backend.subscribe_status(Arc::new(move |s| {
            seen_c.lock().unwrap().push(s.status);
        }));
        backend.ensure();
        let transitions = seen.lock().unwrap().clone();
        assert_eq!(
            transitions,
            vec!["indexing".to_string(), "ready".to_string()]
        );
        assert_eq!(backend.status().status, "ready");

        // Limit is clamped to 1..=50.
        let hits = backend.query("q", None, 500).await.unwrap();
        assert_eq!(hits.len(), 50);
        let hits = backend.query("q", None, 0).await.unwrap();
        assert_eq!(hits.len(), 1);
    }
}
