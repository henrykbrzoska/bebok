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
//! `agent.list.changed`, `config.changed`, `pty.exited` and
//! `plugin.changed` (plugin declaration installed / enabled / disabled:
//! `properties` carries `{ name, change }` with `change` one of
//! `installed` / `enabled` / `disabled`).

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
    fn plugin_install_progress_has_correct_kind_and_properties() {
        let event = Event::plugin_install_progress(
            "/projects/acme",
            "bebok-index",
            "download",
            "https://example.com/bebok-index.zip",
        );
        assert_eq!(event.kind, "plugin.install.progress");
        assert_eq!(event.directory, "/projects/acme");
        assert_eq!(event.properties["name"], "bebok-index");
        assert_eq!(event.properties["stage"], "download");
        assert_eq!(
            event.properties["detail"],
            "https://example.com/bebok-index.zip"
        );
    }
}
