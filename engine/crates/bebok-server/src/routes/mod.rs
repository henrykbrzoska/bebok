//! Routes facade: owns the route table (single place) and re-exports.
//!
//! Handlers stay thin (`extract -> service -> json`); all orchestration lives
//! in `services/*`. Future endpoints (Task 5 sidebar/topbar, Task 6 custom
//! CSS, …) register in `build_api_router` without changing this shape.

use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

pub mod common;
pub mod config;
pub mod debug;
pub mod events;
pub mod fs;
pub mod mcp;
pub mod meta;
#[cfg(not(target_os = "android"))]
pub mod pty;
pub mod session;


/// Build all API routes (same paths/methods as before; only module paths
/// changed). The caller adds middleware/CORS/state (see `server.rs`).
pub fn build_api_router() -> Router<AppState> {
    #[allow(unused_mut)]
    let mut router = Router::new()
        .route("/session", post(session::create_session).get(session::list_sessions))
        .route(
            "/session/{id}",
            get(session::get_session).delete(session::delete_session),
        )
        .route("/session/{id}/message", get(session::get_messages))
        .route("/session/{id}/prompt", post(session::prompt))
        .route("/session/{id}/abort", post(session::abort))
        .route("/session/{id}/task/{taskID}/abort", post(session::abort_task))
        .route("/session/{id}/export", get(session::export_session))
        .route("/session/{id}/compact", post(session::compact_session))
        .route("/session/{id}/truncate", post(session::truncate_session))
        .route(
            "/session/{id}/permission/{requestID}",
            post(session::permission_decision),
        )
        .route("/agent", get(meta::list_agents))
        .route("/mcp", get(mcp::list_mcp))
        .route("/mcp/{name}/toggle", post(mcp::toggle_mcp))
        .route("/config", get(config::get_config).put(config::put_config))
        .route("/docker", get(meta::check_docker_endpoint))
        .route("/models", get(meta::list_models))
        .route("/fs/tree", get(fs::fs_tree))
        .route("/fs/file", get(fs::fs_file).put(fs::fs_file_write))
        .route("/plugins", get(meta::list_plugins))
        .route("/event", get(events::event_stream))
        .route(
            "/debug/log",
            get(debug::debug_log).delete(debug::debug_clear),
        );

    // Terminal (PTY) is unavailable on Android (portable-pty/termios does not
    // compile there); everything else is identical.
    #[cfg(not(target_os = "android"))]
    {
        router = router
            .route("/pty", post(pty::create_pty).get(pty::list_ptys))
            .route("/pty/{id}/ticket", post(pty::pty_ticket))
            .route("/pty/{id}/connect", get(pty::pty_connect));
    }

    // Silence unused-mut on Android where no PTY routes are appended.
    let _ = &mut router;
    router
}
