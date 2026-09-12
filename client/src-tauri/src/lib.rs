//! Bebok desktop shell (Tauri 2) - M3.
//!
//! Responsibilities:
//! 1. Spawn the engine binary as a sidecar (`bebok-server --port 0`, random
//!    port).
//! 2. Wait for the engine's `BEBOK_READY http://host:port` handshake on its
//!    stdout and expose `{ baseUrl }` to the webview through the `engine_info`
//!    command.
//! 3. Kill the sidecar when the app exits.
//! 4. WP-BROWSER2 (F7-6): open the browser viewer as a second webview window
//!    (`open_browser_viewer`), placed right of the main window.
//!
//! The directory picker and every REST/SSE call happen in the Angular client;
//! the shell only owns process plumbing. State lives in the engine, so
//! sessions survive GUI restarts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::Manager;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Connection info handed to the webview (same shape the client's
/// `transport.strategy.ts` expects from `engine_info`).
#[derive(Debug, Clone, Serialize)]
struct EngineConnectionInfo {
    #[serde(rename = "baseUrl")]
    base_url: String,
}

#[derive(Default)]
struct EngineState {
    /// Set once the engine printed `BEBOK_READY <url>`.
    ready: Mutex<Option<EngineConnectionInfo>>,
    /// The spawned sidecar, killed on exit.
    child: Mutex<Option<CommandChild>>,
}

impl EngineState {
    fn mark_ready(&self, info: EngineConnectionInfo) {
        *self.ready.lock().unwrap() = Some(info);
    }

    fn kill_child(&self) {
        if let Some(child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
        }
    }
}

/// Webview command: block (up to a few seconds) until the sidecar engine has
/// announced its random port, then return the URL.
#[tauri::command]
async fn engine_info(
    state: tauri::State<'_, Arc<EngineState>>,
) -> Result<EngineConnectionInfo, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(info) = state.ready.lock().unwrap().clone() {
            return Ok(info);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("engine sidecar did not become ready in time".to_string());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Window label for a session's browser viewer (one window per session).
/// Session ids are UUIDs; anything else is sanitised so the label stays a
/// valid Tauri window label (`[a-zA-Z0-9\-/:_]`).
fn viewer_label(session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    format!("browser-viewer-{safe}")
}

/// App-relative URL of the viewer route (query = session id, percent-encoded
/// conservatively: only unreserved characters pass through).
fn viewer_url(session_id: &str) -> String {
    let mut encoded = String::new();
    for b in session_id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char)
            }
            _ => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    format!("browser-view?session={encoded}")
}

/// Viewer window size (logical px): a 1280x800 page plus toolbar/status.
const VIEWER_SIZE: (f64, f64) = (1360.0, 980.0);
/// Gap between the main window and the viewer (logical px).
const VIEWER_GAP: f64 = 8.0;

/// Top-left corner (logical px) for the viewer: right of the main window,
/// same top edge. `None` when the main window geometry is unavailable.
fn position_right_of(
    outer_position: Option<(i32, i32)>,
    outer_size: Option<(u32, u32)>,
    scale_factor: f64,
) -> Option<(f64, f64)> {
    let (x, y) = outer_position?;
    let (w, _) = outer_size?;
    let scale = if scale_factor > 0.0 { scale_factor } else { 1.0 };
    Some(((x as f64 + w as f64) / scale + VIEWER_GAP, y as f64 / scale))
}

/// Webview command: open (or focus) the browser viewer window for a session.
/// Synchronous on purpose: window creation must happen on the main thread.
#[tauri::command]
fn open_browser_viewer(app: tauri::AppHandle, session_id: String) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("session_id must not be empty".to_string());
    }
    let label = viewer_label(&session_id);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }
    let mut builder = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(viewer_url(&session_id).into()),
    )
    .title("Bebok - Browser")
    .inner_size(VIEWER_SIZE.0, VIEWER_SIZE.1)
    .min_inner_size(640.0, 480.0);
    if let Some(main) = app.get_webview_window("main") {
        let position = position_right_of(
            main.outer_position().ok().map(|p| (p.x, p.y)),
            main.outer_size().ok().map(|s| (s.width, s.height)),
            main.scale_factor().unwrap_or(1.0),
        );
        if let Some((x, y)) = position {
            builder = builder.position(x, y);
        }
    }
    builder
        .build()
        .map(|_| ())
        .map_err(|err| format!("failed to open the browser viewer window: {err}"))
}

fn parse_ready(line: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(line);
    let prefix = "BEBOK_READY ";
    text.strip_prefix(prefix).map(|rest| rest.trim().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let engine_state = Arc::new(EngineState::default());
            app.manage(engine_state.clone());

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let sidecar = match handle.shell().sidecar("bebok-server") {
                    Ok(command) => command,
                    Err(err) => {
                        eprintln!("failed to resolve bebok-server sidecar: {err}");
                        return;
                    }
                };

                let (mut rx, child) = match sidecar.args(["--port", "0"]).spawn() {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("failed to spawn bebok-server sidecar: {err}");
                        return;
                    }
                };

                engine_state.child.lock().unwrap().replace(child);

                // Read stdout lines until BEBOK_READY (the plugin delivers
                // one Stdout event per line), then keep the stream drained so
                // the child never blocks on a full pipe.
                while let Some(event) = rx.recv().await {
                    match event {
                        CommandEvent::Stdout(line) => {
                            if engine_state.ready.lock().unwrap().is_none() {
                                if let Some(url) = parse_ready(&line) {
                                    tracing_log_to_stderr(&format!(
                                        "engine ready at {url} (sidecar)"
                                    ));
                                    engine_state.mark_ready(EngineConnectionInfo {
                                        base_url: url,
                                    });
                                }
                            }
                        }
                        CommandEvent::Stderr(line) => {
                            // Engine logs (info/warn) are useful in dev only.
                            tracing_log_to_stderr(&String::from_utf8_lossy(&line));
                        }
                        CommandEvent::Error(err) => {
                            tracing_log_to_stderr(&format!("engine sidecar error: {err}"));
                        }
                        CommandEvent::Terminated(payload) => {
                            tracing_log_to_stderr(&format!(
                                "engine sidecar exited: code={:?} signal={:?}",
                                payload.code, payload.signal
                            ));
                            break;
                        }
                        _ => {}
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![engine_info, open_browser_viewer])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }) {
                let state = app_handle.state::<Arc<EngineState>>();
                state.kill_child();
            }
        });
}

/// Tiny helper so the shell keeps printing engine logs to stderr without
/// pulling tracing into the desktop crate.
fn tracing_log_to_stderr(line: &str) {
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ready_extracts_the_url() {
        assert_eq!(
            parse_ready(b"BEBOK_READY http://127.0.0.1:8787/?token=abc
"),
            Some("http://127.0.0.1:8787/?token=abc".to_string())
        );
        assert_eq!(parse_ready(b"something else"), None);
    }

    #[test]
    fn viewer_label_is_per_session_and_label_safe() {
        assert_eq!(
            viewer_label("11111111-2222-3333-4444-555555555555"),
            "browser-viewer-11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(viewer_label("a/b:c d"), "browser-viewer-a_b_c_d");
    }

    #[test]
    fn viewer_url_targets_the_bare_route_with_an_encoded_session() {
        assert_eq!(viewer_url("abc-123"), "browser-view?session=abc-123");
        assert_eq!(viewer_url("a b&c"), "browser-view?session=a%20b%26c");
    }

    #[test]
    fn viewer_position_is_right_of_the_main_window_in_logical_px() {
        assert_eq!(
            position_right_of(Some((100, 50)), Some((1280, 860)), 1.0),
            Some((1388.0, 50.0))
        );
        // HiDPI: physical -> logical.
        assert_eq!(
            position_right_of(Some((200, 100)), Some((2560, 1720)), 2.0),
            Some((1388.0, 50.0))
        );
        assert_eq!(position_right_of(None, Some((1, 1)), 1.0), None);
        assert_eq!(position_right_of(Some((0, 0)), None, 1.0), None);
        // A bogus scale factor never divides by zero.
        assert_eq!(
            position_right_of(Some((0, 0)), Some((100, 100)), 0.0),
            Some((108.0, 0.0))
        );
    }
}
