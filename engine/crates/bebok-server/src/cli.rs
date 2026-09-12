//! CLI binding spec (`--host` / `--port` / `--addr` + `BEBOK_ADDR`).

/// Bind settings from CLI flags / `BEBOK_ADDR`, resolved by the caller.
pub struct BindSpec {
    pub host: std::net::IpAddr,
    pub port: u16,
}

/// Parse `--host`, `--port` (or `--addr`) CLI flags; unknown flags are errors.
pub fn parse_cli(args: &[String]) -> anyhow::Result<BindSpec> {
    let mut host: Option<std::net::IpAddr> = None;
    let mut port: Option<u16> = None;
    let mut addr = std::env::var("BEBOK_ADDR").ok().filter(|s| !s.is_empty());

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
            "--help" | "-h" => {
                eprintln!("usage: bebok-server [--host IP] [--port PORT] [--addr IP:PORT]");
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
            });
        }
    }

    Ok(BindSpec {
        host: host.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        port: port.unwrap_or(8787),
    })
}
