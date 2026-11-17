//! Command-line surface of the engine binary: the bind spec
//! (`--host` / `--port` / `--addr` / `--token` + env fallbacks) and the
//! `cli-hook` subcommand a CLI provider's hooks call.
//!
//! `bebok-server cli-hook <pre|post|stop>`
//! --------------------------------------
//! Reads the hook JSON from **stdin**, POSTs it to
//! `$BEBOK_HOOK_URL/<event>` with `Authorization: Bearer $BEBOK_HOOK_TOKEN`
//! and prints the engine's answer on **stdout** — that is exactly the shape a
//! Claude Code command hook speaks. The binary path in the generated profile is
//! `std::env::current_exe()`, so a bundled sidecar and a `cargo run` both work.
//!
//! Fail-closed: if the engine cannot be reached (or the environment is not set)
//! the hook answers `deny` for `pre` and exits non-zero for `post`/`stop`.
//! It never silently allows a tool call it could not gate.

/// Bind settings from CLI flags / env, resolved by the caller.
#[derive(Debug)]
pub struct BindSpec {
    pub host: std::net::IpAddr,
    pub port: u16,
    /// When `true`, the engine runs in *hub* mode: sessions created with an
    /// empty `directory` are rooted at `data_dir/global/` (the `__global__`
    /// instance) instead of resolving to the current working directory.
    pub global: bool,
    /// Explicit `--token` flag value: takes highest precedence for auth
    /// (`--token` > `BEBOK_TOKEN` > token file > memory-only random).
    pub token: Option<String>,
}

/// Parse `--host`, `--port` (or `--addr`), `--token`, `--global` CLI flags;
/// unknown flags are errors.
///
/// Port precedence: `--port` flag > `BEBOK_PORT` env > `8787`.
/// Token precedence: `--token` flag > `BEBOK_TOKEN` env > token file > random.
pub fn parse_cli(args: &[String]) -> anyhow::Result<BindSpec> {
    let mut host: Option<std::net::IpAddr> = None;
    let mut port: Option<u16> = None;
    let mut addr = std::env::var("BEBOK_ADDR").ok().filter(|s| !s.is_empty());
    let mut global = false;
    let mut token: Option<String> = None;

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
            "--token" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--token requires a value"))?;
                token = Some(v.clone());
            }
            "--global" => {
                global = true;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: bebok-server [--host IP] [--port PORT] [--addr IP:PORT] [--token TOKEN] [--global]"
                );
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
                global,
                token,
            });
        }
    }

    // Port fallback: `--port` > `BEBOK_PORT` env > 8787.
    let resolved_port = match port {
        Some(p) => p,
        None => {
            if let Ok(env_port) = std::env::var("BEBOK_PORT") {
                let env_port = env_port.trim().to_string();
                if !env_port.is_empty() {
                    let parsed: u16 = env_port.parse().map_err(|_| {
                        anyhow::anyhow!(
                            "BEBOK_PORT must be a valid port number (0-65535), got '{env_port}'"
                        )
                    })?;
                    tracing::info!("using port {parsed} from BEBOK_PORT env");
                    parsed
                } else {
                    8787
                }
            } else {
                8787
            }
        }
    };

    Ok(BindSpec {
        host: host.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        port: resolved_port,
        global,
        token,
    })
}

/// Hook events a CLI provider may ask for.
pub const HOOK_EVENTS: &[&str] = &["pre", "post", "stop"];

/// What a hook run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Print this on stdout and exit 0 (a decision the CLI understands).
    Printed(String),
    /// Fail: message for stderr, exit code.
    Failed(i32, String),
}

/// The `deny` payload printed when the engine cannot be reached.
pub fn hook_deny(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

/// One hook round-trip, with the environment injected so it is testable.
///
/// `url` is the engine base for this session (`$BEBOK_HOOK_URL`, e.g.
/// `http://127.0.0.1:8787/cli-hook/<session-id>`), `token` the turn token.
pub async fn hook_request(
    event: &str,
    body: &str,
    url: Option<&str>,
    token: Option<&str>,
) -> HookOutcome {
    if !HOOK_EVENTS.contains(&event) {
        return HookOutcome::Failed(
            2,
            format!(
                "bebok hook: unknown event '{event}' (expected one of {})",
                HOOK_EVENTS.join(", ")
            ),
        );
    }
    let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
        return unreachable_engine(event, "BEBOK_HOOK_URL is not set");
    };
    let Some(token) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return unreachable_engine(event, "BEBOK_HOOK_TOKEN is not set");
    };

    let endpoint = format!("{}/{event}", url.trim_end_matches('/'));
    // The engine answers a `pre` hook well before the CLI's own hook timeout;
    // the client timeout only needs to be a little more patient than that.
    let budget = if event == "pre" { 600 } else { 30 };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(budget))
        .build()
    {
        Ok(client) => client,
        Err(e) => return unreachable_engine(event, &format!("cannot build an HTTP client: {e}")),
    };

    let response = client
        .post(&endpoint)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => match response.text().await {
            Ok(text) if !text.trim().is_empty() => HookOutcome::Printed(text),
            Ok(_) => HookOutcome::Printed("{}".to_string()),
            Err(e) => unreachable_engine(event, &format!("cannot read the answer: {e}")),
        },
        Ok(response) => {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            unreachable_engine(
                event,
                &format!("the engine refused the hook ({status}): {}", detail.trim()),
            )
        }
        Err(e) => unreachable_engine(event, &format!("cannot reach the engine: {e}")),
    }
}

/// Fail-closed answer: `deny` JSON for `pre` (the CLI blocks the call),
/// non-zero exit for `post`/`stop` (there is no decision to return, but the
/// failure must not look like success).
fn unreachable_engine(event: &str, reason: &str) -> HookOutcome {
    if event == "pre" {
        HookOutcome::Printed(hook_deny(&format!(
            "bebok could not gate this call: {reason}. Denied (fail-closed)."
        )))
    } else {
        HookOutcome::Failed(2, format!("bebok hook ({event}): {reason}"))
    }
}

/// `bebok-server cli-hook <event>`: read stdin, call the engine, print/write.
///
/// Returns the process exit code. stdout carries only the hook decision, so
/// everything diagnostic goes to stderr.
pub async fn run_hook(event: &str) -> i32 {
    use std::io::Read as _;

    let url = std::env::var("BEBOK_HOOK_URL").ok();
    let token = std::env::var("BEBOK_HOOK_TOKEN").ok();
    // Validate the event before touching stdin: a misconfigured profile must
    // fail fast instead of hanging on an empty pipe.
    if !HOOK_EVENTS.contains(&event) {
        eprintln!(
            "bebok hook: unknown event '{event}' (expected one of {})",
            HOOK_EVENTS.join(", ")
        );
        return 2;
    }
    if url.as_deref().map(str::trim).unwrap_or_default().is_empty()
        || token
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        // No engine address/token: fail closed without blocking on stdin.
        return match unreachable_engine(event, "BEBOK_HOOK_URL/BEBOK_HOOK_TOKEN is not set") {
            HookOutcome::Printed(text) => {
                println!("{text}");
                0
            }
            HookOutcome::Failed(code, message) => {
                eprintln!("{message}");
                code
            }
        };
    }

    let mut body = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut body) {
        let outcome = unreachable_engine(event, &format!("cannot read stdin: {e}"));
        return emit(outcome);
    }

    emit(hook_request(event, &body, url.as_deref(), token.as_deref()).await)
}

fn emit(outcome: HookOutcome) -> i32 {
    match outcome {
        HookOutcome::Printed(text) => {
            println!("{text}");
            0
        }
        HookOutcome::Failed(code, message) => {
            eprintln!("{message}");
            code
        }
    }
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    #[tokio::test]
    async fn unknown_event_fails_before_any_io() {
        let outcome = hook_request("pre-tool", "{}", Some("http://127.0.0.1:1"), Some("t")).await;
        assert!(matches!(outcome, HookOutcome::Failed(2, _)), "{outcome:?}");
    }

    #[tokio::test]
    async fn missing_environment_denies_a_pre_hook() {
        let outcome = hook_request("pre", "{}", None, None).await;
        match outcome {
            HookOutcome::Printed(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
                assert_eq!(
                    value["hookSpecificOutput"]["permissionDecision"], "deny",
                    "a hook that cannot reach the engine must never allow"
                );
                assert!(
                    value["hookSpecificOutput"]["permissionDecisionReason"]
                        .as_str()
                        .unwrap()
                        .contains("BEBOK_HOOK_URL"),
                    "{text}"
                );
            }
            other => panic!("expected a deny payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_environment_fails_post_and_stop() {
        for event in ["post", "stop"] {
            let outcome = hook_request(event, "{}", None, Some("t")).await;
            assert!(
                matches!(outcome, HookOutcome::Failed(2, _)),
                "{event}: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn unreachable_engine_denies_a_pre_hook() {
        // Port 1 on loopback: nothing listens, so the request fails fast.
        let outcome = hook_request(
            "pre",
            r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#,
            Some("http://127.0.0.1:1/cli-hook/s/deadbeef"),
            Some("turn-token"),
        )
        .await;
        match outcome {
            HookOutcome::Printed(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(
                    value["hookSpecificOutput"]["permissionDecision"], "deny",
                    "{text}"
                );
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn deny_payload_has_the_hook_shape() {
        let value: serde_json::Value = serde_json::from_str(&hook_deny("nope")).unwrap();
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(value["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecisionReason"],
            "nope"
        );
    }
}

#[cfg(test)]
mod tests {

    /// All tests in this module observe the same process-global
    /// environment (`BEBOK_PORT` / `BEBOK_TOKEN`), while the test runner
    /// is multi-threaded: serialize them so one test's `set_var` cannot
    /// land inside another test's parse. Poisoning is ignored so a single
    /// failing test doesn't cascade into the rest.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    use super::*;

    #[test]
    fn parse_global_flag() {
        let _env = env_guard();
        let args: Vec<String> = ["--global", "--port", "0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let spec = parse_cli(&args).unwrap();
        assert!(spec.global);
        assert_eq!(spec.port, 0);
    }

    #[test]
    fn without_global_flag_global_is_false() {
        let _env = env_guard();
        let spec = parse_cli(&[]).unwrap();
        assert!(!spec.global);
    }

    #[test]
    fn port_default_is_8787() {
        let _env = env_guard();
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 8787);
    }

    #[test]
    fn port_flag_overrides_env() {
        let _env = env_guard();
        let args: Vec<String> = ["--port", "9999"].iter().map(|s| s.to_string()).collect();
        unsafe { std::env::set_var("BEBOK_PORT", "42") };
        let spec = parse_cli(&args).unwrap();
        assert_eq!(spec.port, 9999);
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn bebok_port_env_fallback() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "3000") };
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 3000);
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn bebok_port_env_invalid_fails() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "not-a-number") };
        let err = parse_cli(&[]).unwrap_err();
        assert!(err.to_string().contains("BEBOK_PORT"));
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn bebok_port_env_empty_falls_back_to_default() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "") };
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 8787);
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn token_flag_parsed() {
        let _env = env_guard();
        let args: Vec<String> = ["--token", "my-stable-token"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let spec = parse_cli(&args).unwrap();
        assert_eq!(spec.token.as_deref(), Some("my-stable-token"));
    }

    #[test]
    fn no_token_flag_returns_none() {
        let _env = env_guard();
        let spec = parse_cli(&[]).unwrap();
        assert!(spec.token.is_none());
    }

    #[test]
    fn token_flag_with_port() {
        let _env = env_guard();
        let args: Vec<String> = ["--token", "abc", "--port", "1234"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let spec = parse_cli(&args).unwrap();
        assert_eq!(spec.token.as_deref(), Some("abc"));
        assert_eq!(spec.port, 1234);
    }

    #[test]
    fn bebok_port_zero_is_valid() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "0") };
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 0);
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn bebok_port_max_is_valid() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "65535") };
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 65535);
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }

    #[test]
    fn bebok_port_overflow_fails() {
        let _env = env_guard();
        unsafe { std::env::set_var("BEBOK_PORT", "99999") };
        let err = parse_cli(&[]).unwrap_err();
        assert!(err.to_string().contains("BEBOK_PORT"));
        unsafe { std::env::remove_var("BEBOK_PORT") };
    }
}
