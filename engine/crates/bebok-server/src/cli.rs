//! CLI binding spec (`--host` / `--port` / `--addr` + `BEBOK_ADDR`).

/// Bind settings from CLI flags / `BEBOK_ADDR`, resolved by the caller.
pub struct BindSpec {
    pub host: std::net::IpAddr,
    pub port: u16,
    /// When `true`, the engine runs in *hub* mode: sessions created with an
    /// empty `directory` are rooted at `data_dir/global/` (the `__global__`
    /// instance) instead of resolving to the current working directory.
    pub global: bool,
}

/// Parse `--host`, `--port` (or `--addr`), `--global` CLI flags;
/// unknown flags are errors.
pub fn parse_cli(args: &[String]) -> anyhow::Result<BindSpec> {
    let mut host: Option<std::net::IpAddr> = None;
    let mut port: Option<u16> = None;
    let mut addr = std::env::var("BEBOK_ADDR").ok().filter(|s| !s.is_empty());
    let mut global = false;

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
            "--global" => {
                global = true;
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: bebok-server [--host IP] [--port PORT] [--addr IP:PORT] [--global]"
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
            });
        }
    }

    Ok(BindSpec {
        host: host.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        port: port.unwrap_or(8787),
        global,
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
}
