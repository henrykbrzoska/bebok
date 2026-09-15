//! Mini-registry for the code-index backend: **one active slot** + a
//! disabled fallback.
//!
//! `store/` never names a concrete backend: it asks
//! [`BackendRegistry::spawn`] for one and gets whatever sits in the single
//! active slot (the tantivy factory is installed by
//! [`BackendRegistry::with_default`] / [`BackendRegistry::global`]). That
//! keeps the door open for a second implementation — or a build without one —
//! without touching `store/`.
//!
//! Three properties matter:
//!
//! * **One active backend.** [`BackendRegistry::set_active`] replaces the
//!   slot wholesale — no fan-out, no priority list, no per-instance choice.
//! * **Fallback disabled.** An empty slot (no plugin registered, backend not
//!   compiled in) yields a [`DisabledBackend`]: status `disabled`, `query` →
//!   [`CodeIndexError::Disabled`], every nudge a no-op. With it the server
//!   starts and serves `/index/status` with no backend plugin at all —
//!   instance creation never fails on a missing index implementation.
//! * **Kill-switch last word.** `BEBOK_NO_INDEX=1` is applied *here*, on
//!   every spawn, so it wins over whatever the caller computed: the active
//!   factory is asked for a backend with `enabled = false`, which parks the
//!   orchestrator and reports the status as `disabled`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use super::backend::{
    CodeIndexBackend, CodeIndexError, CodeIndexHit, CodeIndexStatusDto, StatusCallback,
};
use crate::CODE_INDEX_DISABLED;

/// Name reported by the fallback backend.
pub const DISABLED_BACKEND_NAME: &str = "disabled";

/// Everything the active backend needs to serve one instance directory.
pub struct BackendRequest {
    /// Instance (project) root that gets indexed.
    pub root: PathBuf,
    /// `<data>/instances/<hash>/index` — where the index files live.
    pub index_dir: PathBuf,
    /// Extra glob excludes from `code_index.exclude`.
    pub excludes: Vec<String>,
    /// `code_index.maxFiles` scan cap.
    pub max_files: usize,
    /// Config-level switch (the `BEBOK_NO_INDEX` kill-switch is re-applied
    /// by [`BackendRegistry::spawn`], so pass the config value verbatim).
    pub enabled: bool,
    /// Status sink; fired on every transition (plus once at spawn).
    pub on_status: StatusCallback,
}

/// Builds the active backend for one instance.
pub type BackendFactory = Arc<dyn Fn(BackendRequest) -> Arc<dyn CodeIndexBackend> + Send + Sync>;

/// A backend installed in the active slot.
struct Registered {
    name: String,
    factory: BackendFactory,
}

/// Registry holding exactly one active backend.
pub struct BackendRegistry {
    active: RwLock<Option<Registered>>,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::with_default()
    }
}

impl BackendRegistry {
    /// Empty slot: every [`BackendRegistry::spawn`] falls back to the
    /// disabled backend (server starts without an index plugin).
    pub fn new() -> Self {
        Self {
            active: RwLock::new(None),
        }
    }

    /// Registry with the built-in backend (tantivy) installed in the slot.
    pub fn with_default() -> Self {
        Self::with_backend(
            super::factory::TANTIVY_BACKEND_NAME,
            super::factory::tantivy_factory(),
        )
    }

    /// Registry with an explicit backend in the slot (test/injection seam).
    pub fn with_backend(name: impl Into<String>, factory: BackendFactory) -> Self {
        let registry = Self::new();
        registry.set_active(name, factory);
        registry
    }

    /// The process-wide registry, lazily initialised with the default
    /// (tantivy) backend installed.
    pub fn global() -> Arc<BackendRegistry> {
        static GLOBAL: OnceLock<Arc<BackendRegistry>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Arc::new(BackendRegistry::with_default()))
            .clone()
    }

    /// Install `factory` as **the** active backend, replacing any previous
    /// one (existing instances keep the backend they were built with).
    pub fn set_active(&self, name: impl Into<String>, factory: BackendFactory) {
        let name = name.into();
        tracing::info!(backend = %name, "code-index backend registered");
        *self.active.write().unwrap() = Some(Registered { name, factory });
    }

    /// Empty the slot; subsequent spawns use the disabled fallback.
    pub fn clear(&self) {
        *self.active.write().unwrap() = None;
    }

    /// Name of the active backend, `None` when the slot is empty.
    pub fn active_name(&self) -> Option<String> {
        self.active.read().unwrap().as_ref().map(|r| r.name.clone())
    }

    /// Whether any backend is installed in the slot.
    pub fn is_installed(&self) -> bool {
        self.active.read().unwrap().is_some()
    }

    /// Build the backend for one instance: the active one when the slot is
    /// filled, otherwise the disabled fallback. Never fails — a missing
    /// plugin is a startup condition, not an error.
    pub fn spawn(&self, request: BackendRequest) -> Arc<dyn CodeIndexBackend> {
        let root = request.root.clone();
        // Kill-switch: the environment wins over the caller's `enabled`.
        let enabled = request.enabled && !super::indexing_disabled();
        let factory = self
            .active
            .read()
            .unwrap()
            .as_ref()
            .map(|r| r.factory.clone());
        match factory {
            Some(factory) => factory(BackendRequest { enabled, ..request }),
            None => {
                tracing::warn!(
                    root = %root.display(),
                    "no code-index backend registered: serving the disabled fallback"
                );
                Arc::new(DisabledBackend::with_callback(root, request.on_status))
            }
        }
    }
}

/// Inert backend used when the active slot is empty (or the index is off).
///
/// Status is permanently `disabled`, `query` fails with
/// [`CodeIndexError::Disabled`] and the change hooks are no-ops, so an
/// instance built without a backend behaves exactly like an instance whose
/// index is switched off in the settings.
pub struct DisabledBackend {
    root: PathBuf,
    callbacks: Mutex<Vec<StatusCallback>>,
}

impl DisabledBackend {
    /// Fallback with no status listener yet.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            callbacks: Mutex::new(Vec::new()),
        }
    }

    /// Fallback that reports its `disabled` status to `cb` immediately.
    pub fn with_callback(root: PathBuf, cb: StatusCallback) -> Self {
        let backend = Self {
            root,
            callbacks: Mutex::new(vec![cb.clone()]),
        };
        cb(backend.status());
        backend
    }
}

#[async_trait::async_trait]
impl CodeIndexBackend for DisabledBackend {
    fn name(&self) -> &str {
        DISABLED_BACKEND_NAME
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn enabled(&self) -> bool {
        false
    }

    fn status(&self) -> CodeIndexStatusDto {
        CodeIndexStatusDto {
            status: CODE_INDEX_DISABLED.to_string(),
            files: 0,
            symbols: 0,
        }
    }

    async fn query(
        &self,
        _q: &str,
        _extension: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<CodeIndexHit>, CodeIndexError> {
        Err(CodeIndexError::Disabled)
    }

    fn notify_changed(&self, _rel_path: Option<&str>) {}

    fn rebuild(&self) {}

    fn ensure(&self) {}

    fn subscribe_status(&self, cb: StatusCallback) {
        self.callbacks.lock().unwrap().push(cb.clone());
        cb(self.status());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request(root: PathBuf, enabled: bool) -> BackendRequest {
        let index_dir = root.join("index");
        BackendRequest {
            root,
            index_dir,
            excludes: vec![],
            max_files: 1000,
            enabled,
            on_status: Arc::new(|_| {}),
        }
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bebok-reg-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn empty_slot_serves_the_disabled_fallback() {
        let registry = BackendRegistry::new();
        assert!(!registry.is_installed());
        assert_eq!(registry.active_name(), None);

        let root = tmp("empty");
        let backend = registry.spawn(request(root.clone(), true));
        assert_eq!(backend.name(), DISABLED_BACKEND_NAME);
        assert_eq!(backend.root(), root.as_path());
        assert!(!backend.enabled());
        assert_eq!(backend.status().status, CODE_INDEX_DISABLED);
        assert!(matches!(
            backend.query("anything", None, 10).await,
            Err(CodeIndexError::Disabled)
        ));
        // Nudges are inert, subscribing still reports the disabled status.
        backend.notify_changed(Some("src/main.rs"));
        backend.rebuild();
        backend.ensure();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_c = seen.clone();
        backend.subscribe_status(Arc::new(move |s| seen_c.lock().unwrap().push(s.status)));
        assert_eq!(*seen.lock().unwrap(), vec![CODE_INDEX_DISABLED.to_string()]);

        // The bare constructor describes the same inert backend.
        let bare = DisabledBackend::new(root);
        assert_eq!(bare.status().status, CODE_INDEX_DISABLED);
        assert!(matches!(
            bare.query("x", None, 1).await,
            Err(CodeIndexError::Disabled)
        ));
    }

    #[tokio::test]
    async fn active_slot_wins_and_can_be_replaced() {
        let registry = BackendRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let first = calls.clone();
        registry.set_active("first", {
            Arc::new(move |req: BackendRequest| {
                first.fetch_add(1, Ordering::SeqCst);
                Arc::new(DisabledBackend::with_callback(req.root, req.on_status))
            })
        });
        assert_eq!(registry.active_name().as_deref(), Some("first"));
        let backend = registry.spawn(request(tmp("first"), true));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.name(), DISABLED_BACKEND_NAME);

        // A single slot: the replacement is what gets spawned from now on.
        registry.set_active("second", {
            Arc::new(move |req: BackendRequest| {
                Arc::new(DisabledBackend::with_callback(req.root, req.on_status))
            })
        });
        assert_eq!(registry.active_name().as_deref(), Some("second"));
        let _ = registry.spawn(request(tmp("second"), true));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "old factory must be gone");
        registry.clear();
        assert!(!registry.is_installed());
    }

    #[tokio::test]
    async fn spawn_publishes_the_initial_status() {
        let root = tmp("initial");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_c = seen.clone();
        let mut req = request(root.clone(), true);
        req.on_status = Arc::new(move |s| seen_c.lock().unwrap().push(s.status));
        // Empty slot: the fallback reports `disabled` at spawn time, so the
        // store has a status snapshot even without a backend plugin.
        let _ = BackendRegistry::new().spawn(req);
        assert_eq!(*seen.lock().unwrap(), vec![CODE_INDEX_DISABLED.to_string()]);
    }

    #[tokio::test]
    async fn global_registry_has_the_default_backend() {
        let registry = BackendRegistry::global();
        assert!(registry.is_installed());
        assert_eq!(
            registry.active_name().as_deref(),
            Some(super::super::factory::TANTIVY_BACKEND_NAME)
        );
        // Same singleton on every call.
        assert!(Arc::ptr_eq(&registry, &BackendRegistry::global()));
    }

    #[tokio::test]
    async fn kill_switch_beats_the_requested_flag() {
        // The registry re-applies `BEBOK_NO_INDEX` on every spawn, so the
        // factory (or the fallback) only ever sees the effective flag.
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_c = observed.clone();
        let factory: BackendFactory = Arc::new(move |req: BackendRequest| {
            observed_c.lock().unwrap().push(req.enabled);
            Arc::new(DisabledBackend::with_callback(req.root, req.on_status))
        });
        let registry = BackendRegistry::with_backend("probe", factory);

        unsafe { std::env::set_var("BEBOK_NO_INDEX", "1") };
        let _ = registry.spawn(request(tmp("kill"), true));
        assert_eq!(*observed.lock().unwrap(), vec![false]);

        // Kill-switch off: the caller's flag is what the factory sees.
        unsafe { std::env::remove_var("BEBOK_NO_INDEX") };
        let _ = registry.spawn(request(tmp("kill2"), true));
        assert_eq!(*observed.lock().unwrap(), vec![false, true]);
    }
}
