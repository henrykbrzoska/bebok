//! Relay client (1.8): the engine dials out to a `bebok-relay` worker
//! (Cloudflare Workers + Durable Objects, `relay/` in the repo) and serves
//! phone requests that arrive over that WebSocket exactly as the remote
//! listener would - every frame becomes an axum request tagged
//! `Listener::Remote`, so device tokens, the scope allowlist and rate limits
//! apply unchanged. The relay is a dumb pipe; nothing here trusts it.
//!
//! Wire format (`relay/src/protocol.ts`): JSON text frames, bodies base64.
//!
//!   relay -> engine: `req {id, method, path, headers, body}` | `cancel {id}` | `pong`
//!   engine -> relay: `res {id, status, headers}` | `chunk {id, data}` | `end {id}`
//!                    | `error {id, message}` | `ping`
//!                    | `snapshot {sessionId, readers, data}` | `snapshotDelete {sessionId}`
//!
//! One task owns the socket ([`run`]); each request runs in its own task so
//! a long SSE stream never blocks the others, and `cancel` (the phone went
//! away) aborts it. Reconnects with exponential backoff (1 s .. 60 s).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request};
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

use super::Listener;

const PING_INTERVAL: Duration = Duration::from_secs(30);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Response bodies are streamed in frames of at most this many bytes.
const CHUNK_BYTES: usize = 32 * 1024;

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum RelayFrame {
    Req {
        id: String,
        method: String,
        path: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body: Option<String>,
    },
    Cancel {
        id: String,
    },
    Pong,
}

/// Frames the engine sends; also used by the cloud snapshot push (Phase 4).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineFrame {
    Res {
        id: String,
        status: u16,
        headers: Vec<(String, String)>,
    },
    Chunk {
        id: String,
        data: String,
    },
    End {
        id: String,
    },
    Error {
        id: String,
        message: String,
    },
    Ping,
    /// Cloud snapshot push (wired in the session store, see `cloud.rs`).
    #[allow(dead_code)]
    Snapshot {
        #[serde(rename = "sessionId")]
        session_id: String,
        readers: Vec<String>,
        data: serde_json::Value,
    },
    #[allow(dead_code)]
    SnapshotDelete {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

/// Public tunnel id derived from the per-install id: stable, url-safe, and
/// not the install id itself (which also seeds the pairing fingerprint).
pub fn tunnel_id(install_id: &str, salt: &str) -> String {
    let digest = Sha256::digest(format!("bebok-relay:{install_id}:{salt}").as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// `https://worker/t/<tunnel>` - the endpoint phones use (advertised in the
/// pairing QR next to the LAN/tailnet ones).
pub fn phone_endpoint(url: &str, tunnel: &str) -> String {
    format!("{}/t/{tunnel}", url.trim_end_matches('/'))
}

fn engine_socket_url(url: &str, tunnel: &str) -> String {
    let base = url.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{base}")
    };
    format!("{ws}/t/{tunnel}/engine")
}

/// A 32-byte random secret, url-safe base64 (43 chars).
pub fn new_secret() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Live state of the relay task, shared with `RemoteState` for status.
#[derive(Debug, Default)]
pub struct RelayStatus {
    pub connected: AtomicBool,
    pub attempts: AtomicU64,
    pub requests: AtomicU64,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl RelayStatus {
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_error(&self, message: impl Into<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.into());
    }
}

/// Handle to a running relay task: stop it, read its status, push frames.
#[derive(Clone)]
pub struct RelayHandle {
    pub url: String,
    pub status: Arc<RelayStatus>,
    cancel: CancellationToken,
    outbound: mpsc::Sender<EngineFrame>,
}

impl RelayHandle {
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// Queue a frame for the relay (dropped when disconnected - snapshots
    /// are re-sent on the next session update anyway).
    #[allow(dead_code)]
    pub fn send(&self, frame: EngineFrame) {
        let _ = self.outbound.try_send(frame);
    }
}

/// Called whenever the socket connects or drops (the routes layer publishes
/// `remote.status` so the desktop UI flips its badge).
pub type OnConnectionChange = Arc<dyn Fn(bool) + Send + Sync>;

/// Spawn the relay task. `app` is the fully layered router (the same one the
/// remote listener serves).
pub fn start(
    app: Router,
    url: String,
    tunnel: String,
    secret: String,
    on_change: Option<OnConnectionChange>,
) -> RelayHandle {
    let status = Arc::new(RelayStatus::default());
    let cancel = CancellationToken::new();
    let (outbound, rx) = mpsc::channel::<EngineFrame>(64);
    let handle = RelayHandle {
        url: url.clone(),
        status: status.clone(),
        cancel: cancel.clone(),
        outbound,
    };
    tokio::spawn(run(app, url, tunnel, secret, status, cancel, rx, on_change));
    handle
}

#[allow(clippy::too_many_arguments)]
async fn run(
    app: Router,
    url: String,
    tunnel: String,
    secret: String,
    status: Arc<RelayStatus>,
    cancel: CancellationToken,
    mut outbound: mpsc::Receiver<EngineFrame>,
    on_change: Option<OnConnectionChange>,
) {
    let notify = |connected: bool| {
        if let Some(cb) = &on_change {
            cb(connected);
        }
    };
    let socket_url = engine_socket_url(&url, &tunnel);
    let mut backoff = BACKOFF_MIN;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        status.attempts.fetch_add(1, Ordering::Relaxed);
        match connect(&socket_url, &secret).await {
            Ok(stream) => {
                tracing::info!("relay connected: {socket_url}");
                status.connected.store(true, Ordering::Relaxed);
                notify(true);
                backoff = BACKOFF_MIN;
                let reason = serve(stream, &app, &status, &cancel, &mut outbound).await;
                status.connected.store(false, Ordering::Relaxed);
                notify(false);
                if cancel.is_cancelled() {
                    return;
                }
                tracing::warn!("relay disconnected ({reason}); reconnecting in {backoff:?}");
                status.set_error(reason);
            }
            Err(e) => {
                tracing::warn!("relay connect failed ({e}); retrying in {backoff:?}");
                status.set_error(e);
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(socket_url: &str, secret: &str) -> Result<WsStream, String> {
    let mut request = socket_url
        .into_client_request()
        .map_err(|e| format!("bad relay url: {e}"))?;
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|e| e.to_string())?,
    );
    let connect = tokio_tungstenite::connect_async(request);
    match tokio::time::timeout(Duration::from_secs(20), connect).await {
        Ok(Ok((stream, _))) => Ok(stream),
        Ok(Err(e)) => Err(match e {
            tokio_tungstenite::tungstenite::Error::Http(res) => {
                format!("relay refused: HTTP {}", res.status())
            }
            other => other.to_string(),
        }),
        Err(_) => Err("relay connect timed out".to_string()),
    }
}

/// Serve one connection until it drops or `cancel` fires; returns the reason.
async fn serve(
    stream: WsStream,
    app: &Router,
    status: &Arc<RelayStatus>,
    cancel: &CancellationToken,
    outbound: &mut mpsc::Receiver<EngineFrame>,
) -> String {
    let (mut sink, mut source) = stream.split();
    let (tx, mut rx) = mpsc::channel::<EngineFrame>(256);
    let mut inflight: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;

    let reason = loop {
        tokio::select! {
            _ = cancel.cancelled() => break "stopped".to_string(),
            _ = ping.tick() => {
                if send(&mut sink, &EngineFrame::Ping).await.is_err() {
                    break "ping failed".to_string();
                }
            }
            Some(frame) = rx.recv() => {
                if send(&mut sink, &frame).await.is_err() {
                    break "send failed".to_string();
                }
            }
            Some(frame) = outbound.recv() => {
                if send(&mut sink, &frame).await.is_err() {
                    break "send failed".to_string();
                }
            }
            incoming = source.next() => {
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => break "closed by relay".to_string(),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => break e.to_string(),
                };
                let frame: RelayFrame = match serde_json::from_str(&text) {
                    Ok(frame) => frame,
                    Err(e) => {
                        tracing::debug!("relay: ignoring frame: {e}");
                        continue;
                    }
                };
                match frame {
                    RelayFrame::Pong => {}
                    RelayFrame::Cancel { id } => {
                        if let Some(task) = inflight.remove(&id) {
                            task.abort();
                        }
                    }
                    RelayFrame::Req { id, method, path, headers, body } => {
                        status.requests.fetch_add(1, Ordering::Relaxed);
                        inflight.retain(|_, task| !task.is_finished());
                        let task = tokio::spawn(handle_request(
                            app.clone(),
                            tx.clone(),
                            id.clone(),
                            method,
                            path,
                            headers,
                            body,
                        ));
                        inflight.insert(id, task);
                    }
                }
            }
        }
    };
    for (_, task) in inflight.drain() {
        task.abort();
    }
    let _ = sink.close().await;
    reason
}

async fn send(
    sink: &mut futures::stream::SplitSink<WsStream, Message>,
    frame: &EngineFrame,
) -> Result<(), ()> {
    let text = serde_json::to_string(frame).map_err(|_| ())?;
    sink.send(Message::Text(text.into())).await.map_err(|_| ())
}

/// Run one relayed request through the router and stream the answer back.
async fn handle_request(
    app: Router,
    tx: mpsc::Sender<EngineFrame>,
    id: String,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
) {
    let request = match build_request(&method, &path, &headers, body.as_deref()) {
        Ok(req) => req,
        Err(message) => {
            let _ = tx.send(EngineFrame::Error { id, message }).await;
            return;
        }
    };
    let response = match app.oneshot(request).await {
        Ok(res) => res,
        Err(e) => {
            let _ = tx
                .send(EngineFrame::Error {
                    id,
                    message: format!("router error: {e}"),
                })
                .await;
            return;
        }
    };
    let (parts, body) = response.into_parts();
    let head = EngineFrame::Res {
        id: id.clone(),
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect(),
    };
    if tx.send(head).await.is_err() {
        return;
    }
    let mut body = body;
    let encoder = base64::engine::general_purpose::STANDARD;
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                if let Some(data) = frame.data_ref() {
                    for piece in data.chunks(CHUNK_BYTES) {
                        let chunk = EngineFrame::Chunk {
                            id: id.clone(),
                            data: encoder.encode(piece),
                        };
                        if tx.send(chunk).await.is_err() {
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx
                    .send(EngineFrame::Error {
                        id,
                        message: format!("body error: {e}"),
                    })
                    .await;
                return;
            }
        }
    }
    let _ = tx.send(EngineFrame::End { id }).await;
}

fn build_request(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> Result<Request<Body>, String> {
    if !path.starts_with('/') || path.contains("://") {
        return Err("relayed path must be absolute".to_string());
    }
    let method = Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let bytes = match body {
        Some(text) => base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|e| format!("body is not base64: {e}"))?,
        None => Vec::new(),
    };
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            continue;
        };
        builder = builder.header(name, value);
    }
    let mut request = builder.body(Body::from(bytes)).map_err(|e| e.to_string())?;
    // The same tag the remote listener sets: scope + rate limit apply.
    request.extensions_mut().insert(Listener::Remote);
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_id_is_stable_url_safe_and_not_the_install_id() {
        let id = tunnel_id("install-abc", "salt1");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, tunnel_id("install-abc", "salt1"));
        assert_ne!(id, tunnel_id("install-abd", "salt1"));
        // A reset (new salt) moves the engine to a fresh tunnel.
        assert_ne!(id, tunnel_id("install-abc", "salt2"));
        assert!(!id.contains("install"));
    }

    #[test]
    fn urls_are_derived_from_the_worker_origin() {
        let t = tunnel_id("x", "s");
        assert_eq!(
            phone_endpoint("https://r.workers.dev/", &t),
            format!("https://r.workers.dev/t/{t}")
        );
        assert_eq!(
            engine_socket_url("https://r.workers.dev", &t),
            format!("wss://r.workers.dev/t/{t}/engine")
        );
        assert_eq!(
            engine_socket_url("http://localhost:8787", &t),
            format!("ws://localhost:8787/t/{t}/engine")
        );
    }

    #[test]
    fn secrets_are_long_and_random() {
        let a = new_secret();
        let b = new_secret();
        assert!(a.len() >= 40);
        assert_ne!(a, b);
    }

    #[test]
    fn build_request_tags_remote_and_decodes_the_body() {
        let req = build_request(
            "POST",
            "/session?directory=%2Ftmp",
            &[("content-type".into(), "application/json".into())],
            Some(&base64::engine::general_purpose::STANDARD.encode(b"{}")),
        )
        .unwrap();
        assert_eq!(req.method(), Method::POST);
        assert_eq!(req.uri().path(), "/session");
        assert_eq!(req.extensions().get::<Listener>(), Some(&Listener::Remote));
        assert!(build_request("GET", "http://evil/x", &[], None).is_err());
        assert!(build_request("GET", "/x", &[], Some("not base64!")).is_err());
    }

    /// A fake relay: accepts the engine socket only with the expected
    /// secret, then plays the phone - sends one `req` and collects the answer.
    #[tokio::test]
    async fn dials_the_relay_with_the_secret_and_answers_a_relayed_request() {
        use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
        use axum::extract::{Path, State};
        use axum::routing::get;
        use tokio::sync::oneshot;

        #[derive(Clone)]
        struct Fake {
            secret: String,
            done: Arc<std::sync::Mutex<Option<oneshot::Sender<Vec<serde_json::Value>>>>>,
        }

        async fn engine_ws(
            State(fake): State<Fake>,
            Path(_id): Path<String>,
            headers: axum::http::HeaderMap,
            ws: WebSocketUpgrade,
        ) -> axum::response::Response {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if auth != format!("Bearer {}", fake.secret) {
                return axum::http::StatusCode::FORBIDDEN.into_response();
            }
            ws.on_upgrade(move |mut socket| async move {
                let req = serde_json::json!({
                    "type": "req", "id": "p1", "method": "GET", "path": "/probe",
                    "headers": [["x-test", "1"]], "body": null
                });
                socket
                    .send(AxMsg::Text(req.to_string().into()))
                    .await
                    .unwrap();
                let mut frames = Vec::new();
                while let Some(Ok(AxMsg::Text(text))) = socket.recv().await {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let is_end = value["type"] == "end";
                    if value["type"] != "ping" {
                        frames.push(value);
                    }
                    if is_end {
                        break;
                    }
                }
                if let Some(tx) = fake.done.lock().unwrap().take() {
                    let _ = tx.send(frames);
                }
            })
        }
        use axum::response::IntoResponse;

        let (done_tx, done_rx) = oneshot::channel();
        let fake = Fake {
            secret: "expected-secret".into(),
            done: Arc::new(std::sync::Mutex::new(Some(done_tx))),
        };
        let relay_app = Router::new()
            .route("/t/{id}/engine", get(engine_ws))
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, relay_app).await.unwrap() });

        let engine_app = Router::new().route(
            "/probe",
            get(|req: Request<Body>| async move {
                let tag = req.extensions().get::<Listener>().copied();
                let header = req.headers().get("x-test").is_some();
                format!("probe tag={tag:?} header={header}")
            }),
        );

        // Wrong secret: refused, the client keeps retrying (we just stop it).
        let wrong = start(
            engine_app.clone(),
            format!("http://{addr}"),
            tunnel_id("install-1", ""),
            "wrong".into(),
            None,
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!wrong.status.connected());
        assert!(
            wrong
                .status
                .last_error()
                .unwrap_or_default()
                .contains("403")
        );
        wrong.stop();

        let handle = start(
            engine_app,
            format!("http://{addr}"),
            tunnel_id("install-1", ""),
            "expected-secret".into(),
            None,
        );
        let frames = tokio::time::timeout(Duration::from_secs(5), done_rx)
            .await
            .expect("relay round-trip timed out")
            .unwrap();
        assert!(handle.status.connected());
        assert_eq!(frames[0]["type"], "res");
        assert_eq!(frames[0]["status"], 200);
        let body: String = frames
            .iter()
            .filter(|f| f["type"] == "chunk")
            .map(|f| {
                String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(f["data"].as_str().unwrap())
                        .unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(body, "probe tag=Some(Remote) header=true");
        assert_eq!(frames.last().unwrap()["type"], "end");
        handle.stop();
    }

    #[tokio::test]
    async fn relayed_requests_stream_through_the_router() {
        use axum::routing::get;
        let app = Router::new().route(
            "/hello",
            get(|req: Request<Body>| async move {
                let tag = req.extensions().get::<Listener>().copied();
                format!("hi {:?}", tag)
            }),
        );
        let (tx, mut rx) = mpsc::channel(8);
        handle_request(
            app,
            tx,
            "r1".into(),
            "GET".into(),
            "/hello".into(),
            vec![],
            None,
        )
        .await;
        let mut frames = Vec::new();
        while let Some(f) = rx.recv().await {
            frames.push(f);
        }
        assert!(matches!(&frames[0], EngineFrame::Res { id, status: 200, .. } if id == "r1"));
        let body: String = frames
            .iter()
            .filter_map(|f| match f {
                EngineFrame::Chunk { data, .. } => Some(
                    String::from_utf8(
                        base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .unwrap(),
                    )
                    .unwrap(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(body, "hi Some(Remote)");
        assert!(matches!(frames.last(), Some(EngineFrame::End { .. })));
    }
}
