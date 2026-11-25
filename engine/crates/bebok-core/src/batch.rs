//! Batch processing: asynchronous bulk LLM jobs that run alongside (but
//! never block) the interactive session turn.
//!
//! Records are persisted in the `batches` / `batch_items` tables of the Bebok
//! database (see `db.rs`). Every mutation runs in a single transaction, so a
//! read-modify-write through [`BatchStore::update`] is atomic.

use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of a single item within a batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BatchItemStatus {
    Pending,
    Submitted,
    Done,
    Failed,
}

impl BatchItemStatus {
    fn as_db(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Submitted => "submitted",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "submitted" => Self::Submitted,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// One unit of work inside a batch — a prepared chat-completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchItem {
    /// Zero-based index inside the batch.
    pub index: u32,
    /// Model identifier (e.g. `"openai/gpt-4o"`).
    pub model: String,
    /// Ready-to-send chat messages (`"role"` / `"content"` objects).
    pub messages: Vec<serde_json::Value>,
    pub status: BatchItemStatus,
    /// Successful response body (the assistant message content).
    pub result: Option<String>,
    /// Error message when `status == Failed`.
    pub error: Option<String>,
}

/// Lifecycle state of the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BatchState {
    Created,
    Uploading,
    Submitted,
    Polling,
    Completed,
    Failed,
    Cancelled,
}

impl BatchState {
    /// `Completed`, `Failed` and `Cancelled` are terminal — no further
    /// transitions are expected.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BatchState::Completed | BatchState::Failed | BatchState::Cancelled
        )
    }

    fn as_db(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Uploading => "uploading",
            Self::Submitted => "submitted",
            Self::Polling => "polling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "created" => Self::Created,
            "uploading" => Self::Uploading,
            "submitted" => Self::Submitted,
            "polling" => Self::Polling,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// Persistent record describing a batch job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRecord {
    /// Unique id (UUID v4).
    pub id: String,
    /// Instance directory this batch belongs to.
    pub directory: String,
    /// Session that created the batch.
    pub session_id: String,
    /// Provider key (e.g. `"openrouter"`).
    pub provider: String,
    /// Model identifier shared by all items.
    pub model: String,
    pub state: BatchState,
    /// Provider-specific external job id (set after submission).
    pub job_id: Option<String>,
    pub items: Vec<BatchItem>,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last-update timestamp.
    pub updated_at: String,
    /// Top-level error when state is `Failed`.
    pub error: Option<String>,
}

/// Batch store backed by the `batches` / `batch_items` SQLite tables.
///
/// Methods keep their original signatures. Database errors are logged with
/// `tracing::error!` and reported as `None` / `false` / an empty list; use
/// [`BatchStore::insert`] knowing that an insert failure is only visible in
/// the log.
pub struct BatchStore {
    conn: Mutex<Connection>,
}

impl BatchStore {
    /// Ephemeral store on an in-memory database. Used by tests and by
    /// throw-away `AppState` instances.
    pub fn new() -> Self {
        let conn = crate::db::open_memory().expect("in-memory sqlite must open");
        Self::with_connection(conn)
    }

    /// Store persisted in the shared Bebok database (`db::open_db`).
    pub fn open_default() -> Result<Self, String> {
        Ok(Self::with_connection(crate::db::open_db()?))
    }

    /// Store on an existing connection (schema must already be applied,
    /// e.g. via `db::open_db_at`).
    pub fn with_connection(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means a previous holder panicked mid-query;
        // SQLite itself rolls back uncommitted transactions, so keep going.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Insert a record and return its id.
    ///
    /// The id, `created_at` and `updated_at` are assigned here. If the write
    /// fails the error is logged and the generated id is still returned.
    pub async fn insert(&self, record: BatchRecord) -> String {
        match self.try_insert(record) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("batch insert failed: {e}");
                Uuid::new_v4().to_string()
            }
        }
    }

    fn try_insert(&self, mut record: BatchRecord) -> rusqlite::Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        record.id.clone_from(&id);
        record.created_at = now.clone();
        record.updated_at = now;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        write_record(&tx, &record)?;
        tx.commit()?;
        Ok(id)
    }

    /// Look up a record by id.
    pub async fn get(&self, id: &str) -> Option<BatchRecord> {
        let conn = self.lock();
        log_err(load_record(&conn, id), "batch get").flatten()
    }

    /// Atomically mutate a record. Returns `true` if the record existed.
    pub async fn update(&self, id: &str, f: impl FnOnce(&mut BatchRecord)) -> bool {
        self.mutate(id, |rec| f(rec))
            .unwrap_or_else(|e| {
                tracing::error!("batch update failed: {e}");
                None
            })
            .is_some()
    }

    /// Atomically mutate a record and map it to a value. Returns
    /// `R::default()` when the record does not exist.
    pub async fn update_and_map<R: Default>(
        &self,
        id: &str,
        f: impl FnOnce(&mut BatchRecord) -> R,
    ) -> R {
        self.mutate(id, f)
            .unwrap_or_else(|e| {
                tracing::error!("batch update failed: {e}");
                None
            })
            .unwrap_or_default()
    }

    /// Load, mutate and store a record in one transaction. `Ok(None)` when
    /// the record does not exist; `updated_at` is bumped on every write.
    fn mutate<R>(
        &self,
        id: &str,
        f: impl FnOnce(&mut BatchRecord) -> R,
    ) -> rusqlite::Result<Option<R>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let Some(mut rec) = load_record(&tx, id)? else {
            return Ok(None);
        };
        let out = f(&mut rec);
        rec.updated_at = now_rfc3339();
        write_record(&tx, &rec)?;
        tx.commit()?;
        Ok(Some(out))
    }

    /// All records that belong to a given `(directory, session_id)` pair,
    /// oldest first.
    pub async fn list_for_session(&self, directory: &str, session: &str) -> Vec<BatchRecord> {
        let conn = self.lock();
        log_err(list_in(&conn, directory, session), "batch list").unwrap_or_default()
    }

    /// Transition a batch to `Cancelled` unless it is already terminal.
    /// Returns `true` if the transition happened.
    pub async fn cancel(&self, id: &str) -> bool {
        self.try_cancel(id).unwrap_or_else(|e| {
            tracing::error!("batch cancel failed: {e}");
            false
        })
    }

    fn try_cancel(&self, id: &str) -> rusqlite::Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let Some(mut rec) = load_record(&tx, id)? else {
            return Ok(false);
        };
        if rec.state.is_terminal() {
            return Ok(false);
        }
        rec.state = BatchState::Cancelled;
        rec.updated_at = now_rfc3339();
        write_record(&tx, &rec)?;
        tx.commit()?;
        Ok(true)
    }
}

impl Default for BatchStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SQL helpers (all take a plain connection; callers hold the lock)
// ---------------------------------------------------------------------------

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn log_err<T>(res: rusqlite::Result<T>, what: &str) -> Option<T> {
    match res {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::error!("{what} failed: {e}");
            None
        }
    }
}

fn invalid_text(col: usize, name: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(col, name.to_string(), rusqlite::types::Type::Text)
}

fn load_record(conn: &Connection, id: &str) -> rusqlite::Result<Option<BatchRecord>> {
    let head = conn
        .query_row(
            "SELECT id, directory, session_id, provider, model, state, job_id,
                    created_at, updated_at, error
             FROM batches WHERE id = ?1",
            [id],
            |row| {
                let state_s: String = row.get(5)?;
                let state =
                    BatchState::from_db(&state_s).ok_or_else(|| invalid_text(5, "state"))?;
                Ok(BatchRecord {
                    id: row.get(0)?,
                    directory: row.get(1)?,
                    session_id: row.get(2)?,
                    provider: row.get(3)?,
                    model: row.get(4)?,
                    state,
                    job_id: row.get(6)?,
                    items: Vec::new(),
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    error: row.get(9)?,
                })
            },
        )
        .optional()?;
    let Some(mut rec) = head else {
        return Ok(None);
    };
    rec.items = load_items(conn, id)?;
    Ok(Some(rec))
}

fn load_items(conn: &Connection, batch_id: &str) -> rusqlite::Result<Vec<BatchItem>> {
    let mut stmt = conn.prepare(
        "SELECT idx, model, messages_json, status, result, error
         FROM batch_items WHERE batch_id = ?1 ORDER BY idx",
    )?;
    let rows = stmt.query_map([batch_id], read_item)?;
    rows.collect()
}

fn read_item(row: &Row<'_>) -> rusqlite::Result<BatchItem> {
    let messages_json: String = row.get(2)?;
    let messages: Vec<serde_json::Value> = serde_json::from_str(&messages_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let status_s: String = row.get(3)?;
    let status = BatchItemStatus::from_db(&status_s).ok_or_else(|| invalid_text(3, "status"))?;
    let idx: i64 = row.get(0)?;
    Ok(BatchItem {
        index: u32::try_from(idx).map_err(|_| invalid_text(0, "idx"))?,
        model: row.get(1)?,
        messages,
        status,
        result: row.get(4)?,
        error: row.get(5)?,
    })
}

/// Upsert the batch row and replace its items. Must run inside a transaction.
fn write_record(conn: &Connection, rec: &BatchRecord) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO batches
            (id, directory, session_id, provider, model, state, job_id,
             created_at, updated_at, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            directory = excluded.directory,
            session_id = excluded.session_id,
            provider = excluded.provider,
            model = excluded.model,
            state = excluded.state,
            job_id = excluded.job_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            error = excluded.error",
        params![
            rec.id,
            rec.directory,
            rec.session_id,
            rec.provider,
            rec.model,
            rec.state.as_db(),
            rec.job_id,
            rec.created_at,
            rec.updated_at,
            rec.error,
        ],
    )?;
    conn.execute("DELETE FROM batch_items WHERE batch_id = ?1", [&rec.id])?;
    let mut stmt = conn.prepare(
        "INSERT INTO batch_items
            (batch_id, idx, model, messages_json, status, result, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for item in &rec.items {
        let messages_json = serde_json::to_string(&item.messages)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        stmt.execute(params![
            rec.id,
            item.index,
            item.model,
            messages_json,
            item.status.as_db(),
            item.result,
            item.error,
        ])?;
    }
    Ok(())
}

fn list_in(
    conn: &Connection,
    directory: &str,
    session: &str,
) -> rusqlite::Result<Vec<BatchRecord>> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM batches
             WHERE directory = ?1 AND session_id = ?2
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![directory, session], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(rec) = load_record(conn, &id)? {
            out.push(rec);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> BatchRecord {
        BatchRecord {
            id: String::new(),
            directory: "/projects/acme".into(),
            session_id: "sess-1".into(),
            provider: "openrouter".into(),
            model: "openai/gpt-4o".into(),
            state: BatchState::Created,
            job_id: None,
            items: vec![BatchItem {
                index: 0,
                model: "openai/gpt-4o".into(),
                messages: vec![serde_json::json!({
                    "role": "user",
                    "content": "hello"
                })],
                status: BatchItemStatus::Pending,
                result: None,
                error: None,
            }],
            created_at: String::new(),
            updated_at: String::new(),
            error: None,
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let store = BatchStore::new();
        let id = store.insert(sample_record()).await;
        assert!(!id.is_empty());
        let rec = store.get(&id).await.unwrap();
        assert_eq!(rec.directory, "/projects/acme");
        assert_eq!(rec.state, BatchState::Created);
        assert_eq!(rec.items.len(), 1);
        assert_eq!(rec.items[0].messages[0]["content"], "hello");
        // timestamps are populated
        assert!(!rec.created_at.is_empty());
    }

    #[tokio::test]
    async fn update_mutates_record() {
        let store = BatchStore::new();
        let id = store.insert(sample_record()).await;
        store
            .update(&id, |r| {
                r.state = BatchState::Submitted;
                r.job_id = Some("job-abc".into());
            })
            .await;
        let rec = store.get(&id).await.unwrap();
        assert_eq!(rec.state, BatchState::Submitted);
        assert_eq!(rec.job_id.as_deref(), Some("job-abc"));
    }

    #[tokio::test]
    async fn update_persists_item_changes() {
        let store = BatchStore::new();
        let id = store.insert(sample_record()).await;
        let res = store
            .update_and_map(&id, |r| {
                r.items[0].status = BatchItemStatus::Done;
                r.items[0].result = Some("hi".into());
                42u32
            })
            .await;
        assert_eq!(res, 42);
        let rec = store.get(&id).await.unwrap();
        assert_eq!(rec.items[0].status, BatchItemStatus::Done);
        assert_eq!(rec.items[0].result.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn update_nonexistent_returns_false() {
        let store = BatchStore::new();
        let changed = store.update("nope", |_| {}).await;
        assert!(!changed);
    }

    #[tokio::test]
    async fn list_for_session_filters() {
        let store = BatchStore::new();
        store.insert(sample_record()).await;
        let mut other = sample_record();
        other.session_id = "sess-2".into();
        store.insert(other).await;

        let list = store.list_for_session("/projects/acme", "sess-1").await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, "sess-1");
    }

    #[tokio::test]
    async fn cancel_pending_batch() {
        let store = BatchStore::new();
        let id = store.insert(sample_record()).await;
        assert!(store.cancel(&id).await);
        assert_eq!(store.get(&id).await.unwrap().state, BatchState::Cancelled);
    }

    #[tokio::test]
    async fn cancel_terminal_batch_is_noop() {
        let store = BatchStore::new();
        let mut rec = sample_record();
        rec.state = BatchState::Completed;
        let id = store.insert(rec).await;
        assert!(!store.cancel(&id).await);
        assert_eq!(store.get(&id).await.unwrap().state, BatchState::Completed);
    }

    #[tokio::test]
    async fn records_survive_reopen() {
        let path = std::env::temp_dir().join(format!("bebok-batch-{}.db", Uuid::new_v4()));
        let id = {
            let conn = crate::db::open_db_at(&path).unwrap();
            let store = BatchStore::with_connection(conn);
            store.insert(sample_record()).await
        };
        let conn = crate::db::open_db_at(&path).unwrap();
        let store = BatchStore::with_connection(conn);
        let rec = store.get(&id).await.expect("record persisted");
        assert_eq!(rec.items.len(), 1);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn batch_state_is_terminal() {
        assert!(!BatchState::Created.is_terminal());
        assert!(!BatchState::Uploading.is_terminal());
        assert!(!BatchState::Submitted.is_terminal());
        assert!(!BatchState::Polling.is_terminal());
        assert!(BatchState::Completed.is_terminal());
        assert!(BatchState::Failed.is_terminal());
        assert!(BatchState::Cancelled.is_terminal());
    }

    #[test]
    fn state_strings_round_trip() {
        for s in [
            BatchState::Created,
            BatchState::Uploading,
            BatchState::Submitted,
            BatchState::Polling,
            BatchState::Completed,
            BatchState::Failed,
            BatchState::Cancelled,
        ] {
            assert_eq!(BatchState::from_db(s.as_db()), Some(s));
        }
    }
}
