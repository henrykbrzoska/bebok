//! SSE route: `GET /event` — the single global event stream.
//!
//! WP-M1 (F10-4, closes F3-17): every frame carries `id: <seq>`; a client
//! reconnecting with `Last-Event-ID: <n>` first receives the buffered events
//! it missed (`seq > n`), or one `event: resync` (empty data) when the gap
//! is older than the bus ring buffer — then the live stream. For the
//! `remote` scope the stream is filtered/coalesced by `remote::fanout`
//! (`?interest=browser` opts into thumbnails) and ends with
//! `event: revoked` when the device is revoked.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{self, BoxStream, StreamExt as _};

use crate::routes::remote::fanout::{self, FanoutOptions, Frame};
use crate::routes::remote::{RemoteState, RequestScope};
use crate::state::AppState;

type SseStream = BoxStream<'static, Result<SseEvent, Infallible>>;

fn sse_event(ev: &bebok_core::event::Event) -> SseEvent {
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_string());
    SseEvent::default().id(ev.seq.to_string()).data(data)
}

fn sse_resync() -> SseEvent {
    SseEvent::default().event("resync").data("")
}

/// `Last-Event-ID` header (a plain `seq`); malformed values are ignored.
pub fn last_event_id(parts: &Parts) -> Option<u64> {
    parts
        .headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// `?interest=browser[,…]` — comma-separated interests.
pub fn wants_browser(parts: &Parts) -> bool {
    parts
        .uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == "interest")
        .any(|(_, v)| v.split(',').any(|i| i.trim() == "browser"))
}

/// `GET /event` -> the single global SSE stream.
pub async fn event_stream(
    State(state): State<AppState>,
    remote: Option<Extension<Arc<RemoteState>>>,
    parts: Parts,
) -> Result<Sse<axum::response::sse::KeepAliveStream<SseStream>>, Response> {
    let bus = state.store.bus();
    let replay = bus.subscribe_from(last_event_id(&parts));
    let scope = parts
        .extensions
        .get::<RequestScope>()
        .cloned()
        .unwrap_or(RequestScope::Local);

    let stream: SseStream = match (scope, remote) {
        (
            RequestScope::Remote {
                device_id,
                generation,
                session,
            },
            Some(Extension(remote)),
        ) => {
            let Some(slot) = remote.acquire_sse_slot(&device_id) else {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(serde_json::json!({
                        "error": "sse_limit",
                        "message": "too many open event streams for this device",
                    })),
                )
                    .into_response());
            };
            let cfg = remote.config();
            let opts = FanoutOptions {
                interest_browser: wants_browser(&parts),
                publish_browser_frames: cfg.publish.browser_frames,
                publish_processes: cfg.publish.processes,
            };
            let rx = fanout::spawn(
                replay,
                fanout::RemoteStream {
                    opts,
                    state: remote.clone(),
                    device_id,
                    generation,
                    session,
                    slot,
                },
            );
            stream::unfold(rx, |mut rx| async move {
                match rx.recv().await {
                    Some(Frame::Event(ev)) => Some((Ok(sse_event(&ev)), rx)),
                    Some(Frame::Resync) => Some((Ok(sse_resync()), rx)),
                    Some(Frame::Revoked) => {
                        rx.close();
                        Some((Ok(SseEvent::default().event("revoked").data("")), rx))
                    }
                    None => None,
                }
            })
            .boxed()
        }
        _ => {
            let mut prefix: Vec<Result<SseEvent, Infallible>> = Vec::new();
            if replay.resync {
                prefix.push(Ok(sse_resync()));
            }
            prefix.extend(replay.events.iter().map(|ev| Ok(sse_event(ev))));
            let live = stream::unfold(replay.rx, |mut rx| async move {
                match rx.recv().await {
                    Ok(ev) => Some((Ok(sse_event(&ev)), rx)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // The client can catch up on its own now: tell it.
                        tracing::warn!("event stream lagged, dropped {n} events");
                        Some((Ok(sse_resync()), rx))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                }
            });
            stream::iter(prefix).chain(live).boxed()
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn parts(uri: &str, last_id: Option<&str>) -> Parts {
        let mut b = Request::builder().uri(uri);
        if let Some(id) = last_id {
            b = b.header("last-event-id", id);
        }
        b.body(()).unwrap().into_parts().0
    }

    #[test]
    fn parses_last_event_id_and_interest() {
        assert_eq!(last_event_id(&parts("/event", Some("42"))), Some(42));
        assert_eq!(last_event_id(&parts("/event", Some(" 7 "))), Some(7));
        assert_eq!(last_event_id(&parts("/event", Some("abc"))), None);
        assert_eq!(last_event_id(&parts("/event", None)), None);
        assert!(wants_browser(&parts("/event?interest=browser", None)));
        assert!(wants_browser(&parts(
            "/event?x=1&interest=chat,browser",
            None
        )));
        assert!(!wants_browser(&parts("/event?interest=chat", None)));
        assert!(!wants_browser(&parts("/event", None)));
    }
}
