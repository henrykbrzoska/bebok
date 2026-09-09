//! PTY manager crate (Milestone 5).
//!
//! Spawns per-session PTYs via [`portable_pty`] (the engine owns the terminal,
//! so it survives GUI restarts), buffers a ring-buffer scrollback, supports
//! live output fan-out and resize, and issues single-use connect tickets for
//! the WebSocket surface (browsers cannot set custom headers on WS upgrade).
//!
//! Concurrency model (a slow client must never block the PTY):
//! - A dedicated reader thread drains the master end, appends to the scrollback
//!   and fans bytes out to per-client bounded channels with `try_send` (never
//!   blocks; a lagging consumer is dropped with a gap).
//! - A dedicated writer thread writes client input into the master end.
//! - `resize` and `kill` go through the master/child handles behind mutexes.

mod scrollback;
mod shell;
mod session;
mod ticket;

#[cfg(windows)]
mod win;

pub mod manager;

pub use manager::{CommandSpec, PtyInfo, PtyManager, SpawnOptions};
pub use scrollback::Scrollback;
pub use session::{PtyClient, PtySession};
pub use shell::{default_shell, scrubbed_env};
pub use ticket::TicketStore;

/// Errors surfaced by the PTY layer.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("pty error: {0}")]
    Pty(#[from] anyhow::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown pty: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

/// Default scrollback capacity (~1 MB), overridable per instance.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 1024 * 1024;

/// Default terminal geometry when nothing else is known.
pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;
