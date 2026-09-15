//! Filesystem watcher that nudges the code index on changes.

use std::path::PathBuf;
use std::sync::Arc;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::scan::scan_project;
use super::schema::is_excluded;

/// Snapshot pushed to the watcher callback.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    pub state: String,
    pub files: usize,
}

/// Watch `root` recursively; on create/modify/remove events rescan (capped)
/// and invoke `callback`. No debounce, no tokio.
pub fn spawn_watcher(
    root: PathBuf,
    callback: impl Fn(IndexStatus) + Send + Sync + 'static,
) -> Result<RecommendedWatcher, notify::Error> {
    let callback = Arc::new(callback);
    let scan_root = root.clone();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            let relevant = match res {
                Ok(ev) => matches!(
                    ev.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ),
                Err(_) => false,
            };
            if !relevant {
                return;
            }
            let files = scan_project(&scan_root, &[], 20_000).len();
            callback(IndexStatus {
                state: "indexing".to_string(),
                files,
            });
        })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Lightweight FS watcher wiring for the per-instance code index.
///
/// Unlike [`spawn_watcher`] (which runs a full `scan_project` synchronously
/// in the notify callback on every event), this only signals: on a relevant
/// create/modify/modify-name/remove event whose paths are not all excluded
/// (`DEFAULT_EXCLUDES` + `user_excludes`), it calls `on_change` — the caller
/// (typically `Instance::notify_code_index_changed`) debounces into the
/// orchestrator's background rescan.
///
/// The callback runs on the `notify` worker thread, so it must be cheap and
/// non-blocking (no scans, no locks that a build might hold). The returned
/// watcher must be kept alive for as long as events should flow; dropping it
/// stops the watch (graceful shutdown = drop). Construction failure is a
/// plain `notify::Error` for the caller to log — it must never fail instance
/// creation or a turn.
pub fn spawn_index_watcher(
    root: PathBuf,
    user_excludes: Vec<String>,
    on_change: impl Fn(Option<String>) + Send + Sync + 'static,
) -> Result<RecommendedWatcher, notify::Error> {
    let scan_root = root.clone();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            let Ok(ev) = res else { return };
            if !matches!(
                ev.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            // Relative, slash-normalised path of the first non-excluded path in
            // the event (`None` = all excluded or unresolvable => skip). Rename
            // events carry both sides; either side matching is enough to rescan.
            let rel = ev
                .paths
                .iter()
                .filter_map(|p| p.strip_prefix(&scan_root).ok())
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .filter(|rel| !rel.is_empty())
                .find(|rel| !is_excluded(rel, &user_excludes));
            let Some(rel) = rel else { return };
            on_change(Some(rel));
        })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The watcher signals a new file with its relative path and stays
    /// quiet for excluded dirs (no scan runs in the callback).
    #[test]
    fn index_watcher_signals_on_write_and_ignores_excludes() {
        let base = std::env::temp_dir().join(format!("bebok-idxwatch-{}", uuid::Uuid::new_v4()));
        let root = base.join("proj");
        std::fs::create_dir_all(root.join("target")).unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        let _watcher = spawn_index_watcher(root.clone(), vec![], move |rel| {
            let _ = tx.send(rel.unwrap_or_default());
        })
        .unwrap();

        std::fs::write(root.join("src_main.rs"), "fn probe() {}").unwrap();
        let got = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("watcher must signal a new file");
        assert!(got.contains("src_main.rs"), "got {got}");
        // Drain duplicates of the same save burst before the quiet assertion.
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}

        std::fs::write(root.join("target").join("a"), "x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_secs(1)).is_err(),
            "excluded path must not signal"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
