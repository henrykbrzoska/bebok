//! Docker availability check (settings: "is Docker accessible?").
//!
//! Runs `docker version --format '{{.Server.Version}}'` (resolving the
//! executable through the configurable `runtimes.docker` path) with a timeout.
//! Success means the CLI is installed AND the daemon is reachable.

use serde::Serialize;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// Result of a Docker access probe.
#[derive(Debug, Clone, Serialize)]
pub struct DockerStatus {
    pub executable: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Probe Docker access by asking the daemon for its server version.
pub async fn check_docker(executable: &str) -> DockerStatus {
    let cmd = Command::new(executable)
        .args(["version", "--format", "{{.Server.Version}}"])
        .kill_on_drop(true)
        .output();

    match timeout(Duration::from_secs(8), cmd).await {
        Err(_) => DockerStatus {
            executable: executable.to_string(),
            available: false,
            version: None,
            error: Some("timeout (8s) - docker daemon is not responding".to_string()),
        },
        Ok(Err(e)) => DockerStatus {
            executable: executable.to_string(),
            available: false,
            version: None,
            error: Some(format!("failed to run '{executable}': {e}")),
        },
        Ok(Ok(out)) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !version.is_empty() {
                DockerStatus {
                    executable: executable.to_string(),
                    available: true,
                    version: Some(version),
                    error: None,
                }
            } else {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                DockerStatus {
                    executable: executable.to_string(),
                    available: false,
                    version: None,
                    error: Some(if err.is_empty() {
                        "docker daemon unavailable".to_string()
                    } else {
                        err
                    }),
                }
            }
        }
    }
}
