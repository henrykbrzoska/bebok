//! Session lifecycle Repository (next to `context.rs`).
//!
//! Fork / compact / truncate / export / delete plus the low-level
//! `spawn_session` / `copy_messages` helpers. The original session is never
//! mutated by fork/compact (disk journal stays authoritative); truncate is the
//! explicit in-place rollback path and refuses while a turn is running.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use uuid::Uuid;

use super::instance::Instance;
use super::instance_store::InstanceStore;
use super::session_state::SessionState;
use crate::error::{CoreError, Result};
use crate::session::persist;
use crate::session::{Message, Session};

impl InstanceStore {
    /// Fork a session: a new, independent session that materializes a copy of
    /// messages `0..=message_index`, recording `parent: (source, message_index)`.
    pub async fn fork_session(&self, source: Uuid, message_index: usize) -> Result<Arc<SessionState>> {
        let source_state = self.open_session(source).await?;
        let source_meta = source_state.meta_snapshot().await;
        let messages = source_state.messages_snapshot().await;
        let up_to = (message_index + 1).min(messages.len());

        let instance = self.get_or_create_instance(&source_meta.directory).await?;

        let mut session = Session::new(source_meta.directory.clone(), source_meta.agent.clone());
        session.model = source_meta.model.clone();
        session.parent = Some((source, message_index));

        let state = self.spawn_session(&instance, session).await?;
        self.copy_messages(&state, &messages[..up_to]).await;
        Ok(state)
    }

    /// Compaction as an internal fork: a new session whose transcript is a
    /// `[summary of messages 0..N]` text followed by the tail (`messages[tail_from..]`).
    /// The original session is untouched on disk ("show full history" works).
    pub async fn compact_session(
        &self,
        source: Uuid,
        summary: String,
        tail_from: usize,
    ) -> Result<Arc<SessionState>> {
        let source_state = self.open_session(source).await?;
        let source_meta = source_state.meta_snapshot().await;
        let messages = source_state.messages_snapshot().await;
        let tail_from = tail_from.min(messages.len());

        let instance = self.get_or_create_instance(&source_meta.directory).await?;

        let mut session = Session::new(source_meta.directory.clone(), source_meta.agent.clone());
        session.model = source_meta.model.clone();
        session.parent = Some((source, tail_from));

        let state = self.spawn_session(&instance, session).await?;
        // [summary] first, then the tail.
        let summary_message = Message::summary(&summary);
        self.copy_messages(&state, std::slice::from_ref(&summary_message)).await;
        self.copy_messages(&state, &messages[tail_from..]).await;
        Ok(state)
    }

    /// Rewind a session in place: drop every message with index `>= keep`, so
    /// the transcript ends at `messages[..keep]`. Orphaned message files on disk
    /// are removed and the session's `updated_at` is bumped. This is the
    /// rollback path - unlike `fork_session` it does not create a new session,
    /// it erases the tail of the *current* one. Refuses while a turn is running.
    pub async fn truncate_session(&self, id: Uuid, keep: usize) -> Result<Arc<SessionState>> {
        let state = self.open_session(id).await?;
        if state.is_running() {
            return Err(CoreError::SessionBusy);
        }
        {
            let mut messages = state.messages.write().await;
            if keep >= messages.len() {
                return Err(CoreError::Other(
                    "nothing to truncate: message index is beyond the transcript".into(),
                ));
            }
            messages.truncate(keep);
        }
        // Remove orphaned message files for the dropped tail.
        let dir = state.disk_dir().to_path_buf();
        let mut idx = keep;
        loop {
            let path = persist::message_path(&dir, idx);
            if !path.exists() {
                break;
            }
            if let Err(e) = tokio::fs::remove_file(&path).await {
                tracing::error!(
                    "failed to remove message {idx} of session {}: {e}",
                    state.id()
                );
            }
            idx += 1;
        }
        state.max_message_index.store(keep, Ordering::Relaxed);
        state.touch().await;
        self.bus.publish(crate::event::Event::new(
            "session.updated",
            state.directory(),
            &id.to_string(),
        ));
        Ok(state)
    }

    /// Full JSON export of a session (metadata + transcript).
    pub async fn export_session(&self, id: Uuid) -> Result<serde_json::Value> {
        let state = self.open_session(id).await?;
        let meta = state.meta_snapshot().await;
        let messages = state.messages_snapshot().await;
        Ok(serde_json::json!({
            "session": meta,
            "messages": messages,
        }))
    }

    /// Delete a session permanently: remove it from memory (state + metadata
    /// cache) and drop its on-disk transcript directory. Refuses while a turn
    /// is running (409), so a live agent loop cannot lose its target. The
    /// append-only `index.jsonl` keeps the historical `created`/`deleted`
    /// events. Emits `session.deleted` on the bus.
    pub async fn delete_session(&self, id: Uuid) -> Result<Session> {
        let state = self.open_session(id).await?;
        if state.is_running() {
            return Err(CoreError::SessionBusy);
        }

        // Snapshot the freshest metadata before unwinding anything.
        let snapshot = state.meta_snapshot().await;
        // Take the session out of the live map first (new lookups fail 404),
        // then the metadata cache (session lists stop returning it). If the
        // session was never opened this run, fall back to the scanned metadata.
        let _removed = self.sessions.write().await.remove(&id);
        let meta = self.meta.write().await.remove(&id).unwrap_or(snapshot);

        // Drop the on-disk session directory (session.json + msg-*.json).
        let disk_dir = state.disk_dir().to_path_buf();
        if disk_dir.exists() {
            if let Err(e) = tokio::fs::remove_dir_all(&disk_dir).await {
                tracing::error!("failed to remove session dir {}: {e}", disk_dir.display());
            }
        }

        // Append-only index entry so the deletion itself is auditable.
        let inst_dir = persist::instance_dir(self.data_dir(), &meta.directory);
        persist::append_index_event(&inst_dir, "deleted", &meta).await;

        self.bus.publish(crate::event::Event::new(
            "session.deleted",
            &meta.directory,
            &id.to_string(),
        ));
        Ok(meta)
    }

    /// Low-level session spawn: create the on-disk dir, persist metadata, append
    /// the index event, register in memory and emit `session.created`.
    pub(crate) async fn spawn_session(&self, instance: &Instance, session: Session) -> Result<Arc<SessionState>> {
        let inst_dir = persist::instance_dir(self.data_dir(), &instance.directory);
        let disk_dir = persist::session_dir(&inst_dir, session.id);
        tokio::fs::create_dir_all(&disk_dir)
            .await
            .map_err(CoreError::Io)?;

        persist::persist_session_meta(&disk_dir, &session).await?;
        persist::append_index_event(&inst_dir, "created", &session).await;

        let state = Arc::new(SessionState::new(
            session.clone(),
            inst_dir,
            disk_dir,
            instance.config_snapshot(),
        ));

        self.meta.write().await.insert(session.id, session.clone());
        self.sessions.write().await.insert(session.id, state.clone());
        self.bus.publish(
            crate::event::Event::new("session.created", &instance.directory, &session.id.to_string())
                .with_properties(serde_json::json!({ "session": session })),
        );
        Ok(state)
    }

    /// Copy messages into a session (in-memory + persisted atomically).
    async fn copy_messages(&self, state: &SessionState, messages: &[Message]) {
        let mut guard = state.messages.write().await;
        let start = guard.len();
        for (offset, message) in messages.iter().enumerate() {
            let idx = start + offset;
            guard.push(message.clone());
            if let Err(e) = persist::persist_message(state.disk_dir(), idx, message).await {
                tracing::error!("failed to persist message {idx} of session {}: {e}", state.id());
            }
        }
        state.note_message_index(start + messages.len());
    }
}

#[cfg(test)]
mod tests {
    use super::super::instance_store::InstanceStore;

    #[tokio::test]
    async fn fork_export_compact_lifecycle() {
        let base = std::env::temp_dir().join(format!("bebok-store-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data);
        let s = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        for text in ["one", "two", "three", "four"] {
            s.append_user_message(text).await.unwrap();
        }

        // Export returns the full transcript.
        let exp = store.export_session(s.id()).await.unwrap();
        assert_eq!(exp["messages"].as_array().unwrap().len(), 4);
        assert_eq!(exp["session"]["id"], s.id().to_string());

        // Fork at index 1 -> copies messages 0..=1, records parent.
        let fork = store.fork_session(s.id(), 1).await.unwrap();
        assert_eq!(fork.meta_snapshot().await.parent, Some((s.id(), 1)));
        assert_eq!(fork.messages_snapshot().await.len(), 2);
        assert_ne!(fork.id(), s.id(), "fork must be an independent session");

        // Compaction: summary + tail, original untouched.
        let compact = store
            .compact_session(s.id(), "[summary of messages 0..1]".into(), 2)
            .await
            .unwrap();
        let compact_msgs = compact.messages_snapshot().await;
        assert_eq!(compact_msgs.len(), 3, "summary + 2 tail messages");
        assert!(compact_msgs[0].text_content().contains("[summary"));
        // Original transcript still has all 4 messages.
        assert_eq!(s.messages_snapshot().await.len(), 4);

        // continueLast resolves some session for the directory.
        let last = store
            .continue_last_session(project.to_str().unwrap())
            .await
            .unwrap();
        assert!(last.is_some());

        // Delete removes the session from memory AND disk (M6).
        let disk_dir = fork.disk_dir().to_path_buf();
        assert!(disk_dir.exists(), "session dir exists before delete");
        store.delete_session(fork.id()).await.unwrap();
        assert!(!disk_dir.exists(), "session dir removed by delete");
        assert!(store.open_session(fork.id()).await.is_err());
        assert!(
            !store
                .list_sessions(project.to_str().unwrap())
                .await
                .iter()
                .any(|s| s.id == fork.id())
        );
        // Deleting again -> 404 (already gone).
        assert!(store.delete_session(fork.id()).await.is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
