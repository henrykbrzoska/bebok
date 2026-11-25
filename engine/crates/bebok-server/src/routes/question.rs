//! `ask_user` answers: `GET /session/{id}/question` lists the questions a
//! session's turn is blocked on; `POST /session/{id}/question/{requestID}`
//! delivers the user's answers and unblocks the tool call.
//!
//! The registry and validation live in `bebok_core::agent::question_tool`;
//! this module only maps them to HTTP. The answer is single-use: the first
//! POST wins, later ones get 404.

use axum::Json;
use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use bebok_core::agent::{AnswerError, answer_question, pending_questions};

use crate::error::ApiError;

/// `GET /session/{id}/question` -> `{ questions: [ <question.asked properties> ] }`.
pub async fn list_questions(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "questions": pending_questions(&id) }))
}

#[derive(Debug, Deserialize)]
pub struct AnswerBody {
    /// `Record<questionId, string | string[]>`. Questions may be left out.
    pub answers: serde_json::Value,
}

/// `POST /session/{id}/question/{requestID}` -> unblock the `ask_user` call.
pub async fn answer_question_route(
    Path((id, request_id)): Path<(String, String)>,
    Json(body): Json<AnswerBody>,
) -> Result<Json<serde_json::Value>, Response> {
    match answer_question(&id, &request_id, &body.answers) {
        Ok(()) => Ok(Json(serde_json::json!({
            "requestID": request_id,
            "delivered": true,
        }))),
        Err(AnswerError::NotFound) => Err(ApiError::not_found(format!(
            "no pending question {request_id} in session {id}"
        ))
        .into_response()),
        Err(AnswerError::Invalid(message)) => Err(ApiError::bad_request(message).into_response()),
    }
}
