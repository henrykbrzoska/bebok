//! `/remote/*` handlers (WP-M1, F10-3).
//!
//! | route | scope | purpose |
//! |---|---|---|
//! | `POST /remote/enable` / `disable` | Local | persist `remote.enabled`, start/stop the listener |
//! | `POST /remote/pair/start` | Local | mint a one-time code (+ endpoints for the QR) |
//! | `POST /remote/pair` | none, **remote listener only** | phone claims the code, long-polls for the decision |
//! | `POST /remote/pair/confirm/{pairId}` | Local | create the device; the phone's poll returns the token |
//! | `POST /remote/pair/reject/{pairId}` | Local | the phone's poll returns 403 |
//! | `GET /remote/devices` | Local | list |
//! | `DELETE /remote/devices/{id}` | Local | revoke + remove (open streams end) |
//! | `GET /remote/status` | Local + Remote | `{enabled, listening, endpoints, devicesOnline, …}` |
//! | `POST /remote/heartbeat` | Local + Remote | update `last_seen` / `last_ip` |
//!
//! Bus events for the desktop UI (WP-M4): `remote.status`,
//! `remote.pair.request {pairId, deviceName, model, platform, ip, expiresAt}`,
//! `remote.device.changed {deviceId, change: created|removed|seen}`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, Extension, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use bebok_core::event::Event;

use super::devices::now_ms;
use super::pairing::{CONFIRM_WAIT, Outcome, PairError, PairRequest};
use super::{Listener, RemoteState, RequestScope};
use crate::state::AppState;

/// `{ "error": code, "message": text }` with a status.
pub fn error_json(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": code, "message": message.into() })),
    )
        .into_response()
}

fn pair_err(e: PairError) -> Response {
    error_json(e.status(), e.code(), e.to_message())
}

impl PairError {
    fn to_message(self) -> &'static str {
        match self {
            PairError::Locked => "too many wrong codes from this address; try again in 10 minutes",
            PairError::InvalidCode => "unknown pairing code",
            PairError::Expired => "pairing code expired",
            PairError::AlreadyRequested => "another device already claimed this code",
            PairError::NotFound => "unknown pairing request",
            PairError::NotRequested => "no device has sent this code yet",
        }
    }
}

/// The remote state, or 503 when the router was built without it.
fn remote(ext: Option<Extension<Arc<RemoteState>>>) -> Result<Arc<RemoteState>, Response> {
    ext.map(|Extension(s)| s).ok_or_else(|| {
        error_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "remote_unavailable",
            "remote module not attached to this router",
        )
    })
}

fn publish_status(state: &AppState, remote: &RemoteState) {
    state
        .store
        .bus()
        .publish(Event::new("remote.status", "", "").with_properties(remote.status_json()));
}

fn publish_device_changed(state: &AppState, device_id: &str, change: &str) {
    state.store.bus().publish(
        Event::new("remote.device.changed", "", "")
            .with_properties(serde_json::json!({ "deviceId": device_id, "change": change })),
    );
}

/// `GET /remote/status`.
pub async fn status(
    ext: Option<Extension<Arc<RemoteState>>>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    Ok(Json(remote.status_json()))
}

/// `POST /remote/heartbeat` — a paired phone says hello (updates
/// `last_seen` / `last_ip`). No-op for the local scope.
pub async fn heartbeat(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
    parts: Parts,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    let scope = parts.extensions.get::<RequestScope>().cloned();
    if let Some(RequestScope::Remote { device_id, .. }) = scope {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string())
            .unwrap_or_default();
        let touched = {
            let mut reg = remote.devices.write().unwrap_or_else(|e| e.into_inner());
            let touched = reg.touch(&device_id, &ip);
            if touched && let Err(e) = reg.save() {
                tracing::warn!("device registry save failed: {e}");
            }
            touched
        };
        if touched {
            publish_device_changed(&state, &device_id, "seen");
        }
    }
    Ok(Json(
        serde_json::json!({ "ok": true, "serverTime": now_ms() }),
    ))
}

/// `POST /remote/enable` — persist `remote.enabled=true` and bind the
/// listener now (no engine restart). 200 even when no interface is
/// eligible (`listening: false`, logged), so the UI can explain.
pub async fn enable(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    remote
        .persist_enabled(true)
        .map_err(|e| error_json(StatusCode::INTERNAL_SERVER_ERROR, "config_write", e))?;
    if !remote.is_listening() {
        // The served router (registered by `server::build_app`); the remote
        // listener wraps it with the `Listener::Remote` tag.
        let Some(app) = remote.app() else {
            return Err(error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "remote_unavailable",
                "no router registered for the remote listener",
            ));
        };
        if let Err(e) = super::listener::start_from_config(app, &remote).await {
            tracing::warn!("remote listener failed to start: {e}");
        }
    }
    publish_status(&state, &remote);
    Ok(Json(remote.status_json()))
}

/// `POST /remote/disable` — persist `remote.enabled=false`, stop the
/// listener (open remote streams end with the connection).
pub async fn disable(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    remote
        .persist_enabled(false)
        .map_err(|e| error_json(StatusCode::INTERNAL_SERVER_ERROR, "config_write", e))?;
    if let Some(handle) = remote.set_listener(None) {
        handle.stop();
    }
    publish_status(&state, &remote);
    Ok(Json(remote.status_json()))
}

/// `POST /remote/pair/start` (Local) — mint a code for the QR.
pub async fn pair_start(
    ext: Option<Extension<Arc<RemoteState>>>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    if !remote.is_listening() {
        return Err(error_json(
            StatusCode::CONFLICT,
            "remote_disabled",
            "enable remote access first (no remote listener is running)",
        ));
    }
    let started = remote
        .pairing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .start(Instant::now(), now_ms());
    Ok(Json(serde_json::json!({
        "pairId": started.pair_id,
        "code": started.code,
        "expiresAt": started.expires_at_ms,
        "endpoints": remote.endpoints(),
        "engineName": remote.engine_name(),
        "fingerprint": remote.fingerprint(),
    })))
}

#[derive(Debug, serde::Deserialize)]
pub struct PairBody {
    pub code: String,
    #[serde(rename = "deviceName", default)]
    pub device_name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub platform: String,
}

/// `POST /remote/pair` (unauthenticated, remote listener only) — the phone
/// presents the code and long-polls (≤ 90 s) for the desktop's decision.
pub async fn pair(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
    parts: Parts,
    Json(body): Json<PairBody>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    if parts.extensions.get::<Listener>().copied() != Some(Listener::Remote) {
        // Never on the loopback port: the code must come from the network
        // side the phone actually uses.
        return Err(error_json(
            StatusCode::NOT_FOUND,
            "not_found",
            "pairing is only accepted on the remote listener",
        ));
    }
    let ip = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let device_name = if body.device_name.trim().is_empty() {
        "Phone".to_string()
    } else {
        body.device_name.trim().chars().take(64).collect()
    };
    let request = PairRequest {
        device_name: device_name.clone(),
        model: body.model.trim().chars().take(64).collect(),
        platform: body.platform.trim().chars().take(32).collect(),
        ip: ip.clone(),
    };
    let (pair_id, rx) = remote
        .pairing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .request(&body.code, request, Instant::now())
        .map_err(|e| {
            tracing::warn!(ip = %ip, "pairing attempt refused: {}", e.code());
            pair_err(e)
        })?;
    let expires_at = remote
        .pairing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expires_at_ms(&pair_id);
    tracing::info!(ip = %ip, device = %device_name, "pairing request {pair_id}");
    state
        .store
        .bus()
        .publish(
            Event::new("remote.pair.request", "", "").with_properties(serde_json::json!({
                "pairId": pair_id,
                "deviceName": device_name,
                "model": body.model,
                "platform": body.platform,
                "ip": ip,
                "expiresAt": expires_at,
            })),
        );

    match tokio::time::timeout(CONFIRM_WAIT, rx).await {
        Ok(Ok(Outcome::Confirmed { device_id, token })) => Ok(Json(serde_json::json!({
            "deviceId": device_id,
            "token": token,
            "engineName": remote.engine_name(),
            "fingerprint": remote.fingerprint(),
        }))),
        Ok(Ok(Outcome::Rejected)) => Err(error_json(
            StatusCode::FORBIDDEN,
            "pair_rejected",
            "the desktop rejected this device",
        )),
        Ok(Err(_)) => {
            // Sender dropped without an answer (should not happen).
            remote
                .pairing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .abandon(&pair_id);
            Err(error_json(
                StatusCode::GONE,
                "pair_expired",
                "pairing request was dropped",
            ))
        }
        Err(_) => {
            remote
                .pairing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .abandon(&pair_id);
            Err(error_json(
                StatusCode::REQUEST_TIMEOUT,
                "pair_timeout",
                "the desktop did not confirm within 90 s",
            ))
        }
    }
}

/// `POST /remote/pair/confirm/{pairId}` (Local) — create the device and
/// release the phone's poll with the token. Returns the public device.
pub async fn pair_confirm(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
    Path(pair_id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    let (request, reply) = remote
        .pairing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take_requested(&pair_id)
        .map_err(pair_err)?;
    let created = {
        let mut reg = remote.devices.write().unwrap_or_else(|e| e.into_inner());
        reg.create(
            &request.device_name,
            &request.model,
            &request.platform,
            &request.ip,
        )
    };
    let (device, token) = match created {
        Ok(v) => v,
        Err(e) => {
            let _ = reply.send(Outcome::Rejected);
            let status = match e {
                super::devices::RegistryError::Full => StatusCode::CONFLICT,
                super::devices::RegistryError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return Err(error_json(status, "device_registry", e.to_string()));
        }
    };
    if reply
        .send(Outcome::Confirmed {
            device_id: device.id.clone(),
            token,
        })
        .is_err()
    {
        // The phone gave up while the user was clicking: do not keep a
        // device whose token nobody received.
        let mut reg = remote.devices.write().unwrap_or_else(|e| e.into_inner());
        let _ = reg.remove(&device.id);
        return Err(error_json(
            StatusCode::GONE,
            "pair_expired",
            "the device stopped waiting; start pairing again",
        ));
    }
    tracing::info!(device = %device.id, name = %device.name, "device paired");
    publish_device_changed(&state, &device.id, "created");
    publish_status(&state, &remote);
    Ok(Json(device.public()))
}

/// `POST /remote/pair/reject/{pairId}` (Local).
pub async fn pair_reject(
    ext: Option<Extension<Arc<RemoteState>>>,
    Path(pair_id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    let (_, reply) = remote
        .pairing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take_requested(&pair_id)
        .map_err(pair_err)?;
    let _ = reply.send(Outcome::Rejected);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /remote/devices` (Local).
pub async fn list_devices(
    ext: Option<Extension<Arc<RemoteState>>>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    let devices: Vec<serde_json::Value> = remote
        .devices
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .list()
        .iter()
        .map(|d| d.public())
        .collect();
    Ok(Json(serde_json::json!({ "devices": devices })))
}

/// `DELETE /remote/devices/{id}` (Local) — revoke and remove. Open SSE
/// streams of the device end within one revoke poll; its next request is
/// 401.
pub async fn delete_device(
    State(state): State<AppState>,
    ext: Option<Extension<Arc<RemoteState>>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let remote = remote(ext)?;
    let removed = {
        let mut reg = remote.devices.write().unwrap_or_else(|e| e.into_inner());
        let existed = reg.revoke(&id).map_err(|e| {
            error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "device_registry",
                e.to_string(),
            )
        })?;
        if existed {
            reg.remove(&id).map_err(|e| {
                error_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "device_registry",
                    e.to_string(),
                )
            })?;
        }
        existed
    };
    if !removed {
        return Err(error_json(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "unknown device",
        ));
    }
    tracing::info!(device = %id, "device revoked");
    publish_device_changed(&state, &id, "removed");
    publish_status(&state, &remote);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    //! Integration tests over real sockets: a loopback "local" listener and
    //! a second "remote" listener on `127.0.0.1:0` (what
    //! `BEBOK_REMOTE_BIND_TEST=127.0.0.1` does for a whole engine process),
    //! two `reqwest` clients (desktop + phone).

    use std::sync::Arc;
    use std::time::Duration;

    use axum::Extension;
    use bebok_core::config::model::RemoteConfig;
    use bebok_core::event::Event;

    use crate::routes::remote::RemoteState;
    use crate::state::AppState;

    struct Harness {
        local: String,
        remote_url: String,
        remote: Arc<RemoteState>,
        state: AppState,
        registry_path: std::path::PathBuf,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }

    async fn harness() -> Harness {
        let data = tempfile::tempdir().unwrap();
        let state = AppState {
            store: bebok_core::InstanceStore::with_data_dir(data.path().join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(data.path().join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
        };
        let (remote, cfg_dir) = crate::routes::remote::tests::temp_state(RemoteConfig {
            enabled: true,
            allow_lan: true,
            ..RemoteConfig::default()
        });
        let app = crate::routes::build_api_router()
            .layer(crate::cors::cors_layer())
            .layer(Extension(remote.clone()))
            .with_state(state.clone());
        remote.set_app(app.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app.clone()).await.unwrap();
        });
        // Bind the test address explicitly (port 0 = free port) instead of
        // enumerating real interfaces.
        let handle = crate::routes::remote::listener::start(
            remote.app().unwrap(),
            &["127.0.0.1".parse().unwrap()],
            0,
        )
        .await
        .unwrap()
        .expect("remote listener bound");
        remote.set_listener(Some(handle.clone()));
        Harness {
            local,
            remote_url: handle.endpoints()[0].clone(),
            remote,
            state,
            registry_path: cfg_dir.path().join("remote").join("devices.json"),
            _dirs: (data, cfg_dir),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap()
    }

    fn launch_token() -> String {
        crate::auth::token().to_string()
    }

    /// Wait for the next bus event of `kind`.
    async fn wait_for(rx: &mut tokio::sync::broadcast::Receiver<Event>, kind: &str) -> Event {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(ev) = rx.recv().await
                    && ev.kind == kind
                {
                    return ev;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {kind} event within 10 s"))
    }

    /// Pair a phone through the full HTTP flow; returns `(device_id, token)`.
    async fn pair_phone(h: &Harness, name: &str) -> (String, String) {
        let desktop = client();
        let phone = client();
        let started: serde_json::Value = desktop
            .post(format!("{}/remote/pair/start", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let code = started["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 8);
        assert_eq!(started["endpoints"][0], h.remote_url);
        assert_eq!(started["fingerprint"].as_str().unwrap().len(), 8);
        assert!(started["expiresAt"].as_u64().unwrap() > 0);

        let mut bus_rx = h.state.store.bus().subscribe();
        let remote_url = h.remote_url.clone();
        let name = name.to_string();
        let poll = tokio::spawn(async move {
            phone
                .post(format!("{remote_url}/remote/pair"))
                .json(&serde_json::json!({
                    "code": code, "deviceName": name, "model": "Pixel 9", "platform": "android"
                }))
                .send()
                .await
                .unwrap()
        });
        let asked = wait_for(&mut bus_rx, "remote.pair.request").await;
        assert_eq!(asked.properties["pairId"], started["pairId"]);
        assert_eq!(asked.properties["ip"], "127.0.0.1");
        let confirmed = desktop
            .post(format!(
                "{}/remote/pair/confirm/{}",
                h.local,
                started["pairId"].as_str().unwrap()
            ))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(
            confirmed.status(),
            200,
            "{}",
            confirmed.text().await.unwrap()
        );
        let res = poll.await.unwrap();
        assert_eq!(res.status(), 200);
        let body: serde_json::Value = res.json().await.unwrap();
        let token = body["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 43);
        assert_eq!(body["fingerprint"], started["fingerprint"]);
        (body["deviceId"].as_str().unwrap().to_string(), token)
    }

    /// F10-2 + F10-3: start -> pair -> confirm -> authenticated requests,
    /// allowlist enforcement, launch token rejected on the remote port,
    /// code single-use, revoke -> 401.
    #[tokio::test]
    async fn pairing_flow_and_remote_scope_over_real_sockets() {
        let h = harness().await;
        let c = client();

        // Remote port: no token / launch token -> 401 (never accepted there).
        let res = c
            .get(format!("{}/session", h.remote_url))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
        let res = c
            .get(format!("{}/session", h.remote_url))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "launch token must not work remotely");
        // Local port: unauthenticated pairing is not accepted there.
        let res = c
            .post(format!("{}/remote/pair", h.local))
            .json(&serde_json::json!({ "code": "ABCDEFGH" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);

        // Wrong code from the phone: 404 invalid.
        let res = c
            .post(format!("{}/remote/pair", h.remote_url))
            .json(&serde_json::json!({ "code": "ZZZZZZZZ", "deviceName": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
        assert_eq!(
            res.json::<serde_json::Value>().await.unwrap()["error"],
            "pair_invalid_code"
        );

        let (device_id, token) = pair_phone(&h, "Pixel 9").await;

        // The device is listed (no hash), status counts it.
        let list: serde_json::Value = c
            .get(format!("{}/remote/devices", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list["devices"][0]["id"], device_id);
        assert_eq!(list["devices"][0]["name"], "Pixel 9");
        assert!(list["devices"][0].get("token_hash").is_none());
        assert!(list["devices"][0].get("tokenHash").is_none());

        // Allowed routes with the device token on the remote port.
        let status: serde_json::Value = c
            .get(format!("{}/remote/status", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["listening"], true);
        assert_eq!(status["endpoints"][0], h.remote_url);
        let res = c
            .post(format!("{}/remote/heartbeat", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);

        let project = tempfile::tempdir().unwrap();
        let session: serde_json::Value = c
            .post(format!("{}/session", h.remote_url))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "directory": project.path().to_string_lossy() }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let sid = session["sessionID"]
            .as_str()
            .expect("sessionID")
            .to_string();
        let res = c
            .get(format!("{}/session/{sid}/message", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        // `POST /session/{id}/prompt` passes the scope gate (an unknown
        // provider keeps the test off the network: 400 from the handler,
        // never 401/403).
        let res = c
            .post(format!("{}/session/{sid}/prompt", h.remote_url))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": "hi", "model": "nope/model" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400, "{}", res.text().await.unwrap());
        let res = c
            .post(format!("{}/session/{sid}/abort", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);

        // Denied routes: 403 {"error":"remote_scope"} on both ports.
        let delete_session = format!("/session/{sid}");
        for (method, path) in [
            ("PUT", "/fs/file"),
            ("GET", "/config"),
            ("POST", "/pty"),
            ("GET", "/remote/devices"),
            ("POST", "/remote/pair/start"),
            ("DELETE", delete_session.as_str()),
        ] {
            for base in [&h.remote_url, &h.local] {
                let req = match method {
                    "PUT" => c.put(format!("{base}{path}")).json(&serde_json::json!({})),
                    "POST" => c.post(format!("{base}{path}")).json(&serde_json::json!({})),
                    "DELETE" => c.delete(format!("{base}{path}")),
                    _ => c.get(format!("{base}{path}")),
                };
                let res = req.bearer_auth(&token).send().await.unwrap();
                assert_eq!(res.status(), 403, "{method} {path} on {base}");
                let body: serde_json::Value = res.json().await.unwrap();
                assert_eq!(body["error"], "remote_scope", "{method} {path}");
            }
        }

        // A second phone pairs with a fresh code and works independently.
        let (_, token2) = pair_phone(&h, "Second").await;
        let res = c
            .get(format!("{}/remote/status", h.remote_url))
            .bearer_auth(&token2)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);

        // Revoke the first device: its token is 401 immediately.
        let res = c
            .delete(format!("{}/remote/devices/{device_id}", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let res = c
            .get(format!("{}/session", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "revoked token");
        let res = c
            .delete(format!("{}/remote/devices/{device_id}", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
        // The registry on disk has only the second device.
        let reg = crate::routes::remote::devices::DeviceRegistry::load(h.registry_path.clone());
        assert_eq!(reg.len(), 1);
        assert!(reg.verify(&token).is_none());
        assert!(reg.verify(&token2).is_some());
        assert_eq!(h.remote.status_json()["devices"], 1);
    }

    /// The 6th wrong code from one address answers 429; a rejected pairing
    /// answers 403 to the phone; confirming an unknown pair is 404.
    #[tokio::test]
    async fn wrong_codes_lock_out_and_reject_answers_403() {
        let h = harness().await;
        let c = client();
        for i in 1..=5 {
            let res = c
                .post(format!("{}/remote/pair", h.remote_url))
                .json(&serde_json::json!({ "code": format!("WRONG{i:03}") }))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 404, "attempt {i}");
        }
        let res = c
            .post(format!("{}/remote/pair", h.remote_url))
            .json(&serde_json::json!({ "code": "WRONG006" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 429, "6th attempt is locked out");
        assert_eq!(
            res.json::<serde_json::Value>().await.unwrap()["error"],
            "pair_locked"
        );

        // Reject path (fresh harness: lockouts are per IP).
        let h = harness().await;
        let started: serde_json::Value = c
            .post(format!("{}/remote/pair/start", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pair_id = started["pairId"].as_str().unwrap().to_string();
        // Confirming before any phone sent the code: 409.
        let res = c
            .post(format!("{}/remote/pair/confirm/{pair_id}", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 409);
        let mut bus_rx = h.state.store.bus().subscribe();
        let phone = client();
        let url = format!("{}/remote/pair", h.remote_url);
        let code = started["code"].as_str().unwrap().to_string();
        let poll = tokio::spawn(async move {
            phone
                .post(url)
                .json(&serde_json::json!({ "code": code, "deviceName": "Evil" }))
                .send()
                .await
                .unwrap()
        });
        wait_for(&mut bus_rx, "remote.pair.request").await;
        let res = c
            .post(format!("{}/remote/pair/reject/{pair_id}", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let res = poll.await.unwrap();
        assert_eq!(res.status(), 403);
        assert_eq!(
            res.json::<serde_json::Value>().await.unwrap()["error"],
            "pair_rejected"
        );
        assert!(h.remote.devices.read().unwrap().list().is_empty());
        let res = c
            .post(format!("{}/remote/pair/confirm/nope", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
        // `pair/start` is refused while the listener is down.
        let res = c
            .post(format!("{}/remote/disable", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.json::<serde_json::Value>().await.unwrap()["listening"],
            false
        );
        let res = c
            .post(format!("{}/remote/pair/start", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 409);
    }

    /// Read SSE frames (`id`, `event`, `data`) until `n` frames or timeout.
    async fn read_frames(
        res: reqwest::Response,
        n: usize,
        timeout: Duration,
    ) -> Vec<(Option<String>, Option<String>, String)> {
        use futures::StreamExt as _;
        let mut frames = Vec::new();
        let mut buf = String::new();
        let mut body = res.bytes_stream();
        let deadline = tokio::time::Instant::now() + timeout;
        while frames.len() < n {
            let chunk = tokio::select! {
                c = body.next() => c,
                _ = tokio::time::sleep_until(deadline) => break,
            };
            let Some(Ok(chunk)) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(cut) = buf.find("\n\n") {
                let block = buf[..cut].to_string();
                buf = buf[cut + 2..].to_string();
                let (mut id, mut event, mut data) = (None, None, String::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("id:") {
                        id = Some(v.trim().to_string());
                    } else if let Some(v) = line.strip_prefix("event:") {
                        event = Some(v.trim().to_string());
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim());
                    }
                }
                if id.is_some() || event.is_some() || !data.is_empty() {
                    frames.push((id, event, data));
                }
            }
        }
        frames
    }

    fn kind_of(frame: &(Option<String>, Option<String>, String)) -> String {
        serde_json::from_str::<serde_json::Value>(&frame.2)
            .ok()
            .and_then(|v| v["type"].as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// F10-4: `id:` on every frame; `Last-Event-ID` replays exactly the
    /// missed events; a gap older than the ring buffer yields `resync`.
    #[tokio::test]
    async fn sse_ids_and_last_event_id_resume() {
        let h = harness().await;
        let c = client();
        let bus = h.state.store.bus();
        for i in 1..=10 {
            bus.publish(Event::new("session.updated", "C:/p", &format!("s{i}")));
        }
        // Missed the last 5.
        let res = c
            .get(format!("{}/event", h.local))
            .bearer_auth(launch_token())
            .header("last-event-id", "5")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let frames = read_frames(res, 5, Duration::from_secs(5)).await;
        assert_eq!(frames.len(), 5);
        let ids: Vec<&str> = frames.iter().map(|f| f.0.as_deref().unwrap()).collect();
        assert_eq!(ids, ["6", "7", "8", "9", "10"]);
        let first: serde_json::Value = serde_json::from_str(&frames[0].2).unwrap();
        assert_eq!(first["sessionID"], "s6");
        assert_eq!(first["seq"], 6);

        // A fresh stream gets live events with ids continuing the sequence.
        let res = c
            .get(format!("{}/event", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        bus.publish(Event::new("turn.end", "C:/p", "s11"));
        let frames = read_frames(res, 1, Duration::from_secs(5)).await;
        assert_eq!(frames[0].0.as_deref(), Some("11"));

        // 3 000 missed events: one `event: resync`, then live.
        for i in 0..3_000 {
            bus.publish(Event::new("session.updated", "C:/p", &format!("x{i}")));
        }
        let res = c
            .get(format!("{}/event", h.local))
            .bearer_auth(launch_token())
            .header("last-event-id", "11")
            .send()
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        bus.publish(Event::new("turn.end", "C:/p", "after"));
        let frames = read_frames(res, 2, Duration::from_secs(5)).await;
        assert_eq!(frames[0].1.as_deref(), Some("resync"));
        assert_eq!(frames[1].0.as_deref(), Some("3012"));
        // An id from a previous engine process: resync as well.
        let res = c
            .get(format!("{}/event", h.local))
            .bearer_auth(launch_token())
            .header("last-event-id", "999999")
            .send()
            .await
            .unwrap();
        let frames = read_frames(res, 1, Duration::from_secs(5)).await;
        assert_eq!(frames[0].1.as_deref(), Some("resync"));
    }

    /// F10-4 remote: the filtered stream never carries `browser.frame`
    /// without `?interest=browser`, drops `debug.log`, coalesces deltas, and
    /// ends with `event: revoked` when the device is removed. The per-device
    /// stream cap answers 429.
    #[tokio::test]
    async fn remote_stream_is_filtered_and_ends_on_revoke() {
        let h = harness().await;
        let c = client();
        let (device_id, token) = {
            let mut reg = h.remote.devices.write().unwrap();
            let (d, t) = reg.create("Phone", "", "android", "127.0.0.1").unwrap();
            (d.id, t)
        };
        let bus = h.state.store.bus();
        let res = c
            .get(format!("{}/event", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        // Let the fan-out task subscribe before publishing.
        tokio::time::sleep(Duration::from_millis(100)).await;
        bus.publish(Event::new("browser.frame", "C:/p", "s1"));
        bus.publish(Event::new("debug.log", "C:/p", "s1"));
        bus.publish(Event::new("remote.pair.request", "", ""));
        for i in 0..20 {
            bus.publish(
                Event::new("message.part.updated", "C:/p", "s1")
                    .with_properties(serde_json::json!({ "messageIndex": 1, "i": i })),
            );
        }
        bus.publish(Event::new("permission.asked", "C:/p", "s1"));
        let frames = read_frames(res, 3, Duration::from_secs(3)).await;
        let kinds: Vec<String> = frames.iter().map(kind_of).collect();
        assert!(
            !kinds
                .iter()
                .any(|k| k == "browser.frame" || k == "debug.log" || k == "remote.pair.request"),
            "{kinds:?}"
        );
        assert!(kinds.contains(&"permission.asked".to_string()), "{kinds:?}");
        assert!(
            kinds
                .iter()
                .filter(|k| *k == "message.part.updated")
                .count()
                <= 2,
            "{kinds:?}"
        );

        // With `?interest=browser` frames do arrive (1 fps).
        let res = c
            .get(format!("{}/event?interest=browser", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        bus.publish(Event::new("browser.frame", "C:/p", "s1"));
        let frames = read_frames(res, 1, Duration::from_secs(3)).await;
        assert_eq!(kind_of(&frames[0]), "browser.frame");

        // Two streams are fine, the third is refused.
        let s1 = c
            .get(format!("{}/event", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let s2 = c
            .get(format!("{}/event", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(s1.status(), 200);
        assert_eq!(s2.status(), 200);
        let s3 = c
            .get(format!("{}/event", h.remote_url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(s3.status(), 429);
        drop(s2);

        // Revoke: the open stream ends with `event: revoked` within the poll.
        let res = c
            .delete(format!("{}/remote/devices/{device_id}", h.local))
            .bearer_auth(launch_token())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let started = std::time::Instant::now();
        let frames = read_frames(s1, 50, Duration::from_secs(15)).await;
        assert_eq!(
            frames.last().and_then(|f| f.1.as_deref()),
            Some("revoked"),
            "{frames:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
