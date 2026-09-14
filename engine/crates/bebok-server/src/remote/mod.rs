//! Remote access (WP-M1, release 1.6.0 "Mobile"): a second HTTP listener on
//! the tailnet / LAN interface that paired phones reach directly, with
//! per-device capability tokens restricted to a route allowlist.
//!
//! - [`devices`]   — device registry (`devices.json`, sha256 token hashes).
//! - [`scope`]     — the exhaustive route allowlist + per-device rate limit.
//! - [`listener`]  — binds `100.64.0.0/10` (Tailscale) and, opt-in, RFC1918.
//! - [`pairing`]   — one-time 8-char codes, TTL, attempts, confirmation.
//! - [`fanout`]    — SSE filter/coalescing for the remote scope.
//! - [`routes`]    — `/remote/*` handlers.
//! - [`relay`]     — 1.8: outbound WebSocket to a `bebok-relay` worker so phones
//!                   reach the engine from anywhere (same scope, same tokens).
//!
//! The shared state ([`RemoteState`]) is reached from handlers and the auth
//! layer through a request extension (`Extension<Arc<RemoteState>>`) added
//! in `server::build_app`, so the test routers that build `AppState` by hand
//! keep compiling unchanged; without the extension only the launch token is
//! accepted and the `/remote/*` handlers answer 503.

pub mod devices;
pub mod fanout;
pub mod listener;
pub mod pairing;
pub mod relay;
pub mod routes;
pub mod scope;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use bebok_core::config::model::RemoteConfig;

use devices::DeviceRegistry;
use listener::ListenerHandle;
use pairing::Pairing;
use scope::RateLimiter;

/// Which listener accepted the connection. Inserted into the request
/// extensions by [`mark_remote_listener`] on the remote listener; absent
/// (= `Local`) on the loopback one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    Local,
    Remote,
}

/// The authenticated scope of a request, inserted by `auth::require_token`
/// for handlers that behave differently per scope (`GET /event`,
/// `/remote/heartbeat`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestScope {
    Local,
    Remote {
        device_id: String,
        /// Registry generation at authentication time (revoke bumps it).
        generation: u64,
    },
}

/// Everything the remote module keeps between requests.
pub struct RemoteState {
    config: RwLock<RemoteConfig>,
    /// Where `remote.enabled` is persisted (`None` = memory only, tests).
    config_path: Option<PathBuf>,
    pub devices: RwLock<DeviceRegistry>,
    pub pairing: Mutex<Pairing>,
    limiter: Mutex<RateLimiter>,
    listener: Mutex<Option<ListenerHandle>>,
    /// Open `GET /event` streams per device id.
    sse_streams: Mutex<HashMap<String, usize>>,
    /// The fully layered, stateful router (set by `server::build_app`) that
    /// `POST /remote/enable` binds on the remote addresses at runtime.
    app: Mutex<Option<axum::Router>>,
    /// Per-install random id; its sha256 prefix is the pairing fingerprint.
    install_id: String,
    engine_name: String,
    /// The running relay task (1.8), if any.
    relay: Mutex<Option<relay::RelayHandle>>,
}

impl RemoteState {
    /// Production state: registry + install id under `<config dir>/remote/`,
    /// config from the global `config.json`.
    fn from_disk() -> Self {
        let global = bebok_core::config::global_config_path();
        let dir = global
            .parent()
            .map(|p| p.join("remote"))
            .unwrap_or_else(|| PathBuf::from("remote"));
        let config = bebok_core::config::read_layer_json(&global)
            .and_then(|v| v.get("remote").cloned())
            .map(|v| RemoteConfig::from_value(&v))
            .unwrap_or_default();
        let install_id = load_or_create_install_id(&dir.join("install-id"));
        Self::new(
            config,
            Some(global),
            DeviceRegistry::load(dir.join("devices.json")),
            install_id,
        )
    }

    /// Assemble a state (tests use this with a temp registry).
    pub fn new(
        config: RemoteConfig,
        config_path: Option<PathBuf>,
        devices: DeviceRegistry,
        install_id: String,
    ) -> Self {
        Self {
            config: RwLock::new(config),
            config_path,
            devices: RwLock::new(devices),
            pairing: Mutex::new(Pairing::default()),
            limiter: Mutex::new(RateLimiter::new()),
            listener: Mutex::new(None),
            sse_streams: Mutex::new(HashMap::new()),
            app: Mutex::new(None),
            install_id,
            engine_name: engine_name(),
            relay: Mutex::new(None),
        }
    }

    /// The process-wide state used by `server::build_app`.
    pub fn global() -> Arc<RemoteState> {
        static GLOBAL: LazyLock<Arc<RemoteState>> =
            LazyLock::new(|| Arc::new(RemoteState::from_disk()));
        GLOBAL.clone()
    }

    pub fn config(&self) -> RemoteConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Persist `remote.enabled` (and keep the in-memory copy in sync).
    pub fn persist_enabled(&self, enabled: bool) -> Result<(), String> {
        self.update_config(|cfg| cfg.enabled = enabled)
    }

    /// Mutate the in-memory `remote` section and persist the **whole**
    /// section (`with_set` replaces the top-level value, so a partial delta
    /// would drop the other keys on disk).
    pub fn update_config(&self, mutate: impl FnOnce(&mut RemoteConfig)) -> Result<(), String> {
        let snapshot = {
            let mut cfg = self.config.write().unwrap_or_else(|e| e.into_inner());
            mutate(&mut cfg);
            cfg.clone()
        };
        if let Some(path) = &self.config_path {
            let value = serde_json::to_value(&snapshot).map_err(|e| e.to_string())?;
            bebok_core::config::write_delta_to(path, &serde_json::json!({ "remote": value }))?;
        }
        Ok(())
    }

    // -- relay (1.8) --------------------------------------------------------

    pub fn relay_handle(&self) -> Option<relay::RelayHandle> {
        self.relay.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Start the relay task for the current config (no-op when it is
    /// disabled, has no URL, or is already running against the same URL).
    /// Mints and persists the tunnel secret on first use.
    pub fn start_relay(&self, app: axum::Router) -> Result<Option<relay::RelayHandle>, String> {
        let cfg = self.config();
        if !cfg.relay_enabled() {
            return Ok(None);
        }
        if let Some(existing) = self.relay_handle() {
            if existing.url == cfg.relay.url {
                return Ok(Some(existing));
            }
            existing.stop();
        }
        let secret = if cfg.relay.secret.is_empty() {
            let secret = relay::new_secret();
            self.update_config(|c| c.relay.secret = secret.clone())?;
            secret
        } else {
            cfg.relay.secret.clone()
        };
        let handle = relay::start(app, cfg.relay.url.clone(), self.install_id.clone(), secret);
        *self.relay.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.clone());
        Ok(Some(handle))
    }

    pub fn stop_relay(&self) {
        if let Some(handle) = self.relay.lock().unwrap_or_else(|e| e.into_inner()).take() {
            handle.stop();
        }
    }

    /// The relay endpoint phones use, when the relay is configured.
    pub fn relay_endpoint(&self) -> Option<String> {
        let cfg = self.config();
        cfg.relay_enabled()
            .then(|| relay::phone_endpoint(&cfg.relay.url, &self.install_id))
    }

    pub fn relay_status_json(&self) -> serde_json::Value {
        let cfg = self.config();
        let handle = self.relay_handle();
        serde_json::json!({
            "enabled": cfg.relay.enabled,
            "url": cfg.relay.url,
            "endpoint": self.relay_endpoint(),
            "running": handle.is_some(),
            "connected": handle.as_ref().map(|h| h.status.connected()).unwrap_or(false),
            "lastError": handle.as_ref().and_then(|h| h.status.last_error()),
            "requests": handle.as_ref().map(|h| h.status.requests.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0),
        })
    }

    /// Hostname shown at pairing.
    pub fn engine_name(&self) -> &str {
        &self.engine_name
    }

    /// First 8 hex chars of `sha256(install id)`: lets the phone recognise
    /// the engine it paired with across IP changes.
    pub fn fingerprint(&self) -> String {
        devices::hash_token(&self.install_id)[..8].to_string()
    }

    /// Verify a device token (constant time over the registry).
    pub fn verify_device(&self, token: &str) -> Option<devices::DeviceAuth> {
        self.devices
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .verify(token)
    }

    /// Current generation of a device (`None` = revoked/removed).
    pub fn device_generation(&self, id: &str) -> Option<u64> {
        self.devices
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generation(id)
    }

    /// One request against the per-device token bucket.
    pub fn rate_check(&self, device_id: &str) -> bool {
        self.limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .check(device_id)
    }

    /// Reserve one of the `MAX_SSE_PER_DEVICE` stream slots.
    pub fn acquire_sse_slot(self: &Arc<Self>, device_id: &str) -> Option<SseSlot> {
        let mut map = self.sse_streams.lock().unwrap_or_else(|e| e.into_inner());
        let count = map.entry(device_id.to_string()).or_insert(0);
        if *count >= scope::MAX_SSE_PER_DEVICE {
            return None;
        }
        *count += 1;
        Some(SseSlot {
            state: self.clone(),
            device_id: device_id.to_string(),
        })
    }

    /// Devices with an open SSE stream right now.
    pub fn streaming_devices(&self) -> usize {
        self.sse_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|n| **n > 0)
            .count()
    }

    /// Remember the served router so the listener can be (re)started later.
    pub fn set_app(&self, app: axum::Router) {
        *self.app.lock().unwrap_or_else(|e| e.into_inner()) = Some(app);
    }

    pub fn app(&self) -> Option<axum::Router> {
        self.app.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn listener_handle(&self) -> Option<ListenerHandle> {
        self.listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_listener(&self, handle: Option<ListenerHandle>) -> Option<ListenerHandle> {
        std::mem::replace(
            &mut *self.listener.lock().unwrap_or_else(|e| e.into_inner()),
            handle,
        )
    }

    /// `http://ip:port` for every bound remote address.
    /// LAN/tailnet endpoints first, the relay last: the phone races them
    /// and a direct route wins when it is reachable.
    pub fn endpoints(&self) -> Vec<String> {
        let mut endpoints = self
            .listener_handle()
            .map(|h| h.endpoints())
            .unwrap_or_default();
        if let Some(relay) = self.relay_endpoint() {
            endpoints.push(relay);
        }
        endpoints
    }

    pub fn is_listening(&self) -> bool {
        self.listener_handle().is_some()
    }

    /// The `GET /remote/status` payload (also the `remote.status` event).
    pub fn status_json(&self) -> serde_json::Value {
        let cfg = self.config();
        let seen_recently = self
            .devices
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .online_count(ONLINE_WINDOW_MS);
        serde_json::json!({
            "enabled": cfg.enabled,
            "listening": self.is_listening(),
            "endpoints": self.endpoints(),
            "devicesOnline": seen_recently.max(self.streaming_devices()),
            "devices": self.devices.read().unwrap_or_else(|e| e.into_inner()).len(),
            "engineName": self.engine_name,
            "fingerprint": self.fingerprint(),
            "port": cfg.port,
            "allowLan": cfg.allow_lan,
            "relay": self.relay_status_json(),
        })
    }
}

/// A device counts as online when seen within this window.
pub const ONLINE_WINDOW_MS: u64 = 30_000;

/// RAII guard for one SSE stream slot (released when the stream ends).
pub struct SseSlot {
    state: Arc<RemoteState>,
    device_id: String,
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        let mut map = self
            .state
            .sse_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(n) = map.get_mut(&self.device_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.device_id);
            }
        }
    }
}

/// Middleware layered on the remote listener only: tags the request so the
/// auth layer rejects the launch token and `/remote/pair` knows it is on the
/// right socket.
pub async fn mark_remote_listener(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(Listener::Remote);
    next.run(req).await
}

/// Which listener a request arrived on (`Local` when untagged).
pub fn listener_of(req: &Request) -> Listener {
    req.extensions()
        .get::<Listener>()
        .copied()
        .unwrap_or(Listener::Local)
}

/// Read (or create) the per-install random id.
fn load_or_create_install_id(path: &std::path::Path) -> String {
    if let Ok(text) = std::fs::read_to_string(path) {
        let id = text.trim().to_string();
        if id.len() >= 16 {
            return id;
        }
    }
    let id = devices::generate_token();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, &id) {
        tracing::warn!(
            "cannot persist remote install id at {}: {e}",
            path.display()
        );
    }
    id
}

/// Best-effort machine name without a hostname crate: env vars first
/// (`COMPUTERNAME` on Windows, `HOSTNAME` on unix), then `/etc/hostname`.
pub fn engine_name() -> String {
    for var in ["BEBOK_ENGINE_NAME", "COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string("/etc/hostname") {
        let v = text.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    "bebok".to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh state over a temp registry (no global config, no disk config).
    pub(crate) fn temp_state(cfg: RemoteConfig) -> (Arc<RemoteState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let state = RemoteState::new(
            cfg,
            Some(dir.path().join("config.json")),
            DeviceRegistry::load(dir.path().join("remote").join("devices.json")),
            "install-id-for-tests-0123456789".to_string(),
        );
        (Arc::new(state), dir)
    }

    #[test]
    fn fingerprint_is_8_hex_of_install_id_hash() {
        let (state, _dir) = temp_state(RemoteConfig::default());
        let fp = state.fingerprint();
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            fp,
            devices::hash_token("install-id-for-tests-0123456789")[..8]
        );
    }

    #[test]
    fn persist_enabled_writes_remote_section() {
        let (state, dir) = temp_state(RemoteConfig::default());
        state.persist_enabled(true).unwrap();
        assert!(state.config().enabled);
        let text = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["remote"]["enabled"], true);
        state.persist_enabled(false).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["remote"]["enabled"], false);
    }

    #[test]
    fn sse_slots_are_capped_per_device_and_released_on_drop() {
        let (state, _dir) = temp_state(RemoteConfig::default());
        let a = state.acquire_sse_slot("d1").expect("slot 1");
        let b = state.acquire_sse_slot("d1").expect("slot 2");
        assert!(
            state.acquire_sse_slot("d1").is_none(),
            "third stream refused"
        );
        let c = state
            .acquire_sse_slot("d2")
            .expect("other device has its own cap");
        assert_eq!(state.streaming_devices(), 2);
        drop(a);
        let d = state.acquire_sse_slot("d1").expect("slot freed by drop");
        drop(b);
        drop(d);
        assert_eq!(state.streaming_devices(), 1);
        drop(c);
        assert_eq!(state.streaming_devices(), 0);
    }

    #[test]
    fn install_id_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote").join("install-id");
        let first = load_or_create_install_id(&path);
        assert_eq!(first.len(), 43);
        assert_eq!(load_or_create_install_id(&path), first);
    }
}
