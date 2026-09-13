//! `GET /stats?directory=&from=&to=` (F7-5): usage statistics over every
//! persisted session.
//!
//! The expensive part (walking `<data>/instances/*/sessions/*` and reading
//! every message file) runs at most once between session events: the digest
//! corpus is kept in a process-wide cache that a bus subscriber marks dirty
//! on `session.*` / `message.part.updated`. Filters (project, time range) are
//! applied to the cached digests per request, so switching 7d/30d/all or
//! all/current in the client never rescans the disk.
//!
//! Token-protected like every other route (`crate::auth::require_token`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::event::EventBus;
use bebok_core::stats::{self, SessionDigest, StatsFilter};
use bebok_core::util::{normalize_path, now_ms};

use crate::error::ApiError;
use crate::state::AppState;

/// `GET /stats` query. All fields optional: no `directory` = every project,
/// no bounds = all time. `from` / `to` accept epoch milliseconds,
/// `YYYY-MM-DD` or RFC 3339.
#[derive(Deserialize)]
pub struct StatsQuery {
    pub directory: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Process-wide digest cache (one engine = one data directory in practice;
/// the entry remembers which directory it was scanned from so a test store
/// rooted elsewhere never reads a stale corpus).
struct StatsCache {
    dirty: AtomicBool,
    corpus: tokio::sync::Mutex<Option<(PathBuf, Arc<Vec<SessionDigest>>)>>,
    watcher: OnceLock<()>,
}

static CACHE: LazyLock<StatsCache> = LazyLock::new(|| StatsCache {
    dirty: AtomicBool::new(true),
    corpus: tokio::sync::Mutex::new(None),
    watcher: OnceLock::new(),
});

impl StatsCache {
    /// Spawn the invalidation subscriber once. Lazy (first request) rather
    /// than in `server.rs` so the feature stays a single route registration.
    fn ensure_watching(&self, bus: EventBus) {
        self.watcher.get_or_init(|| {
            let mut rx = bus.subscribe();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            if invalidates(&ev.kind) {
                                CACHE.dirty.store(true, Ordering::Release);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Missed events may include a session change.
                            CACHE.dirty.store(true, Ordering::Release);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        });
    }

    /// Current digest corpus for `data_dir`, rescanning when dirty.
    async fn corpus(&self, data_dir: PathBuf) -> Arc<Vec<SessionDigest>> {
        // The lock serialises scans: a second request arriving mid-scan
        // waits and then reuses the fresh corpus instead of scanning again.
        let mut guard = self.corpus.lock().await;
        let dirty = self.dirty.swap(false, Ordering::AcqRel);
        if !dirty
            && let Some((dir, corpus)) = guard.as_ref()
            && *dir == data_dir
        {
            return corpus.clone();
        }
        let scan_dir = data_dir.clone();
        let scanned = tokio::task::spawn_blocking(move || stats::scan(&scan_dir))
            .await
            .unwrap_or_default();
        let corpus = Arc::new(scanned);
        *guard = Some((data_dir, corpus.clone()));
        corpus
    }
}

/// Which bus events can change the aggregates.
fn invalidates(kind: &str) -> bool {
    kind.starts_with("session.") || kind == "message.part.updated" || kind.starts_with("task.")
}

fn parse_bound(label: &str, value: Option<&str>) -> Result<Option<i64>, ApiError> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => stats::parse_timestamp(v)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request(format!("invalid `{label}` timestamp: {v}"))),
    }
}

/// `GET /stats` -> `bebok_core::stats::Stats` as JSON.
pub async fn get_stats(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let from = parse_bound("from", q.from.as_deref()).map_err(IntoResponse::into_response)?;
    let to = parse_bound("to", q.to.as_deref()).map_err(IntoResponse::into_response)?;
    if let (Some(f), Some(t)) = (from, to)
        && f > t
    {
        return Err(ApiError::bad_request("`from` is after `to`").into_response());
    }
    let directory = q
        .directory
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| normalize_path(std::path::Path::new(d)));
    let filter = StatsFilter {
        directory,
        from,
        to,
    };

    CACHE.ensure_watching(state.store.bus());
    let corpus = CACHE.corpus(state.store.data_dir().to_path_buf()).await;
    let result = stats::aggregate(&corpus, &filter, now_ms());
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_message_events_invalidate_the_cache() {
        assert!(invalidates("session.created"));
        assert!(invalidates("session.updated"));
        assert!(invalidates("session.deleted"));
        assert!(invalidates("message.part.updated"));
        assert!(invalidates("task.ended"));
        assert!(!invalidates("debug.log"));
        assert!(!invalidates("config.changed"));
        assert!(!invalidates("pty.exited"));
    }

    #[test]
    fn bounds_parse_or_reject() {
        assert!(matches!(parse_bound("from", None), Ok(None)));
        assert!(matches!(parse_bound("from", Some("  ")), Ok(None)));
        assert!(matches!(
            parse_bound("from", Some("1970-01-02")),
            Ok(Some(86_400_000))
        ));
        assert!(parse_bound("to", Some("yesterday")).is_err());
    }

    #[tokio::test]
    async fn cache_rescans_only_when_dirty_or_for_another_data_dir() {
        let root = std::env::temp_dir().join(format!("bebok-stats-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let cache = StatsCache {
            dirty: AtomicBool::new(true),
            corpus: tokio::sync::Mutex::new(None),
            watcher: OnceLock::new(),
        };
        let first = cache.corpus(root.clone()).await;
        assert!(first.is_empty());
        // Same dir, not dirty: identical Arc.
        let second = cache.corpus(root.clone()).await;
        assert!(Arc::ptr_eq(&first, &second));
        // Dirty: a fresh scan (new Arc).
        cache.dirty.store(true, Ordering::Release);
        let third = cache.corpus(root.clone()).await;
        assert!(!Arc::ptr_eq(&first, &third));
        // Another data dir: fresh scan even when clean.
        let other = root.join("other");
        let fourth = cache.corpus(other).await;
        assert!(!Arc::ptr_eq(&third, &fourth));
        let _ = std::fs::remove_dir_all(&root);
    }
}
