//! Session persistence (SPEC §3.4a).
//!
//! On-disk layout:
//!
//! ```text
//! <data>/bebok/instances/<hash(cwd)>/
//! ├─ instance.json
//! └─ sessions/
//!    ├─ index.jsonl                  # append-only: 1 line = 1 session event
//!    └─ <session-id>/
//!       ├─ session.json              # metadata
//!       └─ msg-000012.json           # 1 file = 1 message (all parts)
//! ```
//!
//! All writes are atomic (tmp + rename). A repair pass at startup discards
//! stale tmp files left behind by a crash mid-write.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::{Message, Session};
use crate::util::{append_line, atomic_write, hash_dir};

/// Engine data root: `<dirs::data_dir()>/bebok`.
pub fn data_root() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("bebok"))
        .unwrap_or_else(|| PathBuf::from(".bebok-data"))
}

pub fn instance_dir(root: &Path, directory: &str) -> PathBuf {
    root.join("instances").join(hash_dir(directory))
}

pub fn sessions_dir(instance: &Path) -> PathBuf {
    instance.join("sessions")
}

pub fn session_dir(instance: &Path, id: Uuid) -> PathBuf {
    sessions_dir(instance).join(id.to_string())
}

pub fn session_meta_path(dir: &Path) -> PathBuf {
    dir.join("session.json")
}

pub fn message_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("msg-{index:06}.json"))
}

pub fn index_path(instance: &Path) -> PathBuf {
    sessions_dir(instance).join("index.jsonl")
}

/// Write one message file atomically (1 file = 1 message, all parts).
pub async fn persist_message(dir: &Path, index: usize, message: &Message) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(message)?;
    atomic_write(&message_path(dir, index), &bytes).await
}

/// Write session metadata atomically.
pub async fn persist_session_meta(dir: &Path, session: &Session) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(session)?;
    atomic_write(&session_meta_path(dir), &bytes).await
}

/// Append a session metadata event to the append-only index.
pub async fn append_index_event(instance: &Path, op: &str, session: &Session) {
    let event = serde_json::json!({
        "op": op,
        "session": session,
    });
    let Ok(line) = serde_json::to_string(&event) else {
        return;
    };
    if let Err(e) = append_line(&index_path(instance), &line).await {
        tracing::warn!("failed to append to index.jsonl: {e}");
    }
}

/// Load one message file (used by the transcript endpoint and repair).
pub fn load_message(path: &Path) -> Option<Message> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Repair pass: remove stale tmp files and collect valid session metadata.
///
/// A crash mid-write leaves `*.tmp-*` files behind (the rename never happens),
/// so they are safe to delete. `msg-*.json` files are atomic by construction;
/// unreadable ones are skipped when loading transcripts.
pub fn repair(root: &Path) -> Vec<Session> {
    let mut sessions = Vec::new();
    let instances = root.join("instances");
    let Ok(instance_entries) = std::fs::read_dir(&instances) else {
        return sessions;
    };

    for inst in instance_entries.flatten() {
        let sessions_dir = inst.path().join("sessions");
        let Ok(session_entries) = std::fs::read_dir(&sessions_dir) else {
            continue;
        };
        for entry in session_entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                // Stale tmp file in sessions dir; clean up.
                if path.extension().map(|e| e == "tmp").unwrap_or(false) {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            // Remove stale tmp files inside the session dir.
            if let Ok(files) = std::fs::read_dir(&path) {
                for f in files.flatten() {
                    let fp = f.path();
                    let is_tmp = fp
                        .file_name()
                        .map(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with('.') && n.contains(".tmp-")
                        })
                        .unwrap_or(false);
                    if is_tmp {
                        let _ = std::fs::remove_file(&fp);
                    }
                }
            }
            // Load metadata; reject invalid (crash-corrupted) sessions.
            let meta_path = session_meta_path(&path);
            if let Some(session) = std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|text| serde_json::from_str::<Session>(&text).ok())
            {
                sessions.push(session);
            }
        }
    }

    sessions
}
