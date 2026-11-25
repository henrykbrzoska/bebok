use std::path::PathBuf;

use rusqlite::Connection;

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("BEBOK_DB") {
        return PathBuf::from(p);
    }
    crate::session::persist::data_root().join("bebok.db")
}

pub fn open_db() -> Result<Connection, String> {
    open_db_at(&db_path())
}

/// Open (creating if needed) the database at `path` and apply the schema.
pub fn open_db_at(path: &std::path::Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    // Several connections (scheduler tick, HTTP routes, batch store) write to
    // the same file; wait for locks instead of failing with SQLITE_BUSY.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    init_db(&conn)?;
    Ok(conn)
}

/// In-memory database with the full schema (tests, ephemeral stores).
pub fn open_memory() -> Result<Connection, String> {
    let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    init_db(&conn)?;
    Ok(conn)
}

/// Schema version this build expects. Bump it and add a `MIGRATION_Vn` block
/// when the schema changes; never edit an applied migration.
const SCHEMA_VERSION: i64 = 2;

fn init_db(conn: &Connection) -> Result<(), String> {
    // Required for `ON DELETE CASCADE` (batch_items -> batches).
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        );",
    )
    .map_err(|e| e.to_string())?;
    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if current > SCHEMA_VERSION {
        return Err(format!(
            "bebok.db schema version {current} is newer than this build ({SCHEMA_VERSION})"
        ));
    }
    if current < 1 {
        apply_migration(conn, 1, MIGRATION_V1)?;
    }
    if current < 2 {
        apply_migration(conn, 2, MIGRATION_V2)?;
    }
    // V1 is replayed on every open: all of its statements are `IF NOT EXISTS`,
    // so this fills in tables added to an older V1 database. V2 is not
    // replayed (its statements are plain CREATEs, applied exactly once above).
    conn.execute_batch(MIGRATION_V1)
        .map_err(|e| e.to_string())?;
    conn.execute_batch(crate::config::store_ddl::MIGRATION_CONFIG)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn apply_migration(conn: &Connection, version: i64, sql: &str) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(sql).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
        rusqlite::params![version, crate::util::now_ms()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// v1: projects, explorer workspace, session digests, schedules, batches,
/// LLM call log, permission log. All statements must stay idempotent
/// (`IF NOT EXISTS`), see `init_db`.
const MIGRATION_V1: &str = r#"
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            added_at INTEGER NOT NULL,
            last_opened_at INTEGER,
            pinned INTEGER NOT NULL DEFAULT 0,
            grp TEXT
        );
        CREATE TABLE IF NOT EXISTS explorer_state (
            project_dir TEXT PRIMARY KEY,
            workspace TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS session_digests (
            session_id TEXT PRIMARY KEY,
            digest TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS schedules (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            interval_mins INTEGER,
            hour INTEGER,
            minute INTEGER,
            weekday INTEGER,
            prompt TEXT NOT NULL,
            directory TEXT NOT NULL,
            agent TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            last_run_at TEXT,
            last_status TEXT NOT NULL DEFAULT '',
            next_run_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS batches (
            id TEXT PRIMARY KEY,
            directory TEXT NOT NULL,
            session_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            state TEXT NOT NULL,
            job_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS batches_session
            ON batches (directory, session_id);
        CREATE TABLE IF NOT EXISTS batch_items (
            batch_id TEXT NOT NULL REFERENCES batches (id) ON DELETE CASCADE,
            idx INTEGER NOT NULL,
            model TEXT NOT NULL,
            messages_json TEXT NOT NULL,
            status TEXT NOT NULL,
            result TEXT,
            error TEXT,
            PRIMARY KEY (batch_id, idx)
        );
        CREATE TABLE IF NOT EXISTS llm_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            model TEXT NOT NULL,
            session_id TEXT NULL,
            status TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cost REAL NULL
        );
        CREATE INDEX IF NOT EXISTS llm_calls_session_ts
            ON llm_calls (session_id, ts);
        CREATE TABLE IF NOT EXISTS permission_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            session_id TEXT NULL,
            agent TEXT NOT NULL,
            tool TEXT NOT NULL,
            verdict TEXT NOT NULL,
            pattern TEXT NULL
        );
        CREATE INDEX IF NOT EXISTS permission_log_ts
            ON permission_log (ts);
"#;

/// v2: sessions and transcript messages in SQLite, with an FTS5 index over
/// `messages.text_extract`. The FTS table is external-content: its rows are
/// kept in sync by the three triggers below, so every write to `messages`
/// must go through SQL (never bypass the triggers). Applied exactly once,
/// inside a transaction, by `init_db`.
const MIGRATION_V2: &str = r#"
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            instance_hash TEXT NOT NULL,
            directory TEXT NOT NULL,
            agent TEXT NOT NULL,
            title TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            parent_id TEXT,
            parent_index INTEGER,
            meta_json TEXT,
            deleted_at INTEGER
        );
        CREATE INDEX sessions_directory ON sessions (directory);
        CREATE INDEX sessions_live ON sessions (directory, updated_at)
            WHERE deleted_at IS NULL;
        CREATE TABLE messages (
            session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
            idx INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            role TEXT NOT NULL,
            message_json TEXT NOT NULL,
            text_extract TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (session_id, idx)
        );
        CREATE INDEX messages_session ON messages (session_id);
        CREATE VIRTUAL TABLE messages_fts USING fts5(
            text_extract,
            content = 'messages',
            content_rowid = 'rowid'
        );
        CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts (rowid, text_extract)
            VALUES (new.rowid, new.text_extract);
        END;
        CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts (messages_fts, rowid, text_extract)
            VALUES ('delete', old.rowid, old.text_extract);
        END;
        CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts (messages_fts, rowid, text_extract)
            VALUES ('delete', old.rowid, old.text_extract);
            INSERT INTO messages_fts (rowid, text_extract)
            VALUES (new.rowid, new.text_extract);
        END;
"#;

/// Reserved `sessions.id` holding the JSON-to-SQLite import marker. It is a
/// real row so the marker survives restarts in the same table, but it is
/// never listed, searched or returned as a session.
const IMPORT_SENTINEL_ID: &str = "__import__";

/// One row of `sessions`.
#[derive(Debug, Clone, PartialEq)]
pub struct DbSessionRow {
    pub id: String,
    pub instance_hash: String,
    pub directory: String,
    pub agent: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub parent_id: Option<String>,
    pub parent_index: Option<i64>,
    pub meta_json: Option<String>,
    pub deleted_at: Option<i64>,
}

/// One row of `messages`.
#[derive(Debug, Clone, PartialEq)]
pub struct DbMessageRow {
    pub session_id: String,
    pub idx: i64,
    pub created_at: i64,
    pub role: String,
    pub message_json: String,
    pub text_extract: String,
}

/// One FTS hit: the message location plus a highlighted snippet.
#[derive(Debug, Clone, PartialEq)]
pub struct DbSearchHit {
    pub session_id: String,
    pub idx: i64,
    pub snippet: String,
}

const SESSION_COLUMNS: &str = "id, instance_hash, directory, agent, title, created_at, \
     updated_at, parent_id, parent_index, meta_json, deleted_at";

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbSessionRow> {
    Ok(DbSessionRow {
        id: row.get(0)?,
        instance_hash: row.get(1)?,
        directory: row.get(2)?,
        agent: row.get(3)?,
        title: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        parent_id: row.get(7)?,
        parent_index: row.get(8)?,
        meta_json: row.get(9)?,
        deleted_at: row.get(10)?,
    })
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbMessageRow> {
    Ok(DbMessageRow {
        session_id: row.get(0)?,
        idx: row.get(1)?,
        created_at: row.get(2)?,
        role: row.get(3)?,
        message_json: row.get(4)?,
        text_extract: row.get(5)?,
    })
}

/// Insert or update a session by id. Every column is overwritten, including
/// `deleted_at`, so callers that re-save a deleted session must pass it
/// through deliberately. Rejects the reserved import sentinel id.
pub fn upsert_session(conn: &Connection, s: &DbSessionRow) -> Result<(), String> {
    if s.id == IMPORT_SENTINEL_ID {
        return Err(format!("session id '{IMPORT_SENTINEL_ID}' is reserved"));
    }
    conn.execute(
        "INSERT INTO sessions (id, instance_hash, directory, agent, title, created_at,
             updated_at, parent_id, parent_index, meta_json, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
             instance_hash = excluded.instance_hash,
             directory = excluded.directory,
             agent = excluded.agent,
             title = excluded.title,
             created_at = excluded.created_at,
             updated_at = excluded.updated_at,
             parent_id = excluded.parent_id,
             parent_index = excluded.parent_index,
             meta_json = excluded.meta_json,
             deleted_at = excluded.deleted_at",
        rusqlite::params![
            s.id,
            s.instance_hash,
            s.directory,
            s.agent,
            s.title,
            s.created_at,
            s.updated_at,
            s.parent_id,
            s.parent_index,
            s.meta_json,
            s.deleted_at,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Insert a message, or replace the text/JSON of an existing `(session_id, idx)`.
/// Uses `ON CONFLICT DO UPDATE` rather than `INSERT OR REPLACE`: REPLACE
/// deletes the old row without firing `messages_ad`, which would leave stale
/// FTS entries behind.
pub fn insert_message(conn: &Connection, m: &DbMessageRow) -> Result<(), String> {
    conn.execute(
        "INSERT INTO messages (session_id, idx, created_at, role, message_json, text_extract)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id, idx) DO UPDATE SET
             created_at = excluded.created_at,
             role = excluded.role,
             message_json = excluded.message_json,
             text_extract = excluded.text_extract",
        rusqlite::params![
            m.session_id,
            m.idx,
            m.created_at,
            m.role,
            m.message_json,
            m.text_extract,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Delete every message of `session_id` with `idx >= from_idx` (used when a
/// turn is rewound or truncated). Returns the number of rows removed.
pub fn delete_messages_from(
    conn: &Connection,
    session_id: &str,
    from_idx: i64,
) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND idx >= ?2",
        rusqlite::params![session_id, from_idx],
    )
    .map_err(|e| e.to_string())
}

/// Soft-delete a session by stamping `deleted_at`. Messages are kept so the
/// transcript can still be exported or restored. Returns rows changed (0 when
/// the session does not exist).
pub fn mark_session_deleted(conn: &Connection, id: &str, now_ms: i64) -> Result<usize, String> {
    conn.execute(
        "UPDATE sessions SET deleted_at = ?2 WHERE id = ?1",
        rusqlite::params![id, now_ms],
    )
    .map_err(|e| e.to_string())
}

/// Fetch one session by id, including soft-deleted ones (check `deleted_at`).
pub fn get_session_meta(conn: &Connection, id: &str) -> Result<Option<DbSessionRow>, String> {
    if id == IMPORT_SENTINEL_ID {
        return Ok(None);
    }
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query_map([id], session_from_row)
        .map_err(|e| e.to_string())?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// Live (not soft-deleted) sessions of `directory`, most recently updated
/// first. Excludes the import sentinel.
pub fn list_session_metas_for_dir(
    conn: &Connection,
    directory: &str,
) -> Result<Vec<DbSessionRow>, String> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions
         WHERE directory = ?1 AND deleted_at IS NULL AND id <> ?2
         ORDER BY updated_at DESC, id"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            rusqlite::params![directory, IMPORT_SENTINEL_ID],
            session_from_row,
        )
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// All messages of one session in transcript order.
pub fn load_transcript_db(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<DbMessageRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, idx, created_at, role, message_json, text_extract
             FROM messages WHERE session_id = ?1 ORDER BY idx",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([session_id], message_from_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Full-text search over live sessions' messages, best match first.
///
/// `query` is free text: each whitespace-separated token is quoted as an FTS5
/// phrase, so punctuation cannot produce a syntax error and the tokens are
/// ANDed. An empty or blank query returns no hits.
pub fn search_messages_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<DbSearchHit>, String> {
    let match_expr = query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    if match_expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT m.session_id, m.idx,
                    snippet(messages_fts, 0, '<b>', '</b>', '...', 16)
             FROM messages_fts
             JOIN messages m ON m.rowid = messages_fts.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1 AND s.deleted_at IS NULL
             ORDER BY bm25(messages_fts)
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![match_expr, limit as i64], |row| {
            Ok(DbSearchHit {
                session_id: row.get(0)?,
                idx: row.get(1)?,
                snippet: row.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// True once the JSON-to-SQLite import has finished for this database.
pub fn import_done(conn: &Connection) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            [IMPORT_SENTINEL_ID],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// Record that the JSON-to-SQLite import completed (idempotent).
pub fn set_import_done(conn: &Connection, now_ms: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO sessions (id, instance_hash, directory, agent, title, created_at,
             updated_at, meta_json)
         VALUES (?1, '', '', '', '', ?2, ?2, '{\"done\":true}')
         ON CONFLICT(id) DO UPDATE SET meta_json = excluded.meta_json",
        rusqlite::params![IMPORT_SENTINEL_ID, now_ms],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// One row of `llm_calls`.
pub struct LlmCall<'a> {
    pub ts: i64,
    pub model: &'a str,
    pub session_id: Option<&'a str>,
    pub status: &'a str,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: Option<f64>,
}

/// Append one LLM call to `llm_calls`; returns the new row id.
pub fn insert_llm_call(conn: &Connection, call: &LlmCall<'_>) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO llm_calls (ts, model, session_id, status, input_tokens, output_tokens, cost)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            call.ts,
            call.model,
            call.session_id,
            call.status,
            call.input_tokens,
            call.output_tokens,
            call.cost,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// One row of `permission_log`.
pub struct PermissionLog<'a> {
    pub ts: i64,
    pub session_id: Option<&'a str>,
    pub agent: &'a str,
    pub tool: &'a str,
    pub verdict: &'a str,
    pub pattern: Option<&'a str>,
}

/// Append one permission decision to `permission_log`; returns the new row id.
pub fn insert_permission_log(conn: &Connection, entry: &PermissionLog<'_>) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO permission_log (ts, session_id, agent, tool, verdict, pattern)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            entry.ts,
            entry.session_id,
            entry.agent,
            entry.tool,
            entry.verdict,
            entry.pattern,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

// Explorer workspace helpers

pub fn get_workspace(project_dir: &str) -> Option<String> {
    let conn = open_db().ok()?;
    let mut stmt = conn
        .prepare("SELECT workspace FROM explorer_state WHERE project_dir = ?1")
        .ok()?;
    let val: Result<String, _> = stmt.query_row([project_dir], |row| row.get(0));
    val.ok()
}

pub fn put_workspace(project_dir: &str, json: &str) -> Result<(), String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let now = crate::util::now_ms();
    conn.execute(
        "INSERT INTO explorer_state (project_dir, workspace, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(project_dir) DO UPDATE SET workspace = excluded.workspace, updated_at = excluded.updated_at",
        rusqlite::params![project_dir, json, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    #[test]
    fn memory_db_creates_llm_and_permission_tables() {
        let conn = open_memory().unwrap();
        assert!(table_exists(&conn, "llm_calls"));
        assert!(table_exists(&conn, "permission_log"));
    }

    #[test]
    fn reopen_adds_missing_tables_to_existing_v1_db() {
        let dir = std::env::temp_dir().join(format!("bebok-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bebok.db");
        {
            // Simulate a database created before llm_calls/permission_log existed:
            // version 1 recorded, but the new tables absent.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
                 INSERT INTO schema_migrations (version, applied_at) VALUES (1, 0);
                 CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
                   added_at INTEGER NOT NULL, last_opened_at INTEGER, pinned INTEGER NOT NULL DEFAULT 0, grp TEXT);",
            )
            .unwrap();
        }
        let conn = open_db_at(&path).unwrap();
        assert!(table_exists(&conn, "llm_calls"));
        assert!(table_exists(&conn, "permission_log"));
        // Reopening again must stay a no-op.
        drop(conn);
        let conn = open_db_at(&path).unwrap();
        assert!(table_exists(&conn, "permission_log"));
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_llm_call_round_trip() {
        let conn = open_memory().unwrap();
        let id1 = insert_llm_call(
            &conn,
            &LlmCall {
                ts: 100,
                model: "anthropic/claude-haiku-5-5",
                session_id: Some("s1"),
                status: "ok",
                input_tokens: 10,
                output_tokens: 5,
                cost: Some(0.0012),
            },
        )
        .unwrap();
        let id2 = insert_llm_call(
            &conn,
            &LlmCall {
                ts: 200,
                model: "openai/gpt-x",
                session_id: None,
                status: "rate_limited",
                input_tokens: 0,
                output_tokens: 0,
                cost: None,
            },
        )
        .unwrap();
        assert_eq!(id2, id1 + 1);

        let (model, session, status, input, output, cost): (
            String,
            Option<String>,
            String,
            i64,
            i64,
            Option<f64>,
        ) = conn
            .query_row(
                "SELECT model, session_id, status, input_tokens, output_tokens, cost FROM llm_calls WHERE id = ?1",
                [id1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(model, "anthropic/claude-haiku-5-5");
        assert_eq!(session.as_deref(), Some("s1"));
        assert_eq!(status, "ok");
        assert_eq!((input, output), (10, 5));
        assert_eq!(cost, Some(0.0012));

        let null_session: Option<String> = conn
            .query_row(
                "SELECT session_id FROM llm_calls WHERE id = ?1",
                [id2],
                |r| r.get(0),
            )
            .unwrap();
        assert!(null_session.is_none());
    }

    #[test]
    fn insert_permission_log_round_trip() {
        let conn = open_memory().unwrap();
        let id = insert_permission_log(
            &conn,
            &PermissionLog {
                ts: 300,
                session_id: Some("s2"),
                agent: "code",
                tool: "bash",
                verdict: "ask",
                pattern: Some("bash(git *)"),
            },
        )
        .unwrap();
        let (agent, tool, verdict, pattern): (String, String, String, Option<String>) = conn
            .query_row(
                "SELECT agent, tool, verdict, pattern FROM permission_log WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (agent.as_str(), tool.as_str(), verdict.as_str()),
            ("code", "bash", "ask")
        );
        assert_eq!(pattern.as_deref(), Some("bash(git *)"));

        let id2 = insert_permission_log(
            &conn,
            &PermissionLog {
                ts: 301,
                session_id: None,
                agent: "plan",
                tool: "read_file",
                verdict: "allow",
                pattern: None,
            },
        )
        .unwrap();
        assert_eq!(id2, id + 1);
    }

    fn session(id: &str, dir: &str, updated_at: i64) -> DbSessionRow {
        DbSessionRow {
            id: id.into(),
            instance_hash: "hash".into(),
            directory: dir.into(),
            agent: "code".into(),
            title: format!("title {id}"),
            created_at: 1,
            updated_at,
            parent_id: None,
            parent_index: None,
            meta_json: None,
            deleted_at: None,
        }
    }

    fn message(session_id: &str, idx: i64, text: &str) -> DbMessageRow {
        DbMessageRow {
            session_id: session_id.into(),
            idx,
            created_at: 10 + idx,
            role: if idx % 2 == 0 { "user" } else { "assistant" }.into(),
            message_json: format!("{{\"text\":\"{text}\"}}"),
            text_extract: text.into(),
        }
    }

    #[test]
    fn migration_v1_database_upgrades_to_v2_keeping_data() {
        let dir = std::env::temp_dir().join(format!("bebok-db-v2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bebok.db");
        {
            // A database as shipped with schema version 1, holding a project row.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
            )
            .unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, path, added_at) VALUES ('p1', 'proj', '/x', 0)",
                [],
            )
            .unwrap();
        }
        let conn = open_db_at(&path).unwrap();
        assert!(table_exists(&conn, "sessions"));
        assert!(table_exists(&conn, "messages"));
        assert!(table_exists(&conn, "messages_fts"));
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(versions, vec![1, 2]);
        let projects: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(projects, 1);
        drop(conn);
        // Reopening a v2 database must not re-apply V2 (would fail: table exists).
        let conn = open_db_at(&path).unwrap();
        let versions: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 2);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_and_message_round_trip() {
        let conn = open_memory().unwrap();
        upsert_session(&conn, &session("s1", "/p", 100)).unwrap();
        insert_message(&conn, &message("s1", 1, "second")).unwrap();
        insert_message(&conn, &message("s1", 0, "first")).unwrap();

        let meta = get_session_meta(&conn, "s1").unwrap().unwrap();
        assert_eq!(meta, session("s1", "/p", 100));
        assert!(get_session_meta(&conn, "missing").unwrap().is_none());

        let transcript = load_transcript_db(&conn, "s1").unwrap();
        let texts: Vec<&str> = transcript.iter().map(|m| m.text_extract.as_str()).collect();
        assert_eq!(texts, vec!["first", "second"]);
        assert_eq!(transcript[0].role, "user");

        // Upsert updates in place; insert_message on the same idx replaces it.
        let mut changed = session("s1", "/p", 200);
        changed.title = "renamed".into();
        upsert_session(&conn, &changed).unwrap();
        insert_message(&conn, &message("s1", 0, "edited")).unwrap();
        assert_eq!(
            get_session_meta(&conn, "s1").unwrap().unwrap().title,
            "renamed"
        );
        let transcript = load_transcript_db(&conn, "s1").unwrap();
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].text_extract, "edited");
    }

    #[test]
    fn list_for_dir_orders_and_hides_deleted() {
        let conn = open_memory().unwrap();
        upsert_session(&conn, &session("old", "/p", 1)).unwrap();
        upsert_session(&conn, &session("new", "/p", 5)).unwrap();
        upsert_session(&conn, &session("other", "/q", 9)).unwrap();
        let ids: Vec<String> = list_session_metas_for_dir(&conn, "/p")
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["new", "old"]);

        assert_eq!(mark_session_deleted(&conn, "new", 50).unwrap(), 1);
        assert_eq!(mark_session_deleted(&conn, "nope", 50).unwrap(), 0);
        let ids: Vec<String> = list_session_metas_for_dir(&conn, "/p")
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["old"]);
        // Soft-deleted rows stay readable by id, with the stamp set.
        let deleted = get_session_meta(&conn, "new").unwrap().unwrap();
        assert_eq!(deleted.deleted_at, Some(50));
    }

    #[test]
    fn delete_messages_from_truncates_tail() {
        let conn = open_memory().unwrap();
        upsert_session(&conn, &session("s1", "/p", 1)).unwrap();
        for i in 0..4 {
            insert_message(&conn, &message("s1", i, &format!("m{i}"))).unwrap();
        }
        assert_eq!(delete_messages_from(&conn, "s1", 2).unwrap(), 2);
        let left = load_transcript_db(&conn, "s1").unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left.last().unwrap().idx, 1);
    }

    #[test]
    fn fts_finds_inserted_updated_and_deleted_text() {
        let conn = open_memory().unwrap();
        upsert_session(&conn, &session("s1", "/p", 1)).unwrap();
        upsert_session(&conn, &session("s2", "/p", 1)).unwrap();
        insert_message(&conn, &message("s1", 0, "refactor the scheduler tick")).unwrap();
        insert_message(&conn, &message("s2", 0, "unrelated banana text")).unwrap();

        let hits = search_messages_fts(&conn, "scheduler", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].idx, 0);
        assert!(hits[0].snippet.contains("<b>scheduler</b>"));

        // Update replaces the indexed text.
        insert_message(&conn, &message("s1", 0, "rewrote the batcher")).unwrap();
        assert!(
            search_messages_fts(&conn, "scheduler", 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(search_messages_fts(&conn, "batcher", 10).unwrap().len(), 1);

        // Truncation removes the entry from the index.
        delete_messages_from(&conn, "s1", 0).unwrap();
        assert!(
            search_messages_fts(&conn, "batcher", 10)
                .unwrap()
                .is_empty()
        );

        // Non-matching and punctuation-heavy queries must not error.
        assert!(search_messages_fts(&conn, "zzzz", 10).unwrap().is_empty());
        assert!(
            search_messages_fts(&conn, "  \"unbalanced( ", 10)
                .unwrap()
                .is_empty()
        );
        assert!(search_messages_fts(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn fts_excludes_soft_deleted_and_cascades_on_hard_delete() {
        let conn = open_memory().unwrap();
        upsert_session(&conn, &session("s1", "/p", 1)).unwrap();
        upsert_session(&conn, &session("s2", "/p", 1)).unwrap();
        insert_message(&conn, &message("s1", 0, "needle one")).unwrap();
        insert_message(&conn, &message("s2", 0, "needle two")).unwrap();
        assert_eq!(search_messages_fts(&conn, "needle", 10).unwrap().len(), 2);

        mark_session_deleted(&conn, "s1", 9).unwrap();
        let hits = search_messages_fts(&conn, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s2");

        // Hard delete cascades to messages; the delete trigger clears FTS.
        conn.execute("DELETE FROM sessions WHERE id = 's2'", [])
            .unwrap();
        assert!(search_messages_fts(&conn, "needle", 10).unwrap().is_empty());
        let fts_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages_fts_docsize", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fts_rows, 1); // only s1's message remains indexed
    }

    #[test]
    fn import_sentinel_is_hidden_and_reserved() {
        let conn = open_memory().unwrap();
        assert!(!import_done(&conn).unwrap());
        set_import_done(&conn, 1000).unwrap();
        assert!(import_done(&conn).unwrap());
        // Idempotent.
        set_import_done(&conn, 2000).unwrap();
        assert!(import_done(&conn).unwrap());

        // Sentinel never shows up as a session or in listings.
        assert!(
            get_session_meta(&conn, IMPORT_SENTINEL_ID)
                .unwrap()
                .is_none()
        );
        assert!(list_session_metas_for_dir(&conn, "").unwrap().is_empty());

        // Callers cannot overwrite the sentinel through the session API.
        let err = upsert_session(&conn, &session(IMPORT_SENTINEL_ID, "", 1)).unwrap_err();
        assert!(err.contains("reserved"));
    }
}
