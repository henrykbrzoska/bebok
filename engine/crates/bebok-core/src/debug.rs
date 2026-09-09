//! Debug logger (M6): a single in-memory ring + `debug.log` file, capped at
//! [`DEBUG_LOG_MAX_CHARS`] characters. The file is cleared on startup (fresh
//! every app open) and old entries are dropped FIFO when the cap is exceeded.

use std::path::PathBuf;
use std::sync::RwLock;

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

    pub fn log(&self, source: &str, kind: &str, title: impl Into<String>, detail: impl Into<String>) {
        let entry = DebugEntry {
            ts: crate::util::now_ms(),
            source: source.to_string(),
            kind: kind.to_string(),
            title: title.into(),
            detail: detail.into(),
        };
        {
            let mut entries = self.entries.write().unwrap();
            entries.push(entry);
            trim(&mut entries);
        }
        self.flush();
    }

    pub fn entries(&self) -> Vec<DebugEntry> {
        self.entries.read().unwrap().clone()
    }

    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
        self.flush();
    }

    fn flush(&self) {
        let entries = self.entries.read().unwrap();
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
            first.detail.truncate(allowed);
        }
    }
}

fn size_of(e: &DebugEntry) -> usize {
    e.title.len() + e.detail.len() + e.source.len() + e.kind.len() + 24
}
