//! Batch processing: asynchronous bulk LLM jobs that run alongside (but
//! never block) the interactive session turn.

use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Status of a single item within a batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BatchItemStatus {
    Pending,
    Submitted,
    Done,
    Failed,
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

/// Thread-safe in-memory store for batch records.
pub struct BatchStore {
    inner: RwLock<HashMap<String, BatchRecord>>,
}

impl BatchStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Insert a record and return its id.
    pub async fn insert(&self, mut record: BatchRecord) -> String {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        record.id.clone_from(&id);
        record.created_at = now.clone();
        record.updated_at = now;
        self.inner.write().await.insert(id.clone(), record);
        id
    }

    /// Look up a record by id.
    pub async fn get(&self, id: &str) -> Option<BatchRecord> {
        self.inner.read().await.get(id).cloned()
    }

    /// Atomically mutate a record. Returns `true` if the record existed.
    pub async fn update(&self, id: &str, f: impl FnOnce(&mut BatchRecord)) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(rec) = guard.get_mut(id) {
            f(rec);
            rec.updated_at = Utc::now().to_rfc3339();
            true
        } else {
            false
        }
    }

    /// All records that belong to a given `(directory, session_id)` pair.
    pub async fn list_for_session(&self, directory: &str, session: &str) -> Vec<BatchRecord> {
        self.inner
            .read()
            .await
            .values()
            .filter(|r| r.directory == directory && r.session_id == session)
            .cloned()
            .collect()
    }

    /// Transition a batch to `Cancelled` unless it is already terminal.
    /// Returns `true` if the transition happened.
    pub async fn cancel(&self, id: &str) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(rec) = guard.get_mut(id)
            && !rec.state.is_terminal()
        {
            rec.state = BatchState::Cancelled;
            rec.updated_at = Utc::now().to_rfc3339();
            return true;
        }
        false
    }
}

impl Default for BatchStore {
    fn default() -> Self {
        Self::new()
    }
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
}
