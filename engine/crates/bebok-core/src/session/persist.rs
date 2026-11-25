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
//!
//! SQLite mirror (`<data>/bebok/bebok.db`, see `db.rs`): every metadata and
//! message write is dual-written. The files are written first and stay
//! authoritative; the database write is best-effort and only logged on
//! failure. `import_sessions_once` backfills sessions that predate the mirror.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;
use uuid::Uuid;

use super::{Message, Role, Session};
use crate::db::{self, DbMessageRow, DbSessionRow};
use crate::util::{append_line, atomic_write, hash_dir, now_ms};

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

// ---------------------------------------------------------------------------
// SQLite mirror plumbing
// ---------------------------------------------------------------------------

/// Where the mirror of one session directory lives.
struct MirrorTarget {
    db_path: PathBuf,
    instance_hash: String,
    session_id: String,
}

/// Map `<root>/instances/<hash>/sessions/<id>` to its database and keys.
/// Returns `None` for directories outside that layout (no mirror is written).
fn mirror_target(session_dir: &Path) -> Option<MirrorTarget> {
    let session_id = session_dir.file_name()?.to_str()?.to_string();
    let sessions = session_dir.parent()?;
    if sessions.file_name()? != "sessions" {
        return None;
    }
    let instance = sessions.parent()?;
    let instance_hash = instance.file_name()?.to_str()?.to_string();
    let instances = instance.parent()?;
    if instances.file_name()? != "instances" {
        return None;
    }
    let root = instances.parent()?;
    Some(MirrorTarget {
        db_path: root.join("bebok.db"),
        instance_hash,
        session_id,
    })
}

/// Run `f` on the cached connection for `db_path`. The database is opened
/// and migrated once per process; the cache lock serialises writers, so `f`
/// must stay short and must not call `with_conn` itself.
pub(crate) fn with_conn<T>(
    db_path: &Path,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    static CONNS: OnceLock<Mutex<HashMap<PathBuf, Connection>>> = OnceLock::new();
    let cache = CONNS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .map_err(|_| "bebok.db connection cache poisoned".to_string())?;
    if !guard.contains_key(db_path) {
        let conn = db::open_db_at(db_path)?;
        guard.insert(db_path.to_path_buf(), conn);
    }
    let conn = guard
        .get(db_path)
        .ok_or_else(|| "bebok.db connection missing".to_string())?;
    f(conn)
}

/// Best-effort mirror write: off the async runtime, failures only logged.
async fn mirror<F>(db_path: PathBuf, what: &'static str, write: F)
where
    F: FnOnce(&Connection) -> Result<(), String> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || with_conn(&db_path, write)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("bebok.db mirror: {what} failed: {e}"),
        Err(e) => tracing::warn!("bebok.db mirror: {what} task failed: {e}"),
    }
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn message_row(session_id: &str, index: usize, message: &Message) -> Result<DbMessageRow, String> {
    Ok(DbMessageRow {
        session_id: session_id.to_string(),
        idx: index as i64,
        created_at: message.meta.created_at,
        role: role_str(&message.role).to_string(),
        message_json: serde_json::to_string(message).map_err(|e| e.to_string())?,
        text_extract: message.text_content(),
    })
}

fn session_row(instance_hash: &str, session: &Session) -> Result<DbSessionRow, String> {
    Ok(DbSessionRow {
        id: session.id.to_string(),
        instance_hash: instance_hash.to_string(),
        directory: session.directory.clone(),
        agent: session.agent.clone(),
        title: session
            .title
            .clone()
            .or_else(|| session.alias.clone())
            .unwrap_or_default(),
        created_at: session.created_at,
        updated_at: session.updated_at,
        parent_id: session.parent.map(|(id, _)| id.to_string()),
        parent_index: session.parent.map(|(_, idx)| idx as i64),
        meta_json: Some(serde_json::to_string(session).map_err(|e| e.to_string())?),
        deleted_at: None,
    })
}

// ---------------------------------------------------------------------------
// Writes (file first, then the mirror)
// ---------------------------------------------------------------------------

/// Write one message file atomically (1 file = 1 message, all parts), then
/// mirror it into `messages`.
pub async fn persist_message(dir: &Path, index: usize, message: &Message) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(message)?;
    atomic_write(&message_path(dir, index), &bytes).await?;
    if let Some(target) = mirror_target(dir) {
        match message_row(&target.session_id, index, message) {
            Ok(row) => {
                mirror(target.db_path, "message", move |c| {
                    db::insert_message(c, &row)
                })
                .await
            }
            Err(e) => tracing::warn!("bebok.db mirror: message {index} not encoded: {e}"),
        }
    }
    Ok(())
}

/// Write session metadata atomically, then mirror it into `sessions`.
pub async fn persist_session_meta(dir: &Path, session: &Session) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(session)?;
    atomic_write(&session_meta_path(dir), &bytes).await?;
    if let Some(target) = mirror_target(dir) {
        match session_row(&target.instance_hash, session) {
            Ok(row) => {
                mirror(target.db_path, "session", move |c| {
                    db::upsert_session(c, &row)
                })
                .await
            }
            Err(e) => tracing::warn!("bebok.db mirror: session meta not encoded: {e}"),
        }
    }
    Ok(())
}

/// Mirror of `truncate_session`: drop message rows with index `>= keep`.
pub async fn mirror_truncate(dir: &Path, keep: usize) {
    let Some(target) = mirror_target(dir) else {
        return;
    };
    let id = target.session_id;
    mirror(target.db_path, "truncate", move |c| {
        db::delete_messages_from(c, &id, keep as i64).map(|_| ())
    })
    .await;
}

/// Mirror of `delete_session`: soft-delete the row (`deleted_at`).
pub async fn mirror_session_deleted(dir: &Path, at: i64) {
    let Some(target) = mirror_target(dir) else {
        return;
    };
    let id = target.session_id;
    mirror(target.db_path, "delete", move |c| {
        db::mark_session_deleted(c, &id, at).map(|_| ())
    })
    .await;
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

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Load one message file (used by the transcript endpoint and repair).
pub fn load_message(path: &Path) -> Option<Message> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `msg-*.json` files of a session in index order, stopping at the first gap
/// (the same rule `InstanceStore::open_session` uses).
fn load_transcript_files(dir: &Path) -> Vec<Message> {
    let mut messages = Vec::new();
    while let Some(m) = load_message(&message_path(dir, messages.len())) {
        messages.push(m);
    }
    messages
}

/// Number of consecutive `msg-*.json` files, without parsing them.
fn message_file_count(dir: &Path) -> usize {
    let mut count = 0;
    while message_path(dir, count).exists() {
        count += 1;
    }
    count
}

/// Transcript of one session directory, DB-first with a files fallback.
///
/// The mirror is used only when its row count equals the number of message
/// files. A mirror write that failed leaves a gap, and serving the shorter
/// DB transcript would silently lose messages; the files then answer.
pub fn load_transcript_merged(dir: &Path) -> Vec<Message> {
    if let Some(target) = mirror_target(dir) {
        let id = target.session_id;
        let rows = with_conn(&target.db_path, |c| db::load_transcript_db(c, &id));
        if let Ok(rows) = rows
            && !rows.is_empty()
            && rows.len() == message_file_count(dir)
        {
            let parsed: Option<Vec<Message>> = rows
                .iter()
                .map(|r| serde_json::from_str(&r.message_json).ok())
                .collect();
            if let Some(messages) = parsed {
                return messages;
            }
        }
    }
    load_transcript_files(dir)
}

/// True once `import_sessions_once` has completed for `root`.
pub fn sessions_import_done(root: &Path) -> bool {
    with_conn(&root.join("bebok.db"), db::import_done).unwrap_or(false)
}

/// Live session metadata from the mirror, in id order. Soft-deleted rows and
/// the import sentinel (`__import__`, whose JSON is not a `Session`) are skipped.
pub fn load_live_sessions(root: &Path) -> Result<Vec<Session>, String> {
    with_conn(&root.join("bebok.db"), live_session_metas)
}

fn live_session_metas(conn: &Connection) -> Result<Vec<Session>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT meta_json FROM sessions
             WHERE deleted_at IS NULL AND id <> '__import__' AND meta_json IS NOT NULL
             ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for text in rows {
        let text = text.map_err(|e| e.to_string())?;
        if let Ok(session) = serde_json::from_str::<Session>(&text) {
            out.push(session);
        }
    }
    Ok(out)
}

/// Every live session with its transcript, from the mirror. `Ok(None)` until
/// the import has completed (the mirror is incomplete before that).
pub type LiveTranscripts = Option<Vec<(Session, Vec<Message>)>>;
pub fn load_live_transcripts(root: &Path) -> Result<LiveTranscripts, String> {
    with_conn(&root.join("bebok.db"), |conn| {
        if !db::import_done(conn)? {
            return Ok(None);
        }
        let mut out = Vec::new();
        for session in live_session_metas(conn)? {
            let rows = db::load_transcript_db(conn, &session.id.to_string())?;
            let messages = rows
                .iter()
                .filter_map(|r| serde_json::from_str::<Message>(&r.message_json).ok())
                .collect();
            out.push((session, messages));
        }
        Ok(Some(out))
    })
}

// ---------------------------------------------------------------------------
// One-shot import
// ---------------------------------------------------------------------------

/// Import every JSON session under `<root>/instances` into the mirror, once.
///
/// Idempotent: returns immediately once the sentinel row exists. Each session
/// is written in its own transaction (upserts plus removal of rows past the
/// file transcript), so an interrupted import repeats harmlessly. The
/// sentinel is set only when every session was written; otherwise the next
/// start retries. Returns `Ok(true)` when the import is complete.
pub fn import_sessions_once(root: &Path) -> Result<bool, String> {
    with_conn(&root.join("bebok.db"), |conn| {
        if db::import_done(conn)? {
            return Ok(true);
        }
        let mut imported = 0usize;
        let mut failed = 0usize;
        for (instance_hash, dir) in session_dirs(root) {
            match import_one(conn, &instance_hash, &dir) {
                Ok(true) => imported += 1,
                Ok(false) => {}
                Err(e) => {
                    failed += 1;
                    tracing::warn!("bebok.db import: {} failed: {e}", dir.display());
                }
            }
        }
        if failed > 0 {
            return Ok(false);
        }
        db::set_import_done(conn, now_ms())?;
        tracing::info!("imported {imported} session(s) into bebok.db");
        Ok(true)
    })
}

/// Counters of a forced session import (the "Import to DB" button).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SessionImportStats {
    pub scanned: usize,
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Re-import every session directory into `bebok.db`, ignoring the one-shot
/// sentinel (upserts are idempotent). Marks the import done when nothing failed.
pub fn import_all_sessions(root: &Path) -> Result<SessionImportStats, String> {
    with_conn(&root.join("bebok.db"), |conn| {
        let mut stats = SessionImportStats::default();
        for (instance_hash, dir) in session_dirs(root) {
            stats.scanned += 1;
            match import_one(conn, &instance_hash, &dir) {
                Ok(true) => stats.imported += 1,
                Ok(false) => stats.skipped += 1,
                Err(e) => {
                    stats.failed += 1;
                    if stats.errors.len() < 20 {
                        stats.errors.push(format!("{}: {e}", dir.display()));
                    }
                }
            }
        }
        if stats.failed == 0 {
            db::set_import_done(conn, now_ms())?;
        }
        Ok(stats)
    })
}

/// Distinct project directories of the live sessions stored in `bebok.db`.
pub fn session_directories(root: &Path) -> Result<Vec<String>, String> {
    with_conn(&root.join("bebok.db"), |conn| {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT directory FROM sessions \
                 WHERE directory <> '' AND deleted_at IS NULL ORDER BY directory",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        Ok(rows.flatten().collect())
    })
}

/// Every `<root>/instances/<hash>/sessions/<id>` directory, with its hash.
fn session_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(instances) = std::fs::read_dir(root.join("instances")) else {
        return out;
    };
    for inst in instances.flatten() {
        let hash = inst.file_name().to_string_lossy().into_owned();
        let Ok(sessions) = std::fs::read_dir(sessions_dir(&inst.path())) else {
            continue;
        };
        for entry in sessions.flatten() {
            let dir = entry.path();
            if dir.is_dir() {
                out.push((hash.clone(), dir));
            }
        }
    }
    out
}

/// Import one session directory. `Ok(false)` when its metadata is unreadable
/// (skipped, as `repair` does); `Err` only for database failures.
fn import_one(conn: &Connection, instance_hash: &str, dir: &Path) -> Result<bool, String> {
    let Some(session) = std::fs::read_to_string(session_meta_path(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Session>(&t).ok())
    else {
        return Ok(false);
    };
    let messages = load_transcript_files(dir);
    let id = session.id.to_string();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    db::upsert_session(&tx, &session_row(instance_hash, &session)?)?;
    for (index, message) in messages.iter().enumerate() {
        db::insert_message(&tx, &message_row(&id, index, message)?)?;
    }
    db::delete_messages_from(&tx, &id, messages.len() as i64)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Repair
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Part;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("bebok-persist-{}", Uuid::new_v4()))
    }

    /// A session directory inside the real layout, plus its session.
    fn new_session(root: &Path, directory: &str) -> (Session, PathBuf) {
        let session = Session::new(directory, "code");
        let dir = session_dir(&instance_dir(root, directory), session.id);
        std::fs::create_dir_all(&dir).unwrap();
        (session, dir)
    }

    fn db_path(root: &Path) -> PathBuf {
        root.join("bebok.db")
    }

    fn mirror_messages(root: &Path, id: &str) -> Vec<DbMessageRow> {
        with_conn(&db_path(root), |c| db::load_transcript_db(c, id)).unwrap()
    }

    fn mirror_session(root: &Path, id: &str) -> Option<DbSessionRow> {
        with_conn(&db_path(root), |c| db::get_session_meta(c, id)).unwrap()
    }

    fn assistant_text(text: &str) -> Message {
        let mut m = Message::new(Role::Assistant);
        m.parts.push(Part::Text { text: text.into() });
        m
    }

    /// Write session + messages as plain files, bypassing the mirror (what
    /// an engine from before the SQLite change left on disk).
    fn write_files_only(root: &Path, directory: &str, messages: &[Message]) -> Session {
        let (session, dir) = new_session(root, directory);
        std::fs::write(
            session_meta_path(&dir),
            serde_json::to_vec(&session).unwrap(),
        )
        .unwrap();
        for (i, m) in messages.iter().enumerate() {
            std::fs::write(message_path(&dir, i), serde_json::to_vec(m).unwrap()).unwrap();
        }
        session
    }

    #[tokio::test]
    async fn dual_write_mirrors_meta_and_messages() {
        let root = temp_root();
        let (session, dir) = new_session(&root, "/p/one");
        persist_session_meta(&dir, &session).await.unwrap();
        persist_message(&dir, 0, &Message::user("hello"))
            .await
            .unwrap();
        persist_message(&dir, 1, &assistant_text("hi there"))
            .await
            .unwrap();

        let id = session.id.to_string();
        let row = mirror_session(&root, &id).expect("session row mirrored");
        assert_eq!(row.directory, "/p/one");
        assert!(row.deleted_at.is_none());
        let rows = mirror_messages(&root, &id);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[0].text_extract, "hello");
        assert_eq!(rows[1].role, "assistant");
        assert_eq!(rows[1].text_extract, "hi there");

        // Mirror and files agree message for message.
        let merged = load_transcript_merged(&dir);
        let from_files = load_transcript_files(&dir);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            serde_json::to_value(&merged).unwrap(),
            serde_json::to_value(&from_files).unwrap()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn merged_prefers_db_and_falls_back_to_files() {
        let root = temp_root();
        let (session, dir) = new_session(&root, "/p/two");
        persist_session_meta(&dir, &session).await.unwrap();
        persist_message(&dir, 0, &Message::user("hello"))
            .await
            .unwrap();

        // The file is changed behind the mirror's back: the DB row still wins.
        std::fs::write(
            message_path(&dir, 0),
            serde_json::to_vec(&Message::user("tampered")).unwrap(),
        )
        .unwrap();
        let merged = load_transcript_merged(&dir);
        assert_eq!(merged[0].text_content(), "hello");

        // A session the mirror never saw is served from its files.
        let files_only = write_files_only(&root, "/p/three", &[Message::user("from disk")]);
        let dir3 = session_dir(&instance_dir(&root, "/p/three"), files_only.id);
        assert!(mirror_session(&root, &files_only.id.to_string()).is_none());
        let merged = load_transcript_merged(&dir3);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text_content(), "from disk");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn merged_ignores_mirror_with_a_gap() {
        let root = temp_root();
        let (session, dir) = new_session(&root, "/p/gap");
        persist_session_meta(&dir, &session).await.unwrap();
        persist_message(&dir, 0, &Message::user("first"))
            .await
            .unwrap();
        // A file-only message 1 the mirror never received (failed write).
        std::fs::write(
            message_path(&dir, 1),
            serde_json::to_vec(&Message::user("second")).unwrap(),
        )
        .unwrap();
        let merged = load_transcript_merged(&dir);
        assert_eq!(merged.len(), 2, "a short DB transcript must not win");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_is_idempotent_and_runs_once() {
        let root = temp_root();
        let a = write_files_only(&root, "/p/a", &[Message::user("a1"), assistant_text("a2")]);
        let b = write_files_only(&root, "/p/b", &[Message::user("b1")]);

        assert!(import_sessions_once(&root).unwrap());
        assert!(sessions_import_done(&root));
        for (session, expected) in [(&a, 2usize), (&b, 1usize)] {
            let id = session.id.to_string();
            assert!(mirror_session(&root, &id).is_some());
            assert_eq!(mirror_messages(&root, &id).len(), expected);
        }

        // A second run changes nothing.
        assert!(import_sessions_once(&root).unwrap());
        assert_eq!(mirror_messages(&root, &a.id.to_string()).len(), 2);

        // After the sentinel, files written outside the mirror are not picked up.
        let late = write_files_only(&root, "/p/late", &[Message::user("late")]);
        assert!(import_sessions_once(&root).unwrap());
        assert!(mirror_session(&root, &late.id.to_string()).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn import_drops_mirror_rows_past_the_file_transcript() {
        let root = temp_root();
        let (session, dir) = new_session(&root, "/p/tail");
        persist_session_meta(&dir, &session).await.unwrap();
        for i in 0..3 {
            persist_message(&dir, i, &Message::user(format!("m{i}")))
                .await
                .unwrap();
        }
        // Truncated on disk without the mirror seeing it.
        std::fs::remove_file(message_path(&dir, 1)).unwrap();
        std::fs::remove_file(message_path(&dir, 2)).unwrap();
        let id = session.id.to_string();
        assert_eq!(mirror_messages(&root, &id).len(), 3);

        assert!(import_sessions_once(&root).unwrap());
        assert_eq!(mirror_messages(&root, &id).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mirror_target_requires_the_instance_layout() {
        let root = temp_root();
        let inst = instance_dir(&root, "/p/x");
        let dir = session_dir(&inst, Uuid::new_v4());
        let target = mirror_target(&dir).expect("layout");
        assert_eq!(target.db_path, root.join("bebok.db"));
        assert_eq!(
            target.session_id,
            dir.file_name().unwrap().to_str().unwrap()
        );
        assert!(mirror_target(Path::new("/tmp/loose-session")).is_none());
    }
}
