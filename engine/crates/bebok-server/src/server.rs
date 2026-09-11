//! Server bootstrap: state assembly, router, `BEBOK_READY` handshake, serve.
//!
//! `stdout` carries exactly one line — `BEBOK_READY http://host:port` — which
//! the Tauri shell parses to discover the port. Everything else logs to
//! `stderr` via `tracing`.

use std::sync::Arc;

use axum::Router;
use axum::middleware;

#[cfg(not(target_os = "android"))]
use bebok_pty::PtyManager;

use bebok_core::{DebugLog, InstanceStore, LLM_TRACE};

use crate::cli::BindSpec;
use crate::cors::cors_layer;
use crate::middleware::log_http;
use crate::routes::build_api_router;
use crate::state::AppState;

/// Assemble shared state + the full router (same wiring as the old `main`).
pub fn build_app() -> (Router, AppState) {
    let store = InstanceStore::new();

    // Single debug.log file, cleared on startup (fresh every app open).
    let debug = Arc::new(DebugLog::new(
        bebok_core::config::global_config_path().with_file_name("debug.log"),
    ));
    {
        // Forward engine-side LLM debug events into the debug log.
        // Survives broadcast lag (a lagged receiver resyncs) and only exits
        // when the bus itself is closed, so the last request/response pair
        // is never silently dropped.
        let mut rx = store.bus().subscribe();
        let debug = debug.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if ev.kind == "debug.log" {
                            let p = &ev.properties;
                            debug.log(
                                p.get("source").and_then(|v| v.as_str()).unwrap_or("llm"),
                                p.get("kind").and_then(|v| v.as_str()).unwrap_or("request"),
                                p.get("title").and_then(|v| v.as_str()).unwrap_or("llm"),
                                p.get("detail").and_then(|v| v.as_str()).unwrap_or(""),
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("debug log forwarder lagged, skipped {n} events");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // LLM trace: last 2 request/response payloads (memory-only, global).
    // The LazyLock global is the single source of truth; turn.rs pushes there,
    // and both AppState and the debug route read from the same Arc.
    let llm_trace = LLM_TRACE.clone();

    let state = AppState {
        store,
        #[cfg(not(target_os = "android"))]
        ptys: Arc::new(PtyManager::new()),
        debug,
        llm_trace,
    };

    let app = build_api_router()
        // Image attachments: up to 5 images x 5 MiB base64 (~35 MB JSON).
        // Axum's default 2 MiB Json limit would reject those with 413 before
        // our validator runs, so disable it here; size rules live in
        // `bebok_core::agent::images::validate_agent_images`.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(state.clone(), log_http))
        .layer(cors_layer())
        .with_state(state.clone());
    (app, state)
}

/// Bind, print `BEBOK_READY`, and serve until Ctrl-C.
pub async fn serve(bind: BindSpec) -> anyhow::Result<()> {
    // On startup, detect interpreter/tool binary paths and persist them to the
    // global config when nothing is configured yet (idempotent).
    bebok_core::config::ensure_global_runtimes();

    let (app, state) = build_app();

    // Plugin host: attach the engine bus so every published event is observed
    // by registered plugins (event-observer fan-out). Registration happens
    // code-side, e.g. in an embedding app or a future cdylib loader.
    // (Subscribing here vs before route-building is unobservable: broadcast
    // delivers to all subscribers; attach is idempotent, first bus wins.)
    let bus = state.store.bus();
    bebok_core::PluginHost::global().attach(&bus).await;

    // `--port 0` lets the OS pick a free port; we must announce the real one.
    let addr = std::net::SocketAddr::new(bind.host, bind.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    tracing::info!("bebok engine listening on http://{actual}");

    // Machine-readable handshake for the parent (Tauri sidecar spawn).
    // The ONLY stdout line.
    println!("BEBOK_READY http://{actual}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
