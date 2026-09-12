//! Bebok headless engine: axum HTTP + SSE server.
//!
//! Run modes (M3 desktop sidecar):
//! - `--port 0` binds an OS-assigned random port; the actual listening URL is
//!   printed on stdout as a single `BEBOK_READY http://host:port` line so a
//!   parent process (the Tauri shell) can discover it.
//! - defaults stay backwards compatible with M1/M2: `BEBOK_ADDR` env or
//!   `127.0.0.1:8787`.
//!
//! Thin entry point: CLI parsing lives in `cli.rs`, router/state/middleware in
//! their own modules, orchestration in `server.rs`. `stdout` carries only
//! `BEBOK_READY`; all logs go to `stderr`.

mod auth;
mod cli;
mod cors;
mod error;
mod middleware;
mod routes;
mod server;
mod services;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr; stdout stays clean for the `BEBOK_READY` line that
    // the Tauri shell (sidecar) parses to discover the random port.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if std::env::var("ZAI_API_KEY").is_err() {
        tracing::warn!("ZAI_API_KEY is not set; prompts will fail until it is provided");
    }

    let spec = cli::parse_cli(&std::env::args().skip(1).collect::<Vec<_>>())?;
    server::serve(spec).await
}
