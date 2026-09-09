//! Bebok headless engine: axum HTTP + SSE server.
//!
//! Run modes (M3 desktop sidecar):
//! - `--port 0` binds an OS-assigned random port; the actual listening URL is
//!   printed on stdout as a single `BEBOK_READY http://host:port` line so a
//!   parent process (the Tauri shell) can discover it.
//! - defaults stay backwards compatible with M1/M2: `BEBOK_ADDR` env or
//!   `127.0.0.1:8787`.

mod api;

use std::io::Write;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use bebok_core::{DebugLog, InstanceStore};
#[cfg(not(target_os = "android"))]
use bebok_pty::PtyManager;
use tower_http::cors::CorsLayer;

/// Bind settings from CLI flags / `BEBOK_ADDR`, resolved by the caller.
struct BindSpec {
    host: std::net::IpAddr,
    port: u16,
}

/// Parse `--host`, `--port` (or `--addr`) CLI flags; unknown flags are errors.
fn parse_cli(args: &[String]) -> anyhow::Result<BindSpec> {
    let mut host: Option<std::net::IpAddr> = None;
    let mut port: Option<u16> = None;
    let mut addr = std::env::var("BEBOK_ADDR")
        .ok()
        .filter(|s| !s.is_empty());

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--host" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--host requires a value"))?;
                host = Some(v.parse()?);
            }
            "--port" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
                port = Some(v.parse()?);
            }
            "--addr" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--addr requires a value"))?;
                addr = Some(v.clone());
            }
            "--help" | "-h" => {
                eprintln!("usage: bebok-server [--host IP] [--port PORT] [--addr IP:PORT]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument '{other}' (see --help)"),
        }
    }

    // Precedence: explicit `--addr`/`--host`/`--port` flags override BEBOK_ADDR.
    if let Some(a) = addr {
        // Only used when neither host nor port was passed explicitly.
        if host.is_none() && port.is_none() {
            let parsed: std::net::SocketAddr = a.parse()?;
            return Ok(BindSpec {
                host: parsed.ip(),
                port: parsed.port(),
            });
        }
    }

    Ok(BindSpec {
        host: host.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        port: port.unwrap_or(8787),
    })
}

/// CORS origins allowed to talk to the local engine (SPEC §8). The webview
/// runs on `tauri://localhost` / `http://tauri.localhost` in builds and on
/// `http://localhost:4200` in browser dev. Extend via `BEBOK_CORS` (comma
/// separated); the defaults always apply.
fn cors_layer() -> CorsLayer {
    const DEFAULTS: &[&str] = &[
        "http://localhost:4200",
        "http://127.0.0.1:4200",
        "http://localhost:8787",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "tauri://localhost",
        "capacitor://localhost",
        "http://localhost",
    ];
    let mut origins: Vec<HeaderValue> = Vec::new();
    for o in DEFAULTS {
        if let Ok(v) = HeaderValue::from_str(o) {
            origins.push(v);
        }
    }
    if let Ok(extra) = std::env::var("BEBOK_CORS") {
        for o in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(v) = HeaderValue::from_str(o) {
                origins.push(v);
            }
        }
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<InstanceStore>,
    #[cfg(not(target_os = "android"))]
    pub ptys: Arc<PtyManager>,
    pub debug: Arc<DebugLog>,
}

/// Access-log middleware: records every HTTP request + response status in the
/// debug log (skips long-lived streams and the debug endpoints themselves).
async fn log_http(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let skip = path == "/event" || path.starts_with("/debug") || path.ends_with("/connect");
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    if !skip {
        let status = response.status();
        state.debug.log(
            "http",
            if status.is_success() { "response" } else { "error" },
            format!("{method} {path}{query}"),
            format!("{} ({}ms)", status.as_u16(), start.elapsed().as_millis()),
        );
    }
    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr; stdout stays clean for the `BEBOK_READY` line that
    // the Tauri shell (sidecar) parses to discover the random port.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    if std::env::var("ZAI_API_KEY").is_err() {
        tracing::warn!("ZAI_API_KEY is not set; prompts will fail until it is provided");
    }

    let spec = parse_cli(&std::env::args().skip(1).collect::<Vec<_>>())?;

    // On startup, detect interpreter/tool binary paths and persist them to the
    // global config when nothing is configured yet (idempotent).
    bebok_core::config::ensure_global_runtimes();

    let store = Arc::new(InstanceStore::new());

    // Plugin host: attach the engine bus so every published event is observed
    // by registered plugins (event-observer fan-out). Registration happens
    // code-side, e.g. in an embedding app or a future cdylib loader.
    bebok_core::PluginHost::global().attach(&store.bus()).await;

    // Single debug.log file, cleared on startup (fresh every app open).
    let debug = Arc::new(DebugLog::new(
        bebok_core::config::global_config_path().with_file_name("debug.log"),
    ));
    {
        // Forward engine-side LLM debug events into the debug log.
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
                    Err(_) => break,
                }
            }
        });
    }

    let state = AppState {
        store,
        #[cfg(not(target_os = "android"))]
        ptys: Arc::new(PtyManager::new()),
        debug,
    };
    let cors = cors_layer();
    #[allow(unused_mut)]
    let mut app = Router::new()
        .route("/session", post(api::create_session).get(api::list_sessions))
        .route("/session/{id}", get(api::get_session).delete(api::delete_session))
        .route("/session/{id}/message", get(api::get_messages))
        .route("/session/{id}/prompt", post(api::prompt))
        .route("/session/{id}/abort", post(api::abort))
        .route("/session/{id}/export", get(api::export_session))
        .route("/session/{id}/compact", post(api::compact_session))
        .route("/session/{id}/truncate", post(api::truncate_session))
        .route("/session/{id}/permission/{requestID}", post(api::permission_decision))
        .route("/agent", get(api::list_agents))
        .route("/mcp", get(api::list_mcp))
        .route("/mcp/{name}/toggle", post(api::toggle_mcp))
        .route("/config", get(api::get_config).put(api::put_config))
        .route("/docker", get(api::check_docker_endpoint))
        .route("/models", get(api::list_models))
        .route("/fs/tree", get(api::fs_tree))
        .route("/fs/file", get(api::fs_file).put(api::fs_file_write))
        .route("/plugins", get(api::list_plugins))
        .route("/event", get(api::event_stream))
        .route("/debug/log", get(api::debug_log).delete(api::debug_clear));

    // Terminal (PTY) is unavailable on Android (portable-pty/termios does not
    // compile there); everything else is identical.
    #[cfg(not(target_os = "android"))]
    {
        app = app
            .route("/pty", post(api::create_pty).get(api::list_ptys))
            .route("/pty/{id}/ticket", post(api::pty_ticket))
            .route("/pty/{id}/connect", get(api::pty_connect));
    }

    let app = app
        .layer(middleware::from_fn_with_state(state.clone(), log_http))
        .layer(cors)
        .with_state(state);

    // `--port 0` lets the OS pick a free port; we must announce the real one.
    let addr = std::net::SocketAddr::new(spec.host, spec.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    tracing::info!("bebok engine listening on http://{actual}");

    // Machine-readable handshake for the parent (Tauri sidecar spawn).
    println!("BEBOK_READY http://{actual}");
    let _ = std::io::stdout().flush();

    axum::serve(listener, app).await?;
    Ok(())
}
