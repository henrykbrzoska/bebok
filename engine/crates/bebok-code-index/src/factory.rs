//! Default code-index backend wiring (tantivy).
//!
//! [`tantivy_factory`] is what gets installed in the single active slot of
//! [`crate::backend_registry::BackendRegistry`]; the store never calls
//! the orchestrator directly. The built backend implements the stable
//! DTO-based [`crate::backend::CodeIndexBackend`] contract and hands
//! the orchestrator's [`CodeIndexStatusDto`] to listeners verbatim — there is
//! exactly one status callback type in the crate (`index::StatusCallback`).

use std::path::PathBuf;
use std::sync::Arc;

use super::backend::{self, CodeIndexError, CodeIndexHit, CodeIndexStatusDto};
use super::backend_registry::BackendFactory;
use super::orchestrator::IndexOrchestrator;

/// Name the built-in backend is registered under.
pub const TANTIVY_BACKEND_NAME: &str = "tantivy";

/// Factory for the active-slot registration: one backend per instance.
pub fn tantivy_factory() -> BackendFactory {
    Arc::new(|req: super::backend_registry::BackendRequest| {
        spawn_default_backend(
            req.root,
            req.index_dir,
            req.excludes,
            req.max_files,
            req.enabled,
            req.on_status,
        )
    })
}

/// Spawn the default (tantivy) backend for one instance directory.
///
/// `on_status` is the single DTO-based callback type
/// ([`crate::StatusCallback`]); it is forwarded straight into the
/// orchestrator, which already speaks [`CodeIndexStatusDto`].
pub fn spawn_default_backend(
    root: PathBuf,
    index_dir: PathBuf,
    excludes: Vec<String>,
    max_files: usize,
    enabled: bool,
    on_status: backend::StatusCallback,
) -> Arc<dyn backend::CodeIndexBackend> {
    let orch = IndexOrchestrator::spawn(root, index_dir, excludes, max_files, enabled, on_status);
    Arc::new(TantivyBackend(orch))
}

struct TantivyBackend(Arc<IndexOrchestrator>);

#[async_trait::async_trait]
impl backend::CodeIndexBackend for TantivyBackend {
    fn name(&self) -> &str {
        TANTIVY_BACKEND_NAME
    }

    fn root(&self) -> &std::path::Path {
        self.0.root()
    }

    fn enabled(&self) -> bool {
        self.0.enabled()
    }

    fn status(&self) -> CodeIndexStatusDto {
        self.0.status()
    }

    async fn query(
        &self,
        q: &str,
        extension: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeIndexHit>, CodeIndexError> {
        let limit = limit.clamp(1, 50);
        match self.0.query(q, extension, limit).await {
            Ok(hits) => Ok(hits.into_iter().map(CodeIndexHit::from).collect()),
            Err(e) => Err(if e.contains("disabled") {
                CodeIndexError::Disabled
            } else if e.contains("not built") {
                CodeIndexError::NotBuilt
            } else {
                CodeIndexError::Backend(e)
            }),
        }
    }

    fn notify_changed(&self, _rel_path: Option<&str>) {
        self.0.notify_changed();
    }

    fn rebuild(&self) {
        self.0.rebuild();
    }

    fn ensure(&self) {
        if self.enabled() {
            self.0.notify_changed();
        }
    }

    fn subscribe_status(&self, cb: backend::StatusCallback) {
        self.0.subscribe_status(cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend_registry::{BackendRegistry, BackendRequest};

    #[tokio::test]
    async fn factory_returns_trait_object() {
        // Disabled backend: status "disabled", query -> Disabled.
        let base = std::env::temp_dir().join(format!("bebok-fact-{}", uuid::Uuid::new_v4()));
        let idx = base.join("idx-dis");
        let backend = spawn_default_backend(
            base.join("proj"),
            idx,
            vec![],
            1000,
            false,
            Arc::new(|_| {}),
        );
        assert_eq!(backend.name(), TANTIVY_BACKEND_NAME);
        assert!(!backend.enabled());
        assert_eq!(backend.status().status, "disabled");
        assert!(matches!(
            backend.query("x", None, 5).await,
            Err(CodeIndexError::Disabled)
        ));

        // Enabled backend over a tmp dir with one file: reaches ready < 5 s.
        let root = base.join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn factoryprobefn() {}").unwrap();
        let backend = spawn_default_backend(
            root,
            base.join("idx-en"),
            vec![],
            1000,
            true,
            Arc::new(|_| {}),
        );
        let mut ready = false;
        for _ in 0..100 {
            if backend.status().status == "ready" {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(ready, "factory backend never became ready");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The registry slot is what the store goes through: `set_active` with
    /// this factory yields the tantivy backend.
    #[tokio::test]
    async fn registry_spawns_the_tantivy_backend() {
        let base = std::env::temp_dir().join(format!("bebok-factreg-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let registry = BackendRegistry::with_default();
        assert_eq!(
            registry.active_name().as_deref(),
            Some(TANTIVY_BACKEND_NAME)
        );
        let backend = registry.spawn(BackendRequest {
            root,
            index_dir: base.join("idx"),
            excludes: vec![],
            max_files: 1000,
            enabled: false,
            on_status: Arc::new(|_| {}),
        });
        assert_eq!(backend.name(), TANTIVY_BACKEND_NAME);
        assert!(!backend.enabled());
        assert_eq!(backend.status().status, "disabled");
        let _ = std::fs::remove_dir_all(&base);
    }
}
