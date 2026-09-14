//! Cloud chats (1.8): sessions flagged `cloud` are mirrored to the relay's
//! Durable Object after every turn - meta plus a bounded tail of messages -
//! so a paired phone can still read them while the desktop is off. The
//! relay serves them only to device tokens listed as `readers` (sha256 of
//! the token, the same hash the device registry keeps), so the relay never
//! learns a token and an unpaired caller sees nothing.
//!
//! [`spawn_sync`] subscribes to the event bus once at boot: `session.updated`
//! (turn end) and `session.deleted` drive the push; toggling the flag
//! (`POST /session/{id}/cloud`) pushes or deletes immediately.

use std::sync::Arc;

use bebok_core::event::Event;
use bebok_core::store::InstanceStore;
use uuid::Uuid;

use super::RemoteState;
use super::relay::EngineFrame;

/// Newest messages kept in a snapshot (the phone reads a chat, not an archive).
const TAIL_MESSAGES: usize = 200;
/// Hard cap on the serialised snapshot; the tail shrinks until it fits.
const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;

/// Build the frame for `session`, or `None` when the session is not marked
/// `cloud`. `readers` come from the device registry at call time so a
/// revoked phone stops seeing new snapshots at the next push.
pub async fn snapshot_frame(
    store: &InstanceStore,
    remote: &RemoteState,
    session_id: Uuid,
) -> Option<EngineFrame> {
    let session = store.open_session(session_id).await.ok()?;
    let meta = session.meta_snapshot().await;
    if !meta.cloud {
        return None;
    }
    let readers: Vec<String> = remote
        .devices
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .list()
        .iter()
        .filter(|d| {
            !d.revoked
                && d.session
                    .as_deref()
                    .is_none_or(|s| s == session_id.to_string())
        })
        .map(|d| d.token_hash.clone())
        .collect();
    let messages = session.messages_snapshot().await;
    let mut tail = messages.len().min(TAIL_MESSAGES);
    let meta_value = serde_json::to_value(&meta).ok()?;
    loop {
        let slice = &messages[messages.len() - tail..];
        let data = serde_json::json!({
            "meta": meta_value,
            "messages": slice,
            "truncated": tail < messages.len(),
            "total": messages.len(),
        });
        let size = serde_json::to_vec(&data)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size <= MAX_SNAPSHOT_BYTES || tail == 0 {
            return Some(EngineFrame::Snapshot {
                session_id: session_id.to_string(),
                readers,
                data,
            });
        }
        tail /= 2;
    }
}

/// Push the current snapshot (or a delete when the flag is off) through the
/// relay, if one is connected. Cheap no-op otherwise.
pub async fn push(store: &InstanceStore, remote: &RemoteState, session_id: Uuid, deleted: bool) {
    let Some(handle) = remote.relay_handle() else {
        return;
    };
    if !handle.status.connected() {
        return;
    }
    if deleted {
        handle.send(EngineFrame::SnapshotDelete {
            session_id: session_id.to_string(),
        });
        return;
    }
    if let Some(frame) = snapshot_frame(store, remote, session_id).await {
        handle.send(frame);
    }
}

/// Subscribe to the bus and mirror flagged sessions after every turn.
pub fn spawn_sync(store: Arc<InstanceStore>, remote: Arc<RemoteState>) {
    let mut rx = store.bus().subscribe();
    tokio::spawn(async move {
        loop {
            let event: Event = match rx.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("cloud sync lagged, skipped {n} events");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let Ok(session_id) = Uuid::parse_str(&event.session_id) else {
                continue;
            };
            match event.kind.as_str() {
                // The turn-start event carries `running: true`; the end one does not.
                "session.updated"
                    if event.properties.get("running") != Some(&serde_json::json!(true)) =>
                {
                    push(&store, &remote, session_id, false).await;
                }
                "session.deleted" => push(&store, &remote, session_id, true).await,
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_only_for_cloud_sessions_and_bounded() {
        let dir = std::env::temp_dir().join(format!("bebok-cloud-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(InstanceStore::with_data_dir(dir.join("data")));
        let remote = RemoteState::global();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let session = store
            .create_session(project.to_string_lossy().as_ref(), "code", None)
            .await
            .unwrap();
        let id = session.meta_snapshot().await.id;

        assert!(snapshot_frame(&store, &remote, id).await.is_none());

        assert!(session.set_cloud(true).await);
        assert!(!session.set_cloud(true).await);
        for i in 0..400 {
            session
                .append_user_message(&format!("m{i} {}", "x".repeat(4000)))
                .await
                .unwrap();
        }
        let frame = snapshot_frame(&store, &remote, id)
            .await
            .expect("cloud snapshot");
        let EngineFrame::Snapshot {
            session_id, data, ..
        } = frame
        else {
            panic!("expected a snapshot frame");
        };
        assert_eq!(session_id, id.to_string());
        assert_eq!(data["meta"]["cloud"], true);
        assert_eq!(data["total"], 400);
        assert_eq!(data["truncated"], true);
        let messages = data["messages"].as_array().unwrap();
        assert!(messages.len() <= TAIL_MESSAGES);
        assert!(serde_json::to_vec(&data).unwrap().len() <= MAX_SNAPSHOT_BYTES);
        // The tail keeps the newest messages.
        assert!(
            messages.last().unwrap()["parts"]
                .to_string()
                .contains("m399")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
