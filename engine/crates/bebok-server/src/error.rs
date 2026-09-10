//! `ApiError` — the single error type for all handlers (`IntoResponse`).
//!
//! Replaces `err_response` / `pty_err_response` / manual
//! `(StatusCode, String)` tuples. Status-code contract is unchanged:
//! - `202` prompt accepted (returned directly, not an error)
//! - `409` session busy (`CoreError::SessionBusy`)
//! - `404` unknown session / ask id / pty / provider
//! - `400` bad request bodies / truncate preconditions / provider misconfig / fs errors / compact preconditions
//! - `403` invalid PTY ticket
//! - `502` provider `list_models` failure
//! - `500` everything else (pty spawn, config write, …)

use axum::http::StatusCode;
use axum::response::IntoResponse;
use bebok_core::error::CoreError;

/// Single handler error: maps to the same status codes (and trailing-newline
/// text bodies) the previous tuple/`err_response` code produced.
pub enum ApiError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Forbidden(String),
    BadGateway(String),
    Internal(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::BadGateway(msg.into())
    }

    #[allow(dead_code)]
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// Map a borrowed `CoreError` to the historical status code + text body.
    pub fn from_core(e: &CoreError) -> Self {
        match e {
            CoreError::SessionNotFound(_) => Self::NotFound(e.to_string()),
            CoreError::SessionBusy => Self::Conflict(e.to_string()),
            CoreError::ToolNotFound(_) => Self::BadRequest(e.to_string()),
            CoreError::BadRequest(_) => Self::BadRequest(e.to_string()),
            CoreError::ProviderConfig(_) => Self::BadRequest(e.to_string()),
            _ => Self::Internal(e.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, mut body) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m),
            Self::Conflict(m) => (StatusCode::CONFLICT, m),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            Self::BadGateway(m) => (StatusCode::BAD_GATEWAY, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        body.push('\n');
        (status, body).into_response()
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        Self::from_core(&e)
    }
}

/// Compat shim for the previous `err_response(&CoreError)` call sites.
pub fn err_response(e: &CoreError) -> axum::response::Response {
    ApiError::from_core(e).into_response()
}

#[cfg(not(target_os = "android"))]
impl From<bebok_pty::PtyError> for ApiError {
    fn from(e: bebok_pty::PtyError) -> Self {
        match &e {
            bebok_pty::PtyError::NotFound(_) => Self::NotFound(e.to_string()),
            _ => Self::Internal(e.to_string()),
        }
    }
}

#[cfg(not(target_os = "android"))]
#[allow(dead_code)]
pub fn pty_err_response(e: bebok_pty::PtyError) -> axum::response::Response {
    ApiError::from(e).into_response()
}

/// Handler result alias: `extract -> service -> json`, errors via `ApiError`.
#[allow(dead_code)]
pub type ApiResult<T> = Result<T, ApiError>;
