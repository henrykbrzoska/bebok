//! Global event bus (SPEC §3.11).
//!
//! One stream (`GET /event`), every event carries the envelope
//! `{ type, directory, sessionID, properties }` so clients can route by
//! directory. No polling anywhere.

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
