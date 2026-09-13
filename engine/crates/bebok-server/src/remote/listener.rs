//! The remote listener (WP-M1, F10-2): extra `TcpListener`s on the tailnet
//! / LAN addresses, serving the same router as the loopback listener plus a
//! `Listener::Remote` request tag.
//!
//! Address selection is by **range**, never by interface name (`tailscale0`
//! on Linux vs the `Tailscale` adapter on Windows):
//! - `100.64.0.0/10` (CGNAT, what Tailscale hands out) — always when enabled;
//!   if `if-addrs` finds none, `tailscale ip -4` is tried as a fallback.
//! - RFC1918 (`10/8`, `172.16/12`, `192.168/16`) — only with `allow_lan`.
//! - never `0.0.0.0`, never loopback.
//!
//! `BEBOK_REMOTE_BIND_TEST=<ip>[,<ip>]` overrides the enumeration entirely
//! (integration tests bind `127.0.0.1`; CI has no Tailscale).
//!
//! On Android the phone is never a hub: [`start`] is a no-op that logs once.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use tokio_util::sync::CancellationToken;

/// A running remote listener (one task per bound address).
#[derive(Debug, Clone)]
pub struct ListenerHandle {
    addrs: Vec<SocketAddr>,
    cancel: CancellationToken,
}

impl ListenerHandle {
    #[cfg(test)]
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// `http://ip:port` per bound address.
    pub fn endpoints(&self) -> Vec<String> {
        self.addrs.iter().map(|a| format!("http://{a}")).collect()
    }

    /// Stop every bound listener (graceful; in-flight requests finish).
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

/// Tailscale's CGNAT range `100.64.0.0/10`.
pub fn is_tailscale(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// RFC1918 private ranges.
pub fn is_rfc1918(ip: &Ipv4Addr) -> bool {
    ip.is_private()
}

/// Whether an address is eligible under the config (`allow_lan`), ignoring
/// loopback, link-local, unspecified and public addresses.
pub fn eligible(ip: &IpAddr, allow_lan: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_unspecified() || v4.is_link_local() {
                return false;
            }
            is_tailscale(v4) || (allow_lan && is_rfc1918(v4))
        }
        // IPv6: Tailscale's fd7a:115c:a1e0::/48 ULA. Kept out of 1.6 (the
        // QR carries IPv4 endpoints); no LAN IPv6 either.
        IpAddr::V6(_) => false,
    }
}

/// Filter + classify a list of local addresses per the rules above.
pub fn select_addrs(all: &[IpAddr], allow_lan: bool) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = all
        .iter()
        .copied()
        .filter(|ip| eligible(ip, allow_lan))
        .collect();
    // Tailscale first (the QR lists it first), stable, de-duplicated.
    out.sort_by_key(|ip| match ip {
        IpAddr::V4(v4) if is_tailscale(v4) => (0, v4.octets()),
        IpAddr::V4(v4) => (1, v4.octets()),
        IpAddr::V6(_) => (2, [0; 4]),
    });
    out.dedup();
    out
}

/// Parse `BEBOK_REMOTE_BIND_TEST` (`ip[,ip]`); `None` when unset/blank.
pub fn test_override() -> Option<Vec<IpAddr>> {
    let raw = std::env::var("BEBOK_REMOTE_BIND_TEST").ok()?;
    let ips: Vec<IpAddr> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if ips.is_empty() { None } else { Some(ips) }
}

/// Parse `tailscale ip -4` output (one address per line).
pub fn parse_tailscale_ip_output(out: &str) -> Vec<IpAddr> {
    out.lines()
        .map(str::trim)
        .filter_map(|l| l.parse::<Ipv4Addr>().ok())
        .filter(is_tailscale)
        .map(IpAddr::V4)
        .collect()
}

/// Local addresses from the OS (empty on Android / failure).
#[cfg(not(target_os = "android"))]
pub fn local_addrs() -> Vec<IpAddr> {
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces.into_iter().map(|i| i.ip()).collect(),
        Err(e) => {
            tracing::warn!("cannot enumerate interfaces: {e}");
            Vec::new()
        }
    }
}

#[cfg(target_os = "android")]
pub fn local_addrs() -> Vec<IpAddr> {
    Vec::new()
}

/// `tailscale ip -4` as a fallback when the crate saw no CGNAT address
/// (e.g. the userspace-networking mode has no OS interface).
#[cfg(not(target_os = "android"))]
async fn tailscale_cli_addrs() -> Vec<IpAddr> {
    let run = tokio::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .stdin(std::process::Stdio::null())
        .output();
    match tokio::time::timeout(std::time::Duration::from_secs(3), run).await {
        Ok(Ok(out)) if out.status.success() => {
            parse_tailscale_ip_output(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

/// The addresses the remote listener should bind for `cfg`.
pub async fn candidate_addrs(cfg: &bebok_core::config::model::RemoteConfig) -> Vec<IpAddr> {
    if let Some(ips) = test_override() {
        return ips;
    }
    #[cfg(target_os = "android")]
    {
        let _ = cfg;
        Vec::new()
    }
    #[cfg(not(target_os = "android"))]
    {
        let mut all = local_addrs();
        let has_ts = all
            .iter()
            .any(|ip| matches!(ip, IpAddr::V4(v4) if is_tailscale(v4)));
        if !has_ts {
            all.extend(tailscale_cli_addrs().await);
        }
        select_addrs(&all, cfg.allow_lan)
    }
}

/// Bind `addrs` on `port` and serve `app` (already stateful) on each,
/// tagged as the remote listener and with peer addresses available to the
/// handlers (`ConnectInfo<SocketAddr>`).
///
/// `Ok(None)` = nothing eligible (logged, engine keeps running).
pub async fn start(
    app: Router,
    addrs: &[IpAddr],
    port: u16,
) -> anyhow::Result<Option<ListenerHandle>> {
    if addrs.is_empty() {
        tracing::warn!(
            "remote enabled but no eligible interface (no Tailscale address and allow_lan=false)"
        );
        return Ok(None);
    }
    let cancel = CancellationToken::new();
    let mut bound = Vec::new();
    let remote_app = app.layer(axum::middleware::from_fn(super::mark_remote_listener));
    for ip in addrs {
        if ip.is_unspecified() {
            tracing::error!("refusing to bind the remote listener on {ip} (wildcard)");
            continue;
        }
        let addr = SocketAddr::new(*ip, port);
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("remote listener cannot bind {addr}: {e}");
                continue;
            }
        };
        let actual = listener.local_addr()?;
        bound.push(actual);
        let app = remote_app.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move { cancel.cancelled().await });
            if let Err(e) = serve.await {
                tracing::warn!("remote listener on {actual} stopped: {e}");
            }
        });
        tracing::info!("remote listener on http://{actual}");
    }
    if bound.is_empty() {
        anyhow::bail!("remote listener: none of {addrs:?} could be bound on port {port}");
    }
    Ok(Some(ListenerHandle {
        addrs: bound,
        cancel,
    }))
}

/// Convenience: candidates from config, then bind. Used by `server::serve`
/// at startup and by `POST /remote/enable`.
pub async fn start_from_config(
    app: Router,
    state: &Arc<super::RemoteState>,
) -> anyhow::Result<Option<ListenerHandle>> {
    let cfg = state.config();
    let addrs = candidate_addrs(&cfg).await;
    let handle = start(app, &addrs, cfg.port).await?;
    if let Some(old) = state.set_listener(handle.clone()) {
        old.stop();
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifies_by_range_not_by_name() {
        assert!(is_tailscale(&"100.64.0.1".parse().unwrap()));
        assert!(is_tailscale(&"100.101.102.103".parse().unwrap()));
        assert!(is_tailscale(&"100.127.255.254".parse().unwrap()));
        assert!(!is_tailscale(&"100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale(&"100.63.255.255".parse().unwrap()));
        assert!(!is_tailscale(&"10.0.0.1".parse().unwrap()));

        assert!(eligible(&v4("100.100.1.2"), false));
        assert!(!eligible(&v4("192.168.1.20"), false));
        assert!(eligible(&v4("192.168.1.20"), true));
        assert!(eligible(&v4("10.1.2.3"), true));
        assert!(eligible(&v4("172.16.5.5"), true));
        assert!(!eligible(&v4("172.32.0.1"), true), "outside 172.16/12");
        assert!(!eligible(&v4("0.0.0.0"), true), "never the wildcard");
        assert!(!eligible(&v4("127.0.0.1"), true));
        assert!(!eligible(&v4("169.254.1.1"), true));
        assert!(!eligible(&v4("8.8.8.8"), true), "public never");
        assert!(!eligible(&"fd7a:115c:a1e0::1".parse().unwrap(), true));
    }

    #[test]
    fn select_prefers_tailscale_and_dedups() {
        let all = vec![
            v4("192.168.1.20"),
            v4("100.100.1.2"),
            v4("127.0.0.1"),
            v4("192.168.1.20"),
            v4("0.0.0.0"),
        ];
        assert_eq!(select_addrs(&all, false), vec![v4("100.100.1.2")]);
        assert_eq!(
            select_addrs(&all, true),
            vec![v4("100.100.1.2"), v4("192.168.1.20")]
        );
        assert!(select_addrs(&[v4("192.168.1.20")], false).is_empty());
    }

    #[test]
    fn parses_tailscale_cli_output() {
        let out = "100.101.102.103\nfd7a:115c:a1e0::1\n\n";
        assert_eq!(parse_tailscale_ip_output(out), vec![v4("100.101.102.103")]);
        assert!(parse_tailscale_ip_output("Tailscale is stopped.\n").is_empty());
    }

    /// Binding `127.0.0.1:0` (the CI/test override path) serves the router
    /// with the `Listener::Remote` tag; a wildcard address is refused.
    #[tokio::test]
    async fn binds_test_addresses_and_refuses_wildcard() {
        let app = Router::new().route(
            "/probe",
            axum::routing::get(|req: axum::extract::Request| async move {
                format!("{:?}", super::super::listener_of(&req))
            }),
        );
        let handle = start(app.clone(), &[v4("0.0.0.0")], 0).await;
        assert!(handle.is_err(), "wildcard must not be bound");
        assert!(start(app.clone(), &[], 0).await.unwrap().is_none());

        let handle = start(app, &[v4("127.0.0.1")], 0)
            .await
            .unwrap()
            .expect("bound");
        assert_eq!(handle.addrs().len(), 1);
        let url = format!("{}/probe", handle.endpoints()[0]);
        let body = reqwest::get(&url).await.unwrap().text().await.unwrap();
        assert_eq!(body, "Remote");
        handle.stop();
    }
}
