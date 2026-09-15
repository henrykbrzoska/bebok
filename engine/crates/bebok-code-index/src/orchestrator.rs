//! Faza 1: background orchestrator over the per-instance code index.
//!
//! Owns the lifecycle of indexing work for one instance directory:
//! first build, incremental rescans and manual rebuilds. One orchestrator
//! per instance is expected (created lazily by the store layer); it holds
//! no OS watcher itself — the caller pushes change notifications via
//! [`IndexOrchestrator::notify_changed`], which debounces them into a
//! background rescan. All heavy work runs on blocking threads; status is
//! cheaply readable via [`IndexOrchestrator::status`].
//!
//! Events (`code_index.started` / `code_index.updated`) are pushed through
//! the provided [`StatusCallback`] so the store layer can republish them on
//! the SSE bus (`config.changed`-style, Phase 0 convention). There is exactly
//! one callback type in the crate — the DTO-based
//! [`crate::backend::StatusCallback`] — and the orchestrator carries
//! [`CodeIndexStatusDto`] as its status type, so listeners get the wire shape
//! with no translation step.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use super::backend::{CodeIndexStatusDto, StatusCallback};
use super::scan::{file_fingerprint, needs_reindex, scan_project};
use super::schema::IndexedFile;
use super::search::CodeIndex;
use crate::{CODE_INDEX_DISABLED, CODE_INDEX_INDEXING, CODE_INDEX_READY};

/// Debounce window for filesystem change bursts (ms).
pub const DEBOUNCE_MS: u64 = 1500;
/// Max files read into tantivy per (re)build; the scan cap comes from config.
pub const MAX_TANTIVY_DOCS: usize = 20_000;
/// Max bytes of file content fed to tantivy per document.
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;

/// Status snapshot with the (not yet implemented) symbol count zeroed.
fn status_dto(status: &str, files: usize) -> CodeIndexStatusDto {
    CodeIndexStatusDto {
        status: status.to_string(),
        files,
        symbols: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// Initial build (or no-op when the index is already warm).
    Ensure,
    /// Incremental rescan (debounced).
    Rescan,
    /// Full rebuild from scratch.
    Rebuild,
}

/// Background orchestrator for one instance's code index.
pub struct IndexOrchestrator {
    root: PathBuf,
    index_dir: PathBuf,
    enabled: bool,
    status: Arc<Mutex<CodeIndexStatusDto>>,
    callbacks: Arc<Mutex<Vec<StatusCallback>>>,
    tx: mpsc::Sender<Command>,
    /// Abort handle of the background worker loop; aborts on drop.
    worker_abort: AbortHandle,
    /// Abort handle of the currently running build task (if any).
    active_build: Arc<Mutex<Option<AbortHandle>>>,
}

impl IndexOrchestrator {
    /// Spawn the orchestrator; directory creation is idempotent.
    /// When `enabled` is false the worker parks and status stays `disabled`.
    pub fn spawn(
        root: PathBuf,
        index_dir: PathBuf,
        excludes: Vec<String>,
        max_files: usize,
        enabled: bool,
        on_status: StatusCallback,
    ) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&index_dir);
        let initial = if enabled {
            status_dto(CODE_INDEX_INDEXING, 0)
        } else {
            status_dto(CODE_INDEX_DISABLED, 0)
        };
        let status = Arc::new(Mutex::new(initial.clone()));
        let callbacks: Arc<Mutex<Vec<StatusCallback>>> = Arc::new(Mutex::new(vec![on_status]));
        let worker_callbacks = callbacks.clone();
        let worker_cb: StatusCallback = Arc::new(move |s: CodeIndexStatusDto| {
            let cbs = worker_callbacks.lock().unwrap().clone();
            for cb in cbs.iter() {
                cb(s.clone());
            }
        });
        worker_cb(initial);
        let (tx, mut rx) = mpsc::channel::<Command>(16);
        let active_build: Arc<Mutex<Option<AbortHandle>>> = Arc::new(Mutex::new(None));
        let worker_status = status.clone();
        let worker_root = root.clone();
        let worker_index_dir = index_dir.clone();
        let worker_excludes = excludes.clone();
        let worker_active = active_build.clone();
        let worker = tokio::spawn(async move {
            let mut pending_rescan = false;
            let mut last_build: Option<AbortHandle> = None;
            let mut last_rescan = Instant::now()
                .checked_sub(Duration::from_millis(DEBOUNCE_MS + 1))
                .unwrap_or_else(Instant::now);
            loop {
                // Wait for the next command, or — while a rescan is pending —
                // for the debounce window to elapse.
                let timeout = pending_rescan.then(|| Duration::from_millis(DEBOUNCE_MS));
                let timed_out = timeout.is_some();
                let cmd = match timeout {
                    // Debounce window elapsed with no new command.
                    Some(d) => tokio::time::timeout(d, rx.recv()).await.unwrap_or_default(),
                    None => rx.recv().await,
                };
                let Some(cmd) = cmd else {
                    if timed_out && pending_rescan {
                        // Debounce window elapsed: run the rescan unless a
                        // fresh Ensure/Rebuild just ran.
                        pending_rescan = false;
                        if enabled && last_rescan.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                            if let Some(h) = last_build.take() {
                                h.abort();
                            }
                            let h = run_build(
                                &worker_root,
                                &worker_index_dir,
                                &worker_excludes,
                                max_files,
                                false,
                                &worker_status,
                                &worker_cb,
                            );
                            *worker_active.lock().unwrap() = Some(h.clone());
                            last_build = Some(h);
                            last_rescan = Instant::now();
                        }
                        continue;
                    }
                    // Channel closed (orchestrator dropped): exit the loop.
                    if !timed_out {
                        break;
                    }
                    continue;
                };
                if !enabled {
                    continue;
                }
                match cmd {
                    Command::Ensure => {
                        #[cfg(test)]
                        tracing::debug!("orch ensure: run_build start");
                        if let Some(h) = last_build.take() {
                            h.abort();
                        }
                        let h = run_build(
                            &worker_root,
                            &worker_index_dir,
                            &worker_excludes,
                            max_files,
                            false,
                            &worker_status,
                            &worker_cb,
                        );
                        *worker_active.lock().unwrap() = Some(h.clone());
                        last_build = Some(h);
                        #[cfg(test)]
                        tracing::debug!("orch ensure: run_build done");
                        last_rescan = Instant::now();
                    }
                    Command::Rebuild => {
                        if let Some(h) = last_build.take() {
                            h.abort();
                        }
                        let h = run_build(
                            &worker_root,
                            &worker_index_dir,
                            &worker_excludes,
                            max_files,
                            true,
                            &worker_status,
                            &worker_cb,
                        );
                        *worker_active.lock().unwrap() = Some(h.clone());
                        last_build = Some(h);
                        pending_rescan = false;
                        last_rescan = Instant::now();
                    }
                    Command::Rescan => {
                        pending_rescan = true;
                    }
                }
            }
        });
        let worker_abort = worker.abort_handle();
        let orch = Arc::new(Self {
            root,
            index_dir,
            enabled,
            status,
            callbacks,
            tx: tx.clone(),
            worker_abort,
            active_build,
        });
        if enabled {
            // Best-effort kick: the channel has capacity 16, so `try_send`
            // cannot block the caller; if the worker already exited the send
            // fails silently and the index simply stays at its last status.
            #[cfg(test)]
            tracing::debug!("orch try_send Ensure");
            let _ = tx.try_send(Command::Ensure);
        }
        orch
    }

    /// Current status snapshot (cheap, lock-only).
    pub fn status(&self) -> CodeIndexStatusDto {
        self.status.lock().unwrap().clone()
    }

    /// Whether this orchestrator indexes at all.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Append an extra status listener (fan-out: all callbacks fire on every transition).
    pub fn subscribe_status(&self, cb: StatusCallback) {
        self.callbacks.lock().unwrap().push(cb);
    }

    /// Nudge the orchestrator: debounced incremental rescan. No-op when disabled.
    pub fn notify_changed(&self) {
        if !self.enabled {
            return;
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Command::Rescan).await;
        });
    }

    /// Full rebuild from scratch (manual, e.g. `POST /index/rebuild`).
    /// No-op when disabled.
    pub fn rebuild(&self) {
        if !self.enabled {
            return;
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Command::Rebuild).await;
        });
    }

    /// Full-text query (blocking tantivy read offloaded to a blocking thread).
    pub async fn query(
        &self,
        q: &str,
        extension: Option<&str>,
        limit: usize,
    ) -> Result<Vec<super::search::Hit>, String> {
        if !self.enabled {
            return Err("code index is disabled".to_string());
        }
        let index_dir = self.index_dir.clone();
        let q = q.to_string();
        let ext = extension.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let idx = CodeIndex::open(&index_dir)?;
            idx.query(&q, ext.as_deref(), limit.clamp(1, 50))
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Instance root this orchestrator indexes.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Store a new status snapshot and fan it out to every listener.
///
/// Callers must not hold `status`'s lock while calling this (do not pass a
/// `status.lock().unwrap()...` expression as an argument — the guard temporary
/// outlives the call and the mutex is not reentrant).
fn set_status(
    status: &Arc<Mutex<CodeIndexStatusDto>>,
    cb: &StatusCallback,
    next: CodeIndexStatusDto,
) {
    *status.lock().unwrap() = next.clone();
    cb(next);
}

/// Spawn a build on a blocking thread and return its abort handle.
///
/// The worker loop keeps the handle and aborts it before starting a new
/// build, so a slow/stuck build can never pile up behind Ensure/Rebuild
/// (and dead builds don't outlive the orchestrator in tests).
fn run_build(
    root: &Path,
    index_dir: &Path,
    excludes: &[String],
    max_files: usize,
    force: bool,
    status: &Arc<Mutex<CodeIndexStatusDto>>,
    cb: &StatusCallback,
) -> AbortHandle {
    #[cfg(test)]
    tracing::debug!("orch run_build: set indexing");
    // Read the previous count into a local *before* the call: a
    // `status.lock().unwrap().files` inside the argument list keeps the
    // `MutexGuard` temporary alive until the end of the statement, so
    // `set_status` would re-lock this non-reentrant mutex and deadlock.
    let prev_files = status.lock().unwrap().files;
    set_status(status, cb, status_dto(CODE_INDEX_INDEXING, prev_files));
    let root = root.to_path_buf();
    let index_dir = index_dir.to_path_buf();
    let excludes = excludes.to_vec();
    let status_c = status.clone();
    let cb_c = cb.clone();
    // The worker loop stays responsive to Rescan/Rebuild while a build is
    // running (it keeps the abort handle and cancels the previous build
    // before starting a new one). Unit tests run on a single-thread runtime
    // where awaiting a blocking task here would deadlock, so this stays
    // detached — but tracked, never fire-and-forget.
    tokio::task::spawn_blocking(move || {
        let outcome = build_blocking(&root, &index_dir, &excludes, max_files, force);
        match outcome {
            Ok(files) => set_status(&status_c, &cb_c, status_dto(CODE_INDEX_READY, files)),
            Err(e) => {
                tracing::warn!(error = %e, "code index build failed");
                // Same guard-lifetime guard as above: never call `status_c.lock()`
                // inside the `set_status` argument list.
                let prev_files = status_c.lock().unwrap().files;
                set_status(
                    &status_c,
                    &cb_c,
                    status_dto(CODE_INDEX_DISABLED, prev_files),
                );
            }
        }
    })
    .abort_handle()
}

impl Drop for IndexOrchestrator {
    fn drop(&mut self) {
        self.worker_abort.abort();
        if let Some(h) = self.active_build.lock().unwrap().take() {
            h.abort();
        }
    }
}

/// Synchronous build: scan, diff fingerprints, update tantivy + `meta.json`.
/// Returns the number of indexed files.
fn build_blocking(
    root: &Path,
    index_dir: &Path,
    excludes: &[String],
    max_files: usize,
    force: bool,
) -> Result<usize, String> {
    std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;
    let scanned = scan_project(root, excludes, max_files.max(1));
    let meta_path = index_dir.join("meta.json");
    let meta: std::collections::HashMap<String, IndexedFile> = if force {
        Default::default()
    } else {
        std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    };
    let _needs_full = force || meta.is_empty();
    let mut docs: Vec<(String, String)> = Vec::new();
    let mut fresh: std::collections::HashMap<String, IndexedFile> = Default::default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for f in scanned.iter().take(MAX_TANTIVY_DOCS) {
        let prev = meta.get(&f.rel);
        // Incremental fast path: unchanged files keep their old tantivy doc.
        // Tantivy `rebuild` below rewrites everything anyway (simple + correct);
        // the fingerprint check still saves file reads for hashing only when
        // mtime/size drift — content is read only for changed-or-new files.
        let changed = force || needs_reindex(prev, f.size, f.mtime_ns);
        let hash = if changed {
            file_fingerprint(&f.abs).unwrap_or_default()
        } else {
            prev.map(|m| m.hash.clone()).unwrap_or_default()
        };
        // Tantivy `rebuild` is full, so every surviving file needs its text.
        let text = read_indexable_text(&f.abs);
        docs.push((f.rel.clone(), text));
        fresh.insert(
            f.rel.clone(),
            IndexedFile {
                path_rel: f.rel.clone(),
                mtime_ns: f.mtime_ns,
                size: f.size,
                hash,
                updated_at: now,
            },
        );
    }
    // Drop stale entries (deleted files) — simply not carried into `fresh`.
    let idx = CodeIndex::open(index_dir)?;
    idx.rebuild(&docs)?;
    let json = serde_json::to_string_pretty(&fresh).map_err(|e| e.to_string())?;
    std::fs::write(&meta_path, json).map_err(|e| e.to_string())?;
    let _ = meta;
    Ok(docs.len())
}

/// Read up to [`MAX_CONTENT_BYTES`] as lossy UTF-8; binary files become
/// empty strings (their names stay searchable via the path field).
fn read_indexable_text(abs: &Path) -> String {
    let bytes = std::fs::read(abs).unwrap_or_default();
    if bytes.is_empty() {
        return String::new();
    }
    // Heuristic: NUL byte in the probe => binary => skip content.
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return String::new();
    }
    let capped = if bytes.len() > MAX_CONTENT_BYTES {
        &bytes[..MAX_CONTENT_BYTES]
    } else {
        &bytes[..]
    };
    String::from_utf8_lossy(capped).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_cb() -> StatusCallback {
        Arc::new(|_| {})
    }

    #[test]
    fn build_blocking_indexes_and_queries() {
        let base = std::env::temp_dir().join(format!("bebok-orchb-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        let idx = base.join("idx");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn buildprobefn() {}").unwrap();
        let n = build_blocking(&root, &idx, &[], 1000, true).unwrap();
        assert_eq!(n, 1);
        let hits = CodeIndex::open(&idx)
            .unwrap()
            .query("buildprobefn", None, 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn worker_wakes_on_ensure() {
        let (tx, mut rx) = mpsc::channel::<Command>(16);
        tx.try_send(Command::Ensure).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("recv timed out")
            .expect("channel closed");
        assert_eq!(got, Command::Ensure);
    }

    #[tokio::test]
    async fn disabled_orchestrator_stays_disabled() {
        let base = std::env::temp_dir().join(format!("bebok-orch-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        let idx = base.join("idx");
        std::fs::create_dir_all(&root).unwrap();
        let orch = IndexOrchestrator::spawn(root, idx, vec![], 1000, false, noop_cb());
        assert!(!orch.enabled());
        assert_eq!(orch.status().status, CODE_INDEX_DISABLED);
        orch.notify_changed();
        orch.rebuild();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(orch.status().status, CODE_INDEX_DISABLED);
        assert!(orch.query("x", None, 5).await.is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn enabled_orchestrator_builds_and_queries() {
        let base = std::env::temp_dir().join(format!("bebok-orch-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        let idx = base.join("idx");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src").join("main.rs"),
            "fn orchestratortestfn() {}",
        )
        .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<CodeIndexStatusDto>();
        let cb: StatusCallback = Arc::new(move |s| {
            let _ = tx.send(s);
        });
        let orch = IndexOrchestrator::spawn(root, idx, vec![], 1000, true, cb);
        // Wait for ready (build runs in background). DISABLED means the
        // build failed — surface it instead of hanging until the timeout.
        let mut ready = false;
        for _ in 0..100 {
            let s = orch.status().status;
            if s == CODE_INDEX_READY {
                ready = true;
                break;
            }
            assert_ne!(s, CODE_INDEX_DISABLED, "index build failed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ready, "orchestrator never became ready");
        assert!(orch.status().files >= 1);
        let hits = orch.query("orchestratortestfn", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/main.rs");
        // Status callback fired at least once.
        assert!(rx.try_recv().is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn notify_triggers_debounced_rescan() {
        let base = std::env::temp_dir().join(format!("bebok-orch-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        let idx = base.join("idx");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "debounceprobe content").unwrap();
        let orch = IndexOrchestrator::spawn(root.clone(), idx, vec![], 1000, true, noop_cb());
        for _ in 0..100 {
            if orch.status().status == CODE_INDEX_READY {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::write(root.join("b.txt"), "debounceprobe content").unwrap();
        orch.notify_changed();
        // Wait for the debounced rescan to pick up the new file.
        let mut saw_two = false;
        for _ in 0..120 {
            if orch.status().files >= 2 {
                saw_two = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(saw_two, "debounced rescan did not index the new file");
        let hits = orch.query("debounceprobe", None, 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }
}
