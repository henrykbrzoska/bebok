//! Debug logger (M6): a single in-memory ring + `debug.log` file, capped at
//! [`DEBUG_LOG_MAX_CHARS`] characters. The file is cleared on startup (fresh
//! every app open) and old entries are dropped FIFO when the cap is exceeded.

use std::path::PathBuf;
use std::sync::{PoisonError, RwLock};

use serde::Serialize;

/// Total character budget for the debug log (old entries are dropped first).
pub const DEBUG_LOG_MAX_CHARS: usize = 10_000;

/// One log line shown in the Debug tab.
#[derive(Debug, Clone, Serialize)]
pub struct DebugEntry {
    pub ts: i64,
    /// `llm` (engine -> provider) or `http` (client -> engine).
    pub source: String,
    /// `request`, `response` or `error`.
    pub kind: String,
    pub title: String,
    pub detail: String,
}

/// The single debug log. `new()` truncates the backing file; `log()` appends
/// (FIFO, capped) and rewrites the file.
pub struct DebugLog {
    entries: RwLock<Vec<DebugEntry>>,
    path: PathBuf,
}

impl DebugLog {
    pub fn new(path: PathBuf) -> Self {
        // Clear on startup.
        let _ = std::fs::write(&path, "");
        Self {
            entries: RwLock::new(Vec::new()),
            path,
        }
    }

    pub fn log(
        &self,
        source: &str,
        kind: &str,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let entry = DebugEntry {
            ts: crate::util::now_ms(),
            source: source.to_string(),
            kind: kind.to_string(),
            title: title.into(),
            detail: detail.into(),
        };
        {
            let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
            entries.push(entry);
            trim(&mut entries);
        }
        self.flush();
    }

    pub fn entries(&self) -> Vec<DebugEntry> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn clear(&self) {
        self.entries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.flush();
    }

    fn flush(&self) {
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = String::new();
        for e in entries.iter() {
            out.push_str(&format!(
                "[{}] {}:{} {}\n    {}\n",
                e.ts, e.source, e.kind, e.title, e.detail
            ));
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, out);
    }
}

/// Drop the oldest entries until the total size fits the cap.
fn trim(entries: &mut Vec<DebugEntry>) {
    let mut size: usize = entries.iter().map(size_of).sum();
    while size > DEBUG_LOG_MAX_CHARS && entries.len() > 1 {
        size -= size_of(&entries[0]);
        entries.remove(0);
    }
    // A single oversized entry: truncate its detail.
    if let Some(first) = entries.first_mut() {
        if size_of(first) > DEBUG_LOG_MAX_CHARS {
            let allowed = DEBUG_LOG_MAX_CHARS.saturating_sub(first.title.len() + 24);
            first.detail = crate::util::truncate_chars(&first.detail, allowed).to_owned();
        }
    }
}

fn size_of(e: &DebugEntry) -> usize {
    e.title.len() + e.detail.len() + e.source.len() + e.kind.len() + 24
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multibyte detail that exceeds the log cap forces the trim → truncate
    /// branch. Must not panic on a char boundary.
    #[test]
    fn trim_handles_multibyte_detail() {
        let dir = std::env::temp_dir().join(format!("bebok-debug-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let log = DebugLog::new(dir.join("debug.log"));

        // "é" is 2 bytes (U+00E9, Latin-1 Supplement). 20 000 of them = 40 000
        // bytes, well over the 10 000-char cap.
        let multibyte_detail: String = "é".repeat(20_000);

        // Log a large entry so trim truncates the detail.
        log.log("llm", "request", "test-call", &multibyte_detail);

        // Must not panic and must be readable.
        let entries = log.entries();
        assert_eq!(entries.len(), 1);
        // The detail was truncated but should still be valid UTF-8.
        assert!(
            std::str::from_utf8(entries[0].detail.as_bytes()).is_ok(),
            "truncated detail is not valid UTF-8"
        );
        // The detail must be shorter than what we put in.
        assert!(entries[0].detail.len() < multibyte_detail.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Emoji (4-byte UTF-8) detail also truncates cleanly.
    #[test]
    fn trim_handles_emoji_detail() {
        let dir = std::env::temp_dir().join(format!("bebok-debug-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let log = DebugLog::new(dir.join("debug.log"));

        // 🦀 is 4 bytes. 6 000 of them = 24 000 bytes > 10 000 cap.
        let emoji_detail: String = "🦀".repeat(6_000);

        log.log("http", "response", "emoji-test", &emoji_detail);

        let entries = log.entries();
        assert_eq!(entries.len(), 1);
        assert!(
            std::str::from_utf8(entries[0].detail.as_bytes()).is_ok(),
            "truncated emoji detail is not valid UTF-8"
        );
        assert!(entries[0].detail.len() < emoji_detail.len());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
