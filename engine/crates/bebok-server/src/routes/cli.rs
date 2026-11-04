//! CLI agent backends: the Settings -> Agents "CLI agents" card.
//!
//! `bebok-cli-agents` can drive an agentic CLI (Claude Code today) as a
//! provider. The GUI has to answer two questions about every backend before it
//! can offer that: *is it usable here* (is the binary installed, which version)
//! and *what does this particular install actually report* when the user hits
//! "Test". Those are process probes, so they live in the server rather than in
//! the client: the browser cannot spawn `claude --version`, and the engine is
//! the only place that already knows the discovery order.
//!
//! Two consequences shape this module:
//! * a probe is a *subprocess with a user-controlled executable path*, so every
//!   call is bounded by a hard timeout and `kill_on_drop(true)` — a hung CLI
//!   must never hold a request, let alone the settings page, open forever;
//! * the settings card polls, so the expensive part (one spawn per backend) is
//!   cached; only an explicit "Test" bypasses the cache.
//!
//! Nothing here reads credentials: the payload describes a binary, never a key
//! or an environment value.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::Path as UrlPath;
use serde::Serialize;
use tokio::process::Command;

use bebok_cli_agents::{CliBackend, CliCapabilities, all_backends, backend_for, discover_binary};

use crate::error::ApiError;

/// How long `backend.probe_args()` may take for the *cached* listing. Short on
/// purpose: the GUI's settings card would rather show "unknown version" than
/// wait. A real user-initiated probe gets [`PROBE_TIMEOUT`].
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// How long an explicit "Test" may take. Generous (a cold Node CLI can be slow
/// to start) but still bounded so the request always answers.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `--version` output is one line; anything longer is a misbehaving binary and
/// would only bloat the response.
const VERSION_CAP: usize = 120;

/// stdout/stderr kept by the probe payload (diagnostics, not a log download).
const OUTPUT_CAP: usize = 4000;

/// The listing survives this long. Spawning every backend's binary per HTTP
/// request would make a polling settings page a process-spawning loop.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// One backend as the settings card renders it. A local DTO rather than
/// `(backend, capabilities)` serialised directly because `CliCapabilities`
/// serialises snake_case and the client speaks camelCase — translating here
/// keeps that crate's wire contract untouched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliBackendDto {
    pub id: String,
    pub label: String,
    /// `binary.is_some()`; kept separate so the client does not have to derive
    /// the rule (and cannot get it wrong).
    pub available: bool,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub capabilities: CliCapabilitiesDto,
    pub default_models: Vec<String>,
    pub env_override: String,
}

/// camelCase mirror of [`CliCapabilities`].
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCapabilitiesDto {
    pub streaming: bool,
    pub resume: bool,
    pub system_prompt: bool,
    pub tool_events: bool,
    pub usage: bool,
    pub images: bool,
    pub mcp: bool,
}

impl From<CliCapabilities> for CliCapabilitiesDto {
    fn from(c: CliCapabilities) -> Self {
        Self {
            streaming: c.streaming,
            resume: c.resume,
            system_prompt: c.system_prompt,
            tool_events: c.tool_events,
            usage: c.usage,
            images: c.images,
            mcp: c.mcp,
        }
    }
}

/// `POST /cli/backends/{id}/probe` payload.
///
/// `code` is `null` when the process never produced an exit code (killed by the
/// timeout, or killed by a signal); `ok` is therefore `code == Some(0)`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliProbeDto {
    pub id: String,
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Raw result of one bounded subprocess run.
struct ProbeOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A cached listing (`Vec<CliBackendDto>`) plus when it was taken.
///
/// Named rather than inlined: the raw type trips `clippy::type_complexity`,
/// and a cache entry is easier to reason about with a name.
type CacheEntry = (Instant, Vec<CliBackendDto>);

/// The cached listing plus the moment it was taken.
static CACHE: LazyLock<Mutex<Option<CacheEntry>>> = LazyLock::new(|| Mutex::new(None));

/// `GET /cli/backends` -> `{ "backends": [CliBackendDto, ...] }`.
///
/// Wrapped in an object (like `GET /providers/catalog`'s `{"providers": […]}`)
/// so the payload can grow with another key later without breaking the client.
pub async fn list_cli_backends() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "backends": backend_rows().await }))
}

/// `POST /cli/backends/{id}/probe` -> `{id, ok, code, stdout, stderr}`.
///
/// Unknown id is the only real error (`404`); everything the *process* does —
/// not installed, non-zero exit, killed after [`PROBE_TIMEOUT`] — is a `200`
/// with `ok: false` and the reason in `stderr`, so the GUI prints it inline on
/// the card instead of popping an error toast for a perfectly normal state
/// ("Claude Code is not installed here").
pub async fn probe_cli_backend(
    UrlPath(id): UrlPath<String>,
) -> Result<Json<CliProbeDto>, ApiError> {
    let backend = backend_for(&id)
        .ok_or_else(|| ApiError::not_found(format!("unknown CLI backend '{id}'")))?;

    // The explicit test always re-discovers: the user pressing "Test" is
    // usually exactly the person who just installed the binary.
    let Some(binary) = discover_binary(backend.as_ref()) else {
        // Still a 200: "not installed" is a state the card reports inline, not
        // an API failure. The cache is dropped either way so a later listing
        // re-discovers the binary the user may have just installed.
        invalidate_cache();
        return Ok(Json(missing_binary_probe(backend.as_ref())));
    };

    let outcome = match run_probe(backend.as_ref(), &binary, PROBE_TIMEOUT).await {
        Ok(out) => out,
        Err(reason) => ProbeOutput {
            code: None,
            stdout: String::new(),
            stderr: reason,
        },
    };

    let dto = CliProbeDto {
        id: backend.id().to_string(),
        ok: outcome.code == Some(0),
        code: outcome.code,
        stdout: truncate(&outcome.stdout, OUTPUT_CAP),
        stderr: truncate(&outcome.stderr, OUTPUT_CAP),
    };
    // The install state may have changed since the listing was cached (that is
    // the point of a manual probe), so drop it and let the next read rebuild.
    invalidate_cache();
    Ok(Json(dto))
}

/// Probe result reported when discovery found nothing: still a `200`, with the
/// env variable that would fix it, because "not installed" is a normal state.
fn missing_binary_probe(backend: &dyn CliBackend) -> CliProbeDto {
    CliProbeDto {
        id: backend.id().to_string(),
        ok: false,
        code: None,
        stdout: String::new(),
        stderr: format!(
            "{} binary not found (looked for {} on PATH); set {} to point at it",
            backend.label(),
            backend.binary_names().join(", "),
            backend.env_override(),
        ),
    }
}

/// Build (or reuse) the listing.
async fn backend_rows() -> Vec<CliBackendDto> {
    if let Some(rows) = cache_snapshot() {
        return rows;
    }
    // Built without holding the lock: each row spawns a process, and holding a
    // `std::sync::Mutex` guard across an `.await` would not compile anyway.
    let mut rows = Vec::new();
    for backend in all_backends() {
        let binary = discover_binary(backend.as_ref());
        let version = match &binary {
            Some(path) => probe_version(backend.as_ref(), path).await,
            None => None,
        };
        rows.push(describe(backend.as_ref(), binary, version));
    }
    store_cache(&rows);
    rows
}

/// One row from its parts — pure so the mapping is testable without a binary.
fn describe(
    backend: &dyn CliBackend,
    binary: Option<PathBuf>,
    version: Option<String>,
) -> CliBackendDto {
    CliBackendDto {
        id: backend.id().to_string(),
        label: backend.label().to_string(),
        available: binary.is_some(),
        binary: binary.map(|p| p.display().to_string()),
        version,
        capabilities: backend.capabilities().into(),
        default_models: backend.default_models(),
        env_override: backend.env_override().to_string(),
    }
}

/// Version string for the listing; any failure is `None` — the row is still
/// useful without it.
async fn probe_version(backend: &dyn CliBackend, binary: &Path) -> Option<String> {
    let out = run_probe(backend, binary, VERSION_TIMEOUT).await.ok()?;
    if out.code != Some(0) {
        return None;
    }
    first_version_line(&out.stdout)
}

/// The first non-empty line of `--version` output, trimmed and capped.
fn first_version_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate(line, VERSION_CAP))
}

/// Run `binary probe_args` with a hard timeout.
///
/// `kill_on_drop(true)` plus `tokio::time::timeout` is what makes the 2 s / 10 s
/// budget real: dropping the `wait_with_output` future drops the child, which
/// kills the process tree's head. Without it a CLI waiting on stdin would leak
/// a process per request.
async fn run_probe(
    backend: &dyn CliBackend,
    binary: &Path,
    timeout: Duration,
) -> Result<ProbeOutput, String> {
    let mut cmd = Command::new(binary);
    cmd.args(backend.probe_args())
        // A closed stdin keeps a CLI from deciding to read a prompt and block.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The desktop shell is a GUI process: a console window flashing up on every
    // cached refresh would be jarring.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", binary.display()))?;

    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => return Err(format!("probe timed out after {timeout:?}")),
        Ok(Err(e)) => return Err(format!("failed to run {}: {e}", binary.display())),
        Ok(Ok(out)) => out,
    };

    Ok(ProbeOutput {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Character-safe truncation with a trailing ellipsis, so a hostile or chatty
/// CLI cannot balloon the response and multi-byte text is never cut in half.
fn truncate(text: &str, cap: usize) -> String {
    let mut out: String = text.chars().take(cap).collect();
    if text.chars().count() > cap {
        out.push('…');
    }
    out
}

fn cache_snapshot() -> Option<Vec<CliBackendDto>> {
    let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some((taken, rows)) if taken.elapsed() < CACHE_TTL => Some(rows.clone()),
        _ => None,
    }
}

fn store_cache(rows: &[CliBackendDto]) {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((Instant::now(), rows.to_vec()));
}

fn invalidate_cache() {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bebok_cli_agents::spec::CliSpec;
    use bebok_cli_agents::{CliError, CliEvent, CliRequest, CliStreamState};

    /// A backend whose whole surface is data, so `describe` can be exercised
    /// without `claude` being installed on the test machine.
    struct StubBackend;

    impl CliBackend for StubBackend {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn label(&self) -> &'static str {
            "Stub CLI"
        }
        fn capabilities(&self) -> CliCapabilities {
            CliCapabilities {
                streaming: true,
                resume: false,
                system_prompt: true,
                tool_events: true,
                usage: false,
                images: true,
                mcp: false,
            }
        }
        fn default_models(&self) -> Vec<String> {
            vec!["one".into(), "two".into()]
        }
        fn binary_names(&self) -> &'static [&'static str] {
            &["stub"]
        }
        fn env_override(&self) -> &'static str {
            "BEBOK_STUB_BIN"
        }
        fn build_command(&self, _spec: &CliSpec, _req: &CliRequest) -> (String, Vec<String>) {
            ("stub".to_string(), Vec::new())
        }
        fn parse_line(
            &self,
            _line: &str,
            _st: &mut CliStreamState,
        ) -> Result<Option<CliEvent>, CliError> {
            Ok(None)
        }
    }

    #[test]
    fn describe_maps_backend_fields_and_derives_availability() {
        let row = describe(
            &StubBackend,
            Some(PathBuf::from("/opt/bin/stub")),
            Some("1.2.3".into()),
        );
        assert_eq!(row.id, "stub");
        assert_eq!(row.label, "Stub CLI");
        assert_eq!(row.env_override, "BEBOK_STUB_BIN");
        assert_eq!(row.default_models, vec!["one".to_string(), "two".into()]);
        assert_eq!(row.binary.as_deref(), Some("/opt/bin/stub"));
        assert_eq!(row.version.as_deref(), Some("1.2.3"));
        // available is derived from the binary, never set independently.
        assert!(row.available);
        assert!(row.capabilities.streaming);
        assert!(!row.capabilities.resume);
        assert!(row.capabilities.images);

        let missing = describe(&StubBackend, None, None);
        assert!(!missing.available);
        assert!(missing.binary.is_none());
        assert!(missing.version.is_none());
    }

    /// The client reads camelCase; `CliCapabilities` itself serialises
    /// snake_case, which is why the local DTO exists.
    #[test]
    fn dto_serialises_camel_case() {
        let row = describe(&StubBackend, Some(PathBuf::from("/opt/bin/stub")), None);
        let json = serde_json::to_value(&row).expect("serialisable");

        assert!(json.get("defaultModels").is_some(), "{json}");
        assert!(json.get("envOverride").is_some(), "{json}");
        assert!(json.get("default_models").is_none(), "{json}");
        assert!(json.get("env_override").is_none(), "{json}");

        let caps = json["capabilities"]
            .as_object()
            .expect("capabilities object");
        for key in [
            "streaming",
            "resume",
            "systemPrompt",
            "toolEvents",
            "usage",
            "images",
            "mcp",
        ] {
            assert!(caps.contains_key(key), "{key} missing from {json}");
        }
        assert!(!caps.contains_key("system_prompt"), "{json}");
        assert!(!caps.contains_key("tool_events"), "{json}");
        assert_eq!(json["binary"], "/opt/bin/stub");
    }

    #[test]
    fn probe_payload_serialises_camel_case_and_reports_ok_from_the_code() {
        let dto = CliProbeDto {
            id: "stub".into(),
            ok: true,
            code: Some(0),
            stdout: "Stub CLI 1.0\n".into(),
            stderr: String::new(),
        };
        let json = serde_json::to_value(&dto).expect("serialisable");
        for key in ["id", "ok", "code", "stdout", "stderr"] {
            assert!(json.get(key).is_some(), "{key} missing from {json}");
        }
        assert_eq!(json["ok"], true);
        assert_eq!(dto.ok, dto.code == Some(0));

        // No exit code (timeout / signal) is never "ok".
        let killed = CliProbeDto {
            ok: false,
            code: None,
            ..dto
        };
        assert!(!killed.ok);
        assert_eq!(
            serde_json::to_value(&killed).unwrap()["code"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn missing_binary_probe_is_a_200_shaped_body_with_the_env_hint() {
        let dto = missing_binary_probe(&StubBackend);
        assert_eq!(dto.id, "stub");
        assert!(!dto.ok);
        assert!(dto.code.is_none());
        assert!(dto.stdout.is_empty());
        assert!(dto.stderr.contains("BEBOK_STUB_BIN"), "{}", dto.stderr);
        assert!(dto.stderr.contains("stub"), "{}", dto.stderr);
    }

    #[test]
    fn truncate_is_character_safe_and_marks_the_cut() {
        assert_eq!(truncate("short", 120), "short");
        assert_eq!(truncate("", 10), "");
        // Exactly at the cap: no ellipsis.
        assert_eq!(truncate("abcd", 4), "abcd");
        assert_eq!(truncate("abcdef", 4), "abcd…");
        // Multi-byte input must not panic or split a character.
        let long = "ęgłąźć".repeat(10);
        let cut = truncate(&long, 4);
        assert_eq!(cut, "ęgłą…");
        assert_eq!(cut.chars().count(), 5);
    }

    #[test]
    fn first_version_line_skips_blanks_and_trims() {
        assert_eq!(
            first_version_line("\n   \n  2.1.318 (Claude Code)  \nsecond\n").as_deref(),
            Some("2.1.318 (Claude Code)")
        );
        assert_eq!(first_version_line(""), None);
        assert_eq!(first_version_line("\n\n"), None);

        let chatty = format!("  {}  ", "x".repeat(VERSION_CAP + 50));
        let line = first_version_line(&chatty).expect("some line");
        assert_eq!(
            line.chars().count(),
            VERSION_CAP + 1,
            "capped plus ellipsis"
        );
        assert!(line.ends_with('…'));
    }
}
