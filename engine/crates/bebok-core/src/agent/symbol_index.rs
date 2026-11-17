//! In-memory project symbol index for editor completion (`POST /fs/complete`).
//!
//! `goto_definition::find_symbols` re-parses the whole project on every call,
//! which is fine for Ctrl+Shift+F but too slow per keystroke. This keeps
//! `rel path → (mtime, size, symbols)` per instance:
//!
//! - built lazily on first use; a file is re-parsed only when its mtime or
//!   size changed, so agent writes and external edits are picked up on the
//!   next refresh;
//! - `refresh` walks the tree at most once per [`REFRESH_EVERY`] so that
//!   keystroke-driven requests do not stat the whole project each time;
//! - `invalidate` drops one file (called after an editor save) and forces the
//!   next refresh to walk again.
//!
//! Respects `.gitignore` and the same file cap as the other symbol searches.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::goto_definition::{self, SymbolHit};

/// Minimum time between two project walks.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// Same cap as `find_symbols` / `find_definitions`.
const MAX_FILES: usize = 20_000;

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Clone)]
struct Entry {
    stamp: Stamp,
    hits: Arc<Vec<SymbolHit>>,
}

#[derive(Default)]
struct State {
    files: HashMap<String, Entry>,
    refreshed_at: Option<Instant>,
}

/// Process-held per instance (`Instance::symbols`).
#[derive(Default)]
pub struct SymbolIndex {
    state: Mutex<State>,
}

impl std::fmt::Debug for SymbolIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolIndex").finish_non_exhaustive()
    }
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bring the index up to date with the files under `root`.
    ///
    /// Blocking (walks and parses); call from `spawn_blocking`. A no-op when
    /// the last walk happened less than [`REFRESH_EVERY`] ago.
    pub fn refresh(&self, root: &Path) {
        let known = {
            let st = self.state.lock().unwrap();
            if st.refreshed_at.is_some_and(|t| t.elapsed() < REFRESH_EVERY) {
                return;
            }
            st.files.clone()
        };

        let (files, _) =
            goto_definition::source_files(root, &goto_definition::source_exts(), MAX_FILES);
        let mut next: HashMap<String, Entry> = HashMap::with_capacity(files.len());
        for (path, rel) in &files {
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let stamp = Stamp {
                modified: meta.modified().ok(),
                len: meta.len(),
            };
            let hits = match known.get(rel) {
                Some(old) if old.stamp == stamp => old.hits.clone(),
                _ => {
                    let Ok(content) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    Arc::new(goto_definition::symbols_in_file(rel, &content))
                }
            };
            next.insert(rel.clone(), Entry { stamp, hits });
        }

        let mut st = self.state.lock().unwrap();
        st.files = next;
        st.refreshed_at = Some(Instant::now());
    }

    /// Forget one project-relative file; it is re-parsed on the next refresh.
    pub fn invalidate(&self, rel: &str) {
        let mut st = self.state.lock().unwrap();
        st.files.remove(rel);
        st.refreshed_at = None;
    }

    /// Indexed symbols whose name starts with `prefix` (case-insensitive),
    /// skipping the file `skip_rel` (its buffer is answered separately).
    /// Shorter names first, then name, path and line.
    pub fn starts_with(
        &self,
        prefix: &str,
        skip_rel: Option<&str>,
        limit: usize,
    ) -> Vec<SymbolHit> {
        let needle = prefix.to_lowercase();
        let st = self.state.lock().unwrap();
        let mut out: Vec<SymbolHit> = Vec::new();
        for (rel, entry) in &st.files {
            if skip_rel == Some(rel.as_str()) {
                continue;
            }
            out.extend(
                entry
                    .hits
                    .iter()
                    .filter(|h| h.name.to_lowercase().starts_with(&needle))
                    .cloned(),
            );
        }
        drop(st);
        out.sort_by(|a, b| {
            a.name
                .len()
                .cmp(&b.name.len())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.path.cmp(&b.path))
                .then(a.line.cmp(&b.line))
        });
        out.truncate(limit);
        out
    }

    /// Indexed symbols whose name contains `query` as a case-insensitive
    /// subsequence (Explorer "Go to Class / Symbol": `FiF` finds
    /// `FindInFiles`). Ranked exact → prefix → substring → subsequence, then
    /// shorter names, name, path and line; the client re-scores the list.
    pub fn fuzzy(&self, query: &str, limit: usize) -> Vec<SymbolHit> {
        let needle: Vec<char> = query
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '*')
            .flat_map(char::to_lowercase)
            .collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let needle_str: String = needle.iter().collect();
        let tier = |name: &str| -> Option<u8> {
            let lower = name.to_lowercase();
            if lower == needle_str {
                Some(0)
            } else if lower.starts_with(&needle_str) {
                Some(1)
            } else if lower.contains(&needle_str) {
                Some(2)
            } else {
                let mut it = needle.iter().peekable();
                for c in lower.chars() {
                    if it.peek() == Some(&&c) {
                        it.next();
                    }
                }
                it.peek().is_none().then_some(3)
            }
        };
        let st = self.state.lock().unwrap();
        let mut out: Vec<(u8, SymbolHit)> = Vec::new();
        for entry in st.files.values() {
            for h in entry.hits.iter() {
                if let Some(t) = tier(&h.name) {
                    out.push((t, h.clone()));
                }
            }
        }
        drop(st);
        out.sort_by(|(ta, a), (tb, b)| {
            ta.cmp(tb)
                .then(a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.path.cmp(&b.path))
                .then(a.line.cmp(&b.line))
        });
        out.truncate(limit);
        out.into_iter().map(|(_, h)| h).collect()
    }

    /// Number of indexed files (diagnostics / tests).
    pub fn file_count(&self) -> usize {
        self.state.lock().unwrap().files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_camel_humps_and_ranks_prefix_first() {
        let root = std::env::temp_dir().join(format!("bebok-symidx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("lib.rs"),
            "pub struct FindInFiles;\npub struct Fif;\npub fn unrelated() {}\n",
        )
        .unwrap();
        let idx = SymbolIndex::new();
        idx.refresh(&root);
        let names: Vec<String> = idx.fuzzy("fif", 10).into_iter().map(|h| h.name).collect();
        assert_eq!(names, vec!["Fif".to_string(), "FindInFiles".to_string()]);
        assert!(idx.fuzzy("zzz", 10).is_empty());
        assert!(idx.fuzzy("  ", 10).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
