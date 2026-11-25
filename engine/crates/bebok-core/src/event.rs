//! Global event bus (SPEC §3.11).
//!
//! One stream (`GET /event`), every event carries the envelope
//! `{ type, directory, sessionID, properties }` so clients can route by
//! directory. No polling anywhere.
//!
//! Known event types include `session.*`, `message.updated`,
//! `message.part.updated`, `permission.asked` / `permission.resolved`,
//! `task.started` / `task.progress` / `task.ended` / `task.restarted`
//! (`properties` carries `{ taskID, reason, attempt }`), `browser.frame`,
//! `agent.list.changed`, `config.changed`, `pty.exited`,
//! `plugin.changed` (plugin declaration installed / enabled / disabled:
//! `properties` carries `{ name, change }` with `change` one of
//! `installed` / `enabled` / `disabled`),
//! `plugin.install.progress` (`properties`: `{ name, stage, detail }`),
//! `batch.submitted` (`properties`: `{ batchID, count }`),
//! `batch.completed` (`properties`: `{ batchID, ok, failed }`),
//! `batch.failed` (`properties`: `{ batchID, error }`), and
//! `batch.progress` (`properties`: `{ batchID, done, total }`), and
//! `ssh.prompt` (a persistent SSH session needs a password:
//! `properties`: `{ requestID, sessionID, directory, host, prompt }`, where
//! `prompt` is `password` (SSH login) or `sudo`).
//!
//! `question.asked` (the `ask_user` tool is blocked on the user:
//! `properties`: `{ requestID, sessionID, messageIndex, questions }`, where
//! `questions` is the validated array of `{ id, question, header?, options:
//! [{ label, description? }], multiSelect? }`) and `question.resolved`
//! (`properties`: `{ requestID, messageIndex, outcome, answers? }`, `outcome`
//! one of `answered` / `timeout` / `aborted`; `answers` only when answered).
//! Answer with `POST /session/{id}/question/{requestID}` `{ answers }`.
//!
//! `cli.turn.stopped` (`properties`: `{ changedFiles }`) is published by
//! `POST /cli-hook/{session}/stop` when a managed-profile CLI turn (Claude Code)
//! ends. `changedFiles` is the number of paths the change tracker holds for the
//! session at that moment (`Tracker::tracked_count`), not a per-turn diff.

use tokio::sync::broadcast;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub directory: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub properties: serde_json::Value,
}

impl Event {
    pub fn new(kind: &str, directory: &str, session_id: &str) -> Self {
        Self {
            kind: kind.to_string(),
            directory: directory.to_string(),
            session_id: session_id.to_string(),
            properties: serde_json::Value::Null,
        }
    }

    pub fn with_properties(mut self, properties: serde_json::Value) -> Self {
        self.properties = properties;
        self
    }

    /// TOR B plugin declarations: `plugin.changed` for an instance
    /// directory, with `properties = { name, change }`. `change` is one of
    /// `installed` (declaration + slot dir created) / `enabled` /
    /// `disabled` (the `enabled` switch flipped).
    pub fn plugin_changed(directory: &str, name: &str, change: &str) -> Self {
        Self::new("plugin.changed", directory, "").with_properties(serde_json::json!({
            "name": name,
            "change": change,
        }))
    }

    /// Plugin install progress: `plugin.install.progress` for an instance
    /// directory, with `properties = { name, stage, detail }`.
    /// `stage` is one of `download` / `verify` / `unpack` / `install`.
    pub fn plugin_install_progress(directory: &str, name: &str, stage: &str, detail: &str) -> Self {
        Self::new("plugin.install.progress", directory, "").with_properties(serde_json::json!({
            "name": name,
            "stage": stage,
            "detail": detail,
        }))
    }

    /// Batch submitted: `batch.submitted` — items uploaded and job created.
    pub fn batch_submitted(
        directory: &str,
        session_id: &str,
        batch_id: &str,
        count: usize,
    ) -> Self {
        Self::new("batch.submitted", directory, session_id).with_properties(serde_json::json!({
            "batchID": batch_id,
            "count": count,
        }))
    }

    /// Batch completed: `batch.completed` — all items processed.
    pub fn batch_completed(
        directory: &str,
        session_id: &str,
        batch_id: &str,
        ok: usize,
        failed: usize,
    ) -> Self {
        Self::new("batch.completed", directory, session_id).with_properties(serde_json::json!({
            "batchID": batch_id,
            "ok": ok,
            "failed": failed,
        }))
    }

    /// Batch failed: `batch.failed` — unrecoverable top-level error.
    pub fn batch_failed(directory: &str, session_id: &str, batch_id: &str, error: &str) -> Self {
        Self::new("batch.failed", directory, session_id).with_properties(serde_json::json!({
            "batchID": batch_id,
            "error": error,
        }))
    }

    /// `ask_user` is waiting for the user: `question.asked` (see the module
    /// header for the payload).
    pub fn question_asked(
        directory: &str,
        session_id: &str,
        request_id: &str,
        message_index: usize,
        questions: serde_json::Value,
    ) -> Self {
        Self::new("question.asked", directory, session_id).with_properties(serde_json::json!({
            "requestID": request_id,
            "sessionID": session_id,
            "messageIndex": message_index,
            "questions": questions,
        }))
    }

    /// `ask_user` finished: `question.resolved` with `outcome` `answered`,
    /// `timeout` or `aborted`; `answers` is set only for `answered`.
    pub fn question_resolved(
        directory: &str,
        session_id: &str,
        request_id: &str,
        message_index: usize,
        outcome: &str,
        answers: Option<serde_json::Value>,
    ) -> Self {
        let mut properties = serde_json::json!({
            "requestID": request_id,
            "messageIndex": message_index,
            "outcome": outcome,
        });
        if let Some(answers) = answers {
            properties["answers"] = answers;
        }
        Self::new("question.resolved", directory, session_id).with_properties(properties)
    }

    /// Batch progress: `batch.progress` — periodic update while polling.
    pub fn batch_progress(
        directory: &str,
        session_id: &str,
        batch_id: &str,
        done: usize,
        total: usize,
    ) -> Self {
        Self::new("batch.progress", directory, session_id).with_properties(serde_json::json!({
            "batchID": batch_id,
            "done": done,
            "total": total,
        }))
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event; a missing subscriber is not an error.
    pub fn publish(&self, event: Event) {
        let kind = event.kind.clone();
        if self.tx.send(event).is_err() {
            tracing::debug!("event {kind} dropped: no active subscribers");
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}
