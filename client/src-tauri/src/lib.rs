//! Bebok desktop shell (Tauri 2) - M3.
//!
//! Responsibilities:
//! 1. Spawn the engine binary as a sidecar (`bebok-server --port 0`, random
//!    port).
//! 2. Wait for the engine's `BEBOK_READY http://host:port` handshake on its
//!    stdout and expose `{ baseUrl }` to the webview through the `engine_info`
//!    command.
//! 3. Kill the sidecar when the app exits.
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
        .invoke_handler(tauri::generate_handler![engine_info])
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
