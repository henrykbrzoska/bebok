//! CLI binding spec (`--host` / `--port` / `--addr` / `--token` + env fallbacks).

/// Bind settings from CLI flags / env, resolved by the caller.
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
                        anyhow::anyhow!("BEBOK_PORT must be a valid port number (0-65535), got '{env_port}'")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_global_flag() {
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
        let spec = parse_cli(&[]).unwrap();
        assert!(!spec.global);
    }

    #[test]
    fn port_default_is_8787() {
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 8787);
    }

    #[test]
    fn port_flag_overrides_env() {
        let args: Vec<String> = ["--port", "9999"].iter().map(|s| s.to_string()).collect();
        std::env::set_var("BEBOK_PORT", "42");
        let spec = parse_cli(&args).unwrap();
        assert_eq!(spec.port, 9999);
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn bebok_port_env_fallback() {
        std::env::set_var("BEBOK_PORT", "3000");
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 3000);
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn bebok_port_env_invalid_fails() {
        std::env::set_var("BEBOK_PORT", "not-a-number");
        let err = parse_cli(&[]).unwrap_err();
        assert!(err.to_string().contains("BEBOK_PORT"));
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn bebok_port_env_empty_falls_back_to_default() {
        std::env::set_var("BEBOK_PORT", "");
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 8787);
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn token_flag_parsed() {
        let args: Vec<String> = ["--token", "my-stable-token"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let spec = parse_cli(&args).unwrap();
        assert_eq!(spec.token.as_deref(), Some("my-stable-token"));
    }

    #[test]
    fn no_token_flag_returns_none() {
        let spec = parse_cli(&[]).unwrap();
        assert!(spec.token.is_none());
    }

    #[test]
    fn token_flag_with_port() {
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
        std::env::set_var("BEBOK_PORT", "0");
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 0);
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn bebok_port_max_is_valid() {
        std::env::set_var("BEBOK_PORT", "65535");
        let spec = parse_cli(&[]).unwrap();
        assert_eq!(spec.port, 65535);
        std::env::remove_var("BEBOK_PORT");
    }

    #[test]
    fn bebok_port_overflow_fails() {
        std::env::set_var("BEBOK_PORT", "99999");
        let err = parse_cli(&[]).unwrap_err();
        assert!(err.to_string().contains("BEBOK_PORT"));
        std::env::remove_var("BEBOK_PORT");
    }
}
