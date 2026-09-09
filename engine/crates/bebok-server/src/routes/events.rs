//! SSE route: `GET /event` — the single global event stream.

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::{Stream, stream};

use crate::state::AppState;

/// `GET /event` -> the single global SSE stream.
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.store.bus().subscribe();
    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let data =
                        serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                    return Some((Ok(SseEvent::default().data(data)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event stream lagged, dropped {n} events");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
