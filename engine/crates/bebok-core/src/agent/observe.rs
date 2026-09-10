//! Event observers: `emit_message` / `emit_part` / `emit_session` +
//! `title_from`.
//!
//! The turn loop never touches the bus directly for transcript updates; it
//! calls these Observer helpers, which fan events out to SSE clients and
//! registered plugins.

use crate::event::{Event, EventBus};
use crate::session::Session;
use crate::store::SessionState;

pub fn emit_message(bus: &EventBus, state: &SessionState, kind: &str, idx: usize) {
    bus.publish(
        Event::new(kind, state.directory(), &state.id().to_string()).with_properties(
            serde_json::json!({
                "messageIndex": idx,
            }),
        ),
    );
}

pub async fn emit_part(bus: &EventBus, state: &SessionState, kind: &str, idx: usize) {
    let snapshot = {
        let messages = state.messages.read().await;
        messages.get(idx).and_then(|m| serde_json::to_value(m).ok())
    };
    let properties = match snapshot {
        Some(message) => serde_json::json!({ "messageIndex": idx, "message": message }),
        None => serde_json::json!({ "messageIndex": idx }),
    };
    bus.publish(
        Event::new(kind, state.directory(), &state.id().to_string()).with_properties(properties),
    );
}

pub fn emit_session(bus: &EventBus, state: &SessionState, kind: &str) {
    bus.publish(Event::new(kind, state.directory(), &state.id().to_string()));
}

/// Summarize a session into a title (first user text) - M1 heuristic.
pub fn title_from(session: &Session, first_prompt: &str) -> Option<String> {
    if session.title.is_some() {
        return None;
    }
    let mut title: String = first_prompt.chars().take(60).collect();
    if title.is_empty() {
        return None;
    }
    if first_prompt.chars().count() > 60 {
        title.push('…');
    }
    Some(title)
}
