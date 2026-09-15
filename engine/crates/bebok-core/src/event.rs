//! Global event bus (SPEC §3.11).
//!
//! One stream (`GET /event`), every event carries the envelope
//! `{ type, directory, sessionID, properties }` so clients can route by
//! directory. No polling anywhere.
//!
//! Known event types include `session.*`, `message.updated`,
//! `message.part.updated`, `permission.asked` / `permission.resolved`,
//! `task.started` / `task.progress` / `task.ended`, `browser.frame`,
//! `agent.list.changed`, `config.changed`, `pty.exited`,
//! `plugin.changed` (plugin declaration installed / enabled / disabled:
//! `properties` carries `{ name, change }` with `change` one of
//! `installed` / `enabled` / `disabled`) and
//! `code.index.updated` (Phase 0 code-index wiring: `properties` carries
//! `{ status, files, symbols }`; `symbols` is 0 until the index engine
//! lands).

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

    /// Phase 0 code-index wiring: `code.index.updated` for an instance
    /// directory, with `properties = { status, files, symbols }`.
    /// `status` is one of `ready` / `indexing` / `disabled`; `symbols` is 0
    /// in Phase 0 (no index engine yet).
    pub fn code_index_updated(directory: &str, status: &str, files: usize, symbols: usize) -> Self {
        Self::new("code.index.updated", directory, "").with_properties(serde_json::json!({
            "status": status,
            "files": files,
            "symbols": symbols,
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

#[cfg(test)]
mod tests {
    use super::Event;

    #[test]
    fn plugin_changed_has_correct_kind_and_properties() {
        let event = Event::plugin_changed("/projects/acme", "bebok-index", "installed");
        assert_eq!(event.kind, "plugin.changed");
        assert_eq!(event.directory, "/projects/acme");
        assert_eq!(event.properties["name"], "bebok-index");
        assert_eq!(event.properties["change"], "installed");
    }

    #[test]
    fn code_index_updated_has_correct_kind_and_properties() {
        let event = Event::code_index_updated("/projects/acme", "indexing", 12, 0);
        assert_eq!(event.kind, "code.index.updated");
        assert_eq!(event.directory, "/projects/acme");
        assert_eq!(event.properties["status"], "indexing");
        assert_eq!(event.properties["files"], 12);
        assert_eq!(event.properties["symbols"], 0);
    }
}
