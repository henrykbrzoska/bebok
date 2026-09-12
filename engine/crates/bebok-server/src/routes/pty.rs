//! Terminal routes (M5): `POST /pty`, `GET /pty`,
//! `POST /pty/{id}/ticket`, `GET /pty/{id}/connect`.
//!
//! Unavailable on Android (`portable-pty`/`termios` does not compile there);
//! the whole module is gated with `#[cfg(not(target_os = "android"))]`.

use std::sync::Arc;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;

use bebok_pty::SpawnOptions;

use crate::error::ApiError;
use crate::state::AppState;

/// `POST /pty` body.
#[derive(Deserialize)]
pub struct CreatePtyBody {
    /// Working directory of the shell (project root).
    pub directory: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub title: Option<String>,
}

/// `GET /pty/{id}/connect` query.
#[derive(Deserialize)]
pub struct ConnectQuery {
    pub ticket: String,
}

/// `POST /pty` -> spawn a terminal session, return `{ ptyId }`.
pub async fn create_pty(
    State(state): State<AppState>,
    Json(body): Json<CreatePtyBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let opts = SpawnOptions {
        cwd: body.directory.map(std::path::PathBuf::from),
        rows: body.rows.unwrap_or(bebok_pty::DEFAULT_ROWS),
        cols: body.cols.unwrap_or(bebok_pty::DEFAULT_COLS),
        title: body.title,
        shell: None,
        command: None,
    };
    let session = state.ptys.spawn(opts).map_err(ApiError::from)?;
    let pty_id = session.id().to_string();

    // Publish `pty.exited` on the global event bus when the session terminates.
    let bus = state.store.bus();
    let mut exit_rx = session.exit_rx();
    let pty_for_event = pty_id.clone();
    tokio::spawn(async move {
        loop {
            if exit_rx.changed().await.is_err() {
                return;
            }
            if let Some(code) = *exit_rx.borrow() {
                bus.publish(
                    bebok_core::event::Event::new("pty.exited", "", &pty_for_event)
                        .with_properties(serde_json::json!({
                            "ptyId": pty_for_event,
                            "exitCode": code,
                        })),
                );
                return;
            }
        }
    });

    Ok(Json(serde_json::json!({ "ptyId": pty_id })))
}

/// `GET /pty` -> all terminal sessions (for listing + reattach).
pub async fn list_ptys(State(state): State<AppState>) -> Json<serde_json::Value> {
    let ptys = state.ptys.list();
    Json(serde_json::json!({ "ptys": ptys }))
}

/// `POST /pty/{id}/ticket` -> a one-time, short-lived, scope-bound ticket.
pub async fn pty_ticket(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ticket = state.ptys.issue_ticket(&id).map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({ "ptyId": id, "ticket": ticket })))
}

/// `GET /pty/{id}/connect?ticket=...` -> WebSocket upgrade; the ticket is
/// consumed atomically (a second connect with the same ticket is rejected).
pub async fn pty_connect(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ConnectQuery>,
) -> axum::response::Response {
    let Some(ticket_pty) = state.ptys.consume_ticket(&q.ticket) else {
        return ApiError::forbidden("invalid or expired ticket").into_response();
    };
    if ticket_pty != id {
        return ApiError::forbidden("ticket is not bound to this pty").into_response();
    }
    let Some(session) = state.ptys.get(&id) else {
        // Same 404 text as before (`StatusCode::NOT_FOUND, "unknown pty\n"`).
        return (StatusCode::NOT_FOUND, "unknown pty\n").into_response();
    };
    ws.on_upgrade(move |socket| handle_pty_socket(socket, session))
        .into_response()
}

/// Stream the scrollback dump (binary frames), then live bytes; parse control
/// frames (JSON text) for `resize` / `input`.
async fn handle_pty_socket(socket: WebSocket, session: Arc<bebok_pty::PtySession>) {
    let (mut tx, mut rx) = socket.split();
    let mut client = session.connect();
    let pty = session; // separate Arc for control (avoids borrow conflicts)

    // 1. Dump scrollback first, chunked so a ~1 MB history stays bounded.
    let scrollback = std::mem::take(&mut client.scrollback);
    for chunk in scrollback.chunks(65536) {
        if tx
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    // 2. Live stream + control frames.
    loop {
        tokio::select! {
            msg = rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => handle_control_frame(&pty, &text).await,
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => {} // binary / ping / pong ignored
                    Some(Err(_)) => return,
                }
            }
            chunk = client.recv() => {
                match chunk {
                    Some(bytes) => {
                        if tx.send(Message::Binary(bytes.to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                    None => return, // pty exited
                }
            }
        }
    }
}

/// Parse one JSON text control frame: `resize` or `input` (base64 payload).
async fn handle_control_frame(pty: &Arc<bebok_pty::PtySession>, text: &str) {
    #[derive(Deserialize)]
    struct ControlFrame {
        #[serde(rename = "type")]
        kind: String,
        cols: Option<u16>,
        rows: Option<u16>,
        data: Option<String>,
    }
    let Ok(frame) = serde_json::from_str::<ControlFrame>(text) else {
        return;
    };
    match frame.kind.as_str() {
        "resize" => {
            if let (Some(rows), Some(cols)) = (frame.rows, frame.cols) {
                let _ = pty.resize(rows, cols);
            }
        }
        "input" => {
            if let Some(data) = frame.data
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data)
            {
                let _ = pty.send_input(bytes).await;
            }
        }
        _ => {}
    }
}
