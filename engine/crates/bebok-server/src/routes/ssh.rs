//! Persistent SSH sessions: `GET /ssh` and `POST /ssh/prompt/{requestID}`.
//!
//! The sessions themselves live in `bebok_tools::SshRegistry` (process-wide,
//! see `bebok-tools/src/ssh/`). This module only exposes the two things the
//! UI needs while a `ssh` tool call is blocked:
//!
//! * the list of open sessions (a status view), and
//! * the answer to a password prompt raised as an `ssh.prompt` event.
//!
//! Answering is deliberately not tied to a Bebok session: the prompt id is a
//! single-use capability minted by the engine, and the password travels from
//! the dialog straight into the `ssh` process — never into the transcript.

use axum::Json;
use axum::extract::Path;
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_tools::SshRegistry;

use crate::error::ApiError;

/// `GET /ssh` -> every open SSH session.
pub async fn list_ssh() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "sessions": SshRegistry::global().list() }))
}

#[derive(Debug, Deserialize)]
pub struct AnswerBody {
    /// The password (or passphrase) the user typed. Absent when `cancel`.
    #[serde(default)]
    pub answer: Option<String>,
    /// Dismiss the prompt: the pending `connect` fails instead of hanging.
    #[serde(default)]
    pub cancel: bool,
}

/// `POST /ssh/prompt/{requestID}` -> deliver (or cancel) a password prompt.
pub async fn answer_prompt(
    Path(request_id): Path<String>,
    Json(body): Json<AnswerBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let answer = if body.cancel {
        None
    } else {
        match body.answer.as_deref() {
            Some(value) if !value.is_empty() => Some(value.to_string()),
            _ => {
                return Err(
                    ApiError::bad_request("a password is required (or set cancel: true)")
                        .into_response(),
                );
            }
        }
    };
    let delivered = SshRegistry::global().answer_prompt(&request_id, answer);
    if !delivered {
        return Err(
            ApiError::not_found(format!("no pending SSH prompt {request_id}")).into_response(),
        );
    }
    Ok(Json(serde_json::json!({
        "requestID": request_id,
        "delivered": true,
        "cancelled": body.cancel,
    })))
}

/// `POST /ssh/{id}/close` -> close an open session.
pub async fn close_ssh(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if SshRegistry::global().close(&id) {
        Ok(Json(serde_json::json!({ "closed": id })))
    } else {
        Err(ApiError::not_found(format!("no open SSH session {id}")).into_response())
    }
}
