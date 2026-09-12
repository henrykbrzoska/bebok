//! PtyManager: the engine-side registry of terminal sessions.
//!
//! Keyed globally by a random `ptyId`. A session survives its GUI connection
//! (the WS may drop and reconnect with the same id), so the manager is
//! intentionally decoupled from the HTTP/WS layer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use portable_pty::{CommandBuilder, PtySize};
use serde::Serialize;

use crate::session::{self, PtySession};
use crate::shell::{default_shell, scrubbed_env};
use crate::ticket::TicketStore;
use crate::{DEFAULT_COLS, DEFAULT_ROWS, DEFAULT_SCROLLBACK_BYTES, PtyError, Scrollback};

/// A command to run instead of the default interactive shell (tests, one-shots).
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

/// Parameters for spawning a PTY.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Working directory of the child (project root).
    pub cwd: Option<PathBuf>,
    pub rows: u16,
    pub cols: u16,
    /// Shell override (config `terminal.shell`); default is per-OS.
    pub shell: Option<String>,
    /// Optional tab label.
    pub title: Option<String>,
    /// Run this instead of the shell (used by integration tests).
    pub command: Option<CommandSpec>,
}

impl SpawnOptions {
    fn rows_cols(&self) -> (u16, u16) {
        (
            if self.rows == 0 {
                DEFAULT_ROWS
            } else {
                self.rows
            },
            if self.cols == 0 {
                DEFAULT_COLS
            } else {
                self.cols
            },
        )
    }
}

/// Metadata for one terminal session (surfaces via `GET /pty`).
#[derive(Debug, Clone, Serialize)]
pub struct PtyInfo {
    pub pty_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub exited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
}

/// Engine-side registry of terminal sessions + one-time connect tickets.
pub struct PtyManager {
    sessions: RwLock<HashMap<String, Arc<PtySession>>>,
    tickets: TicketStore,
    max_scrollback: usize,
}

impl PtyManager {
    pub fn new() -> Self {
        Self::with_max_scrollback(DEFAULT_SCROLLBACK_BYTES)
    }

    pub fn with_max_scrollback(max_scrollback: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            tickets: TicketStore::new(),
            max_scrollback,
        }
    }

    /// Spawn a new terminal session and return its handle.
    pub fn spawn(&self, opts: SpawnOptions) -> Result<Arc<PtySession>, PtyError> {
        let (rows, cols) = opts.rows_cols();
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let command_desc = describe_command(&opts);
        let child = pair.slave.spawn_command(build_command(&opts))?;

        let id = uuid::Uuid::new_v4().to_string();
        let cwd = opts.cwd.as_ref().map(|p| p.to_string_lossy().to_string());
        let title = opts.title.clone();

        let session = session::spawn(
            id.clone(),
            cwd,
            command_desc,
            title,
            pair.master,
            child,
            Scrollback::new(self.max_scrollback),
        );

        self.sessions.write().unwrap().insert(id, session.clone());
        Ok(session)
    }

    /// Look up a session by id.
    pub fn get(&self, id: &str) -> Option<Arc<PtySession>> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// All known sessions (for reattach + listing in the GUI).
    pub fn list(&self) -> Vec<PtyInfo> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .values()
            .map(|s| PtyInfo {
                pty_id: s.id().to_string(),
                cwd: s.cwd.clone(),
                command: s.command.clone(),
                title: s.title.clone(),
                exited: s.is_exited(),
                exit_code: s.exit_code(),
            })
            .collect()
    }

    /// Issue a one-time, short-lived, scope-bound ticket for a session.
    pub fn issue_ticket(&self, id: &str) -> Result<String, PtyError> {
        if !self.sessions.read().unwrap().contains_key(id) {
            return Err(PtyError::NotFound(id.to_string()));
        }
        Ok(self.tickets.issue(id))
    }

    /// Atomically consume a connect ticket -> the pty id it was bound to.
    pub fn consume_ticket(&self, token: &str) -> Option<String> {
        self.tickets.consume(token)
    }

    /// Kill the process tree of a session (it is not removed from the registry).
    pub fn kill(&self, id: &str) -> Result<(), PtyError> {
        match self.get(id) {
            Some(session) => session.kill(),
            None => Err(PtyError::NotFound(id.to_string())),
        }
    }

    /// Remove a session from the registry (cleanup of exited terminals).
    pub fn remove(&self, id: &str) -> Option<Arc<PtySession>> {
        self.sessions.write().unwrap().remove(id)
    }
}

impl Default for PtyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the child command: default shell (or override/command), scrubbed env,
/// `TERM` set for color support, and the project cwd.
fn build_command(opts: &SpawnOptions) -> CommandBuilder {
    let mut cmd = match &opts.command {
        Some(spec) => {
            let mut c = CommandBuilder::new(&spec.program);
            c.args(&spec.args);
            c
        }
        None => CommandBuilder::new(opts.shell.clone().unwrap_or_else(default_shell)),
    };

    cmd.env_clear();
    for (key, value) in scrubbed_env() {
        cmd.env(key, value);
    }
    cmd.env("TERM", "xterm-256color");

    if let Some(cwd) = &opts.cwd {
        cmd.cwd(cwd);
    }
    cmd
}

/// Human-readable command for the session list.
fn describe_command(opts: &SpawnOptions) -> String {
    match &opts.command {
        Some(spec) => {
            let mut s = spec.program.clone();
            for arg in &spec.args {
                s.push(' ');
                s.push_str(arg);
            }
            s
        }
        None => opts.shell.clone().unwrap_or_else(default_shell),
    }
}
