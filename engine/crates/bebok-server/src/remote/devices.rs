//! Device registry (WP-M1, F10-1): per-device capability tokens for the
//! remote listener.
//!
//! One entry per paired phone, persisted at
//! `<global config dir>/remote/devices.json`:
//!
//! ```json
//! [{ "id": "…", "name": "Pixel 9", "token_hash": "<sha256 hex>",
//!    "created_at": 1710000000000, "last_seen": 1710000000000,
//!    "last_ip": "100.101.102.103", "revoked": false,
//!    "model": "Pixel 9", "platform": "android" }]
//! ```
//!
//! The token itself (32 random bytes, base64url) is shown exactly once at
//! pairing and never stored; verification hashes the presented token and
//! compares it against **every** entry with a constant-time equality
//! (`subtle`), so neither the prefix of a token nor the position of a device
//! in the file can be probed through timing.
//!
//! Revoking bumps a per-device generation counter (memory only): open SSE
//! streams poll it once per keep-alive and end when it changes (F10-4).

use std::path::PathBuf;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;

/// Upper bound on paired phones in the registry (revoked-but-kept entries count).
pub const MAX_DEVICES: usize = 5;
/// Upper bound on session share links (1.8), counted separately.
pub const MAX_SHARES: usize = 20;

/// One paired device (the persisted shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// `sha256(token)` as lowercase hex.
    pub token_hash: String,
    /// Unix milliseconds.
    pub created_at: u64,
    /// Unix milliseconds of the last authenticated request / heartbeat.
    pub last_seen: u64,
    #[serde(default)]
    pub last_ip: String,
    #[serde(default)]
    pub revoked: bool,
    /// Free-form device model reported at pairing (`Pixel 9`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// `android` / `ios` / `web` … as reported at pairing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub platform: String,
    /// 1.8 share links: the token is confined to this one session (see
    /// `scope::share_allowed`). `None` = a paired phone with the full remote scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Memory-only revocation generation: bumped on revoke/remove so open
    /// streams notice. Not persisted (a restart starts at 0 again, which is
    /// fine: a revoked entry is rejected by the flag anyway).
    #[serde(skip)]
    pub generation: u64,
}

impl Device {
    pub fn is_share(&self) -> bool {
        self.session.is_some()
    }
}

impl Device {
    /// The public view (no hash): what `GET /remote/devices` returns.
    pub fn public(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "createdAt": self.created_at,
            "lastSeen": self.last_seen,
            "lastIp": self.last_ip,
            "revoked": self.revoked,
            "model": self.model,
            "platform": self.platform,
            "session": self.session,
        })
    }
}

/// What a successful token check yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuth {
    pub device_id: String,
    pub generation: u64,
    /// Set for share-link tokens: the only session this caller may touch.
    pub session: Option<String>,
}

#[derive(Debug)]
pub enum RegistryError {
    Full,
    Io(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Full => write!(
                f,
                "device registry is full ({MAX_DEVICES} devices); remove one first"
            ),
            RegistryError::Io(e) => write!(f, "device registry io: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// The registry: a small vector, persisted as one JSON array.
#[derive(Debug, Default)]
pub struct DeviceRegistry {
    path: Option<PathBuf>,
    devices: Vec<Device>,
}

/// Current unix time in milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 32 random bytes, base64url without padding (43 chars). CSPRNG-backed
/// (`rand::rng()` is ChaCha reseeded from the OS).
pub fn generate_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `sha256(token)` as lowercase hex.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Constant-time equality over two hex digests (length-checked first; the
/// lengths are public — both are sha256 hex).
pub fn ct_eq_hex(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

#[allow(clippy::len_without_is_empty)]
impl DeviceRegistry {
    /// An in-memory registry (tests).
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load from `path` (missing file = empty registry; a corrupt file is
    /// logged and treated as empty rather than blocking the engine).
    pub fn load(path: PathBuf) -> Self {
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Vec<Device>>(&text) {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!("invalid device registry {}: {e}", path.display());
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            path: Some(path),
            devices,
        }
    }

    /// Atomic save: write `<file>.tmp` then rename over the target.
    pub fn save(&self) -> Result<(), RegistryError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
        }
        let text = serde_json::to_string_pretty(&self.devices)
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| RegistryError::Io(e.to_string()))?;
        // Windows: rename over an existing file fails on some filesystems;
        // `rename` on NTFS replaces, but be defensive and retry after a remove.
        if let Err(first) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(path);
            if let Err(e) = std::fs::rename(&tmp, path) {
                let _ = std::fs::remove_file(&tmp);
                return Err(RegistryError::Io(format!("{first}; retry: {e}")));
            }
        }
        Ok(())
    }

    pub fn list(&self) -> &[Device] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Register a device and return it together with the **plaintext token**
    /// (the only time it is available). Persists.
    pub fn create(
        &mut self,
        name: &str,
        model: &str,
        platform: &str,
        ip: &str,
    ) -> Result<(Device, String), RegistryError> {
        if self.devices.iter().filter(|d| !d.is_share()).count() >= MAX_DEVICES {
            return Err(RegistryError::Full);
        }
        let token = generate_token();
        let now = now_ms();
        let device = Device {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.trim().chars().take(64).collect(),
            token_hash: hash_token(&token),
            created_at: now,
            last_seen: now,
            last_ip: ip.to_string(),
            revoked: false,
            model: model.trim().chars().take(64).collect(),
            platform: platform.trim().chars().take(32).collect(),
            session: None,
            generation: 0,
        };
        self.devices.push(device.clone());
        self.save()?;
        Ok((device, token))
    }

    /// 1.8: mint a share link token confined to `session_id`. Shares have
    /// their own cap so they never crowd out paired phones.
    pub fn create_share(
        &mut self,
        session_id: &str,
        label: &str,
    ) -> Result<(Device, String), RegistryError> {
        if self
            .devices
            .iter()
            .filter(|d| d.is_share() && !d.revoked)
            .count()
            >= MAX_SHARES
        {
            return Err(RegistryError::Full);
        }
        let token = generate_token();
        let now = now_ms();
        let device = Device {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: label.trim().chars().take(64).collect(),
            token_hash: hash_token(&token),
            created_at: now,
            last_seen: now,
            last_ip: String::new(),
            revoked: false,
            model: String::new(),
            platform: "share".to_string(),
            session: Some(session_id.to_string()),
            generation: 0,
        };
        self.devices.push(device.clone());
        self.save()?;
        Ok((device, token))
    }

    /// Share links for one session (revoked ones excluded).
    pub fn shares_for(&self, session_id: &str) -> Vec<&Device> {
        self.devices
            .iter()
            .filter(|d| !d.revoked && d.session.as_deref() == Some(session_id))
            .collect()
    }

    /// Verify a presented token. Constant time over the whole registry: the
    /// hash is compared against every entry (revoked ones included) and the
    /// match is folded without an early exit.
    pub fn verify(&self, token: &str) -> Option<DeviceAuth> {
        let presented = hash_token(token);
        let mut found: Option<DeviceAuth> = None;
        for device in &self.devices {
            let hit = ct_eq_hex(&device.token_hash, &presented) & !device.revoked;
            if hit {
                found = Some(DeviceAuth {
                    device_id: device.id.clone(),
                    generation: device.generation,
                    session: device.session.clone(),
                });
            }
        }
        found
    }

    /// Flag a device revoked and bump its generation. `true` when it existed.
    pub fn revoke(&mut self, id: &str) -> Result<bool, RegistryError> {
        let Some(device) = self.devices.iter_mut().find(|d| d.id == id) else {
            return Ok(false);
        };
        device.revoked = true;
        device.generation += 1;
        self.save()?;
        Ok(true)
    }

    /// Remove a device entirely (open streams see `generation() == None`
    /// and end). `true` when it existed.
    pub fn remove(&mut self, id: &str) -> Result<bool, RegistryError> {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        let removed = self.devices.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Current generation of a device; `None` once removed or revoked, which
    /// an open stream treats as "changed".
    pub fn generation(&self, id: &str) -> Option<u64> {
        self.devices
            .iter()
            .find(|d| d.id == id && !d.revoked)
            .map(|d| d.generation)
    }

    /// Record activity (heartbeat / authenticated request). Persisted lazily
    /// by the caller (`save`) to avoid a disk write per request.
    pub fn touch(&mut self, id: &str, ip: &str) -> bool {
        let Some(device) = self.devices.iter_mut().find(|d| d.id == id) else {
            return false;
        };
        device.last_seen = now_ms();
        if !ip.is_empty() {
            device.last_ip = ip.to_string();
        }
        true
    }

    /// Devices seen within `window_ms`.
    pub fn online_count(&self, window_ms: u64) -> usize {
        let now = now_ms();
        self.devices
            .iter()
            .filter(|d| !d.revoked && now.saturating_sub(d.last_seen) <= window_ms)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_random_bytes_base64url() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43, "32 bytes base64url without padding");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a)
            .unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn hash_is_sha256_hex() {
        // sha256("abc")
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The constant-time helper agrees with `==` on every class of input.
    #[test]
    fn ct_eq_hex_matches_eq() {
        let h = hash_token("x");
        assert!(ct_eq_hex(&h, &h));
        assert!(!ct_eq_hex(&h, &hash_token("y")));
        assert!(!ct_eq_hex(&h, &h[..63]));
        assert!(ct_eq_hex("", ""));
        assert!(!ct_eq_hex("", "a"));
    }

    #[test]
    fn create_verify_revoke_remove_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote").join("devices.json");
        let mut reg = DeviceRegistry::load(path.clone());
        assert!(reg.is_empty());

        let (device, token) = reg
            .create("Pixel 9", "Pixel 9 Pro", "android", "100.64.1.2")
            .unwrap();
        assert_eq!(device.name, "Pixel 9");
        assert_eq!(device.token_hash, hash_token(&token));
        assert!(path.exists(), "registry persisted on create");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(&token),
            "plaintext token must never hit the disk"
        );
        assert!(on_disk.contains(&device.token_hash));
        assert!(!on_disk.contains("generation"));

        // Verify: right token -> the device; wrong token -> None.
        let auth = reg.verify(&token).unwrap();
        assert_eq!(auth.device_id, device.id);
        assert_eq!(auth.generation, 0);
        assert!(reg.verify("nope").is_none());
        assert!(reg.verify(&generate_token()).is_none());

        // Survives a restart.
        let reg2 = DeviceRegistry::load(path.clone());
        assert_eq!(reg2.len(), 1);
        assert_eq!(reg2.verify(&token).unwrap().device_id, device.id);
        assert_eq!(reg2.get(&device.id).unwrap().last_ip, "100.64.1.2");

        // Revoke: flag + generation bump; verify fails; persisted.
        assert!(reg.revoke(&device.id).unwrap());
        assert!(reg.verify(&token).is_none());
        assert_eq!(reg.generation(&device.id), None);
        assert!(reg.get(&device.id).unwrap().revoked);
        assert_eq!(reg.get(&device.id).unwrap().generation, 1);
        let reg3 = DeviceRegistry::load(path.clone());
        assert!(reg3.get(&device.id).unwrap().revoked);
        assert!(reg3.verify(&token).is_none());
        assert!(!reg.revoke("missing").unwrap());

        // Remove.
        assert!(reg.remove(&device.id).unwrap());
        assert!(!reg.remove(&device.id).unwrap());
        assert!(DeviceRegistry::load(path).is_empty());
    }

    #[test]
    fn registry_is_capped_at_five_devices() {
        let mut reg = DeviceRegistry::in_memory();
        for i in 0..MAX_DEVICES {
            reg.create(&format!("d{i}"), "", "", "").unwrap();
        }
        assert!(matches!(
            reg.create("one too many", "", "", ""),
            Err(RegistryError::Full)
        ));
        // Removing one frees a slot.
        let id = reg.list()[0].id.clone();
        reg.remove(&id).unwrap();
        assert!(reg.create("again", "", "", "").is_ok());
    }

    #[test]
    fn touch_and_online_count() {
        let mut reg = DeviceRegistry::in_memory();
        let (d, _) = reg.create("a", "", "", "").unwrap();
        assert!(reg.touch(&d.id, "10.0.0.5"));
        assert_eq!(reg.get(&d.id).unwrap().last_ip, "10.0.0.5");
        assert!(!reg.touch("missing", ""));
        assert_eq!(reg.online_count(30_000), 1);
        reg.revoke(&d.id).unwrap();
        assert_eq!(reg.online_count(30_000), 0);
    }

    #[test]
    fn corrupt_registry_file_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        std::fs::write(&path, "{ not json").unwrap();
        let reg = DeviceRegistry::load(path);
        assert!(reg.is_empty());
    }
}
