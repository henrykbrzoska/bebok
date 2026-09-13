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

// Axum handlers intentionally return Response for HTTP errors. Boxing every
// error response to satisfy Clippy's size heuristic would add an allocation on
// the error path without improving the route contract.
#![allow(clippy::result_large_err)]

mod auth;
mod cli;
mod cors;
mod error;
mod middleware;
mod routes;
mod server;
mod services;
mod state;

/// Default `RUST_LOG` when the environment does not set one. chromiumoxide's
/// CDP handler logs a WARN for every event it cannot deserialise (~20 per
/// `browser_open` against current Chrome, all harmless), so it is capped at
/// ERROR to keep the engine log readable (E2E R12).
fn default_log_filter() -> &'static str {
    "info,chromiumoxide::handler=error"
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr; stdout stays clean for the `BEBOK_READY` line that
    // the Tauri shell (sidecar) parses to discover the random port.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_log_filter().into()),
        )
        .init();

    if std::env::var("ZAI_API_KEY").is_err() {
        tracing::warn!("ZAI_API_KEY is not set; prompts will fail until it is provided");
    }

    let spec = cli::parse_cli(&std::env::args().skip(1).collect::<Vec<_>>())?;
    let result = server::serve(spec).await;
    // F9-14: last-resort sweep (runtime-free) so a serve error or an early
    // return never leaves a background process behind.
    let swept = bebok_tools::processes::ProcessRegistry::global().kill_all_blocking();
    if swept > 0 {
        tracing::warn!("swept {swept} background process(es) still running at exit");
    }
    result
}
