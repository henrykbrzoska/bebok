//! MCP bridge manager (SPEC §3.7, milestone M4).
//!
//! Connects enabled MCP servers (stdio or streamable HTTP via `rmcp`), lists
//! their tools, and wraps each as a `bebok_tools::Tool` named
//! `mcp__<server>__<tool>`. Toggling a server off drops the connection (which
//! cancels in-flight calls) and removes its tools from the registry.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use http::{HeaderName, HeaderValue};
use rmcp::model::Tool as McpToolInfo;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, serve_client};
use serde::Serialize;

use bebok_tools::{Runtimes, Tool};

use crate::config::{McpServerSpec, McpTransport};
use crate::tool::McpTool;

type Running = rmcp::service::RunningService<RoleClient, ()>;

/// Public status of one configured MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpStatus {
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub tool_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One connected server (its tools plus the keep-alive running service).
struct ServerConn {
    /// Held only to keep the connection/transport alive; calls go through the
    /// cloned `Peer` stored inside each wrapped tool.
    #[allow(dead_code)]
    running: Running,
    tools: Vec<Arc<dyn Tool>>,
}

/// Per-server entry in the manager (configured + optional live connection).
struct ServerEntry {
    spec: McpServerSpec,
    conn: Option<ServerConn>,
    error: Option<String>,
}

/// Runtime MCP state for one instance (all configured servers).
#[derive(Default)]
pub struct McpManager {
    servers: RwLock<HashMap<String, ServerEntry>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bring the manager in line with `specs`: connect newly-enabled servers,
    /// disconnect disabled/removed servers. Returns the complete set of MCP
    /// tools to register in the tool registry. `runtimes` resolves stdio
    /// `command` values that name a known runtime (python/python3/node/php).
    pub async fn sync(&self, specs: &[McpServerSpec], runtimes: &Runtimes) -> Vec<Arc<dyn Tool>> {
        // 1. Determine which servers need connecting (short read lock).
        let to_connect: Vec<McpServerSpec> = {
            let map = self.servers.read().unwrap();
            specs
                .iter()
                .filter(|spec| {
                    spec.enabled
                        && match map.get(&spec.name) {
                            Some(entry) => entry.spec != **spec || entry.conn.is_none(),
                            None => true,
                        }
                })
                .cloned()
                .collect()
        };

        // 2. Connect outside any lock.
        let mut connected: HashMap<String, Result<ServerConn, String>> = HashMap::new();
        for spec in &to_connect {
            let result = connect(spec, runtimes).await;
            connected.insert(spec.name.clone(), result);
        }

        // 3. Apply mutations under the write lock and collect the tool set.
        let mut tools = Vec::new();
        {
            let mut map = self.servers.write().unwrap();

            // Drop servers no longer configured (disconnect + cancel in-flight).
            map.retain(|name, _| specs.iter().any(|s| &s.name == name));

            for spec in specs {
                let entry = map.entry(spec.name.clone()).or_insert_with(|| ServerEntry {
                    spec: spec.clone(),
                    conn: None,
                    error: None,
                });

                if entry.spec != *spec {
                    // Spec changed: drop the old connection and reset.
                    entry.conn = None;
                    entry.error = None;
                    entry.spec = spec.clone();
                }

                if spec.enabled {
                    if entry.conn.is_none() {
                        match connected.remove(&spec.name) {
                            Some(Ok(conn)) => entry.conn = Some(conn),
                            Some(Err(e)) => {
                                entry.conn = None;
                                entry.error = Some(e);
                            }
                            None => {}
                        }
                    }
                } else {
                    entry.conn = None;
                    entry.error = None;
                }
            }

            for entry in map.values() {
                if let Some(conn) = &entry.conn {
                    tools.extend(conn.tools.clone());
                }
            }
        }

        tools
    }

    /// Snapshot status for every configured server.
    pub fn status(&self) -> Vec<McpStatus> {
        let map = self.servers.read().unwrap();
        let mut out: Vec<McpStatus> = map
            .values()
            .map(|e| McpStatus {
                name: e.spec.name.clone(),
                enabled: e.spec.enabled,
                connected: e.conn.is_some(),
                tool_count: e.conn.as_ref().map(|c| c.tools.len()).unwrap_or(0),
                error: e.error.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Connect to one MCP server and wrap its tools.
async fn connect(spec: &McpServerSpec, runtimes: &Runtimes) -> Result<ServerConn, String> {
    match &spec.transport {
        McpTransport::Stdio { command, args, env } => {
            // A bare runtime name (python/python3/node/php/docker) resolves to
            // the configured executable path; anything else is used verbatim.
            let command = runtimes.resolve(command).unwrap_or(command).to_string();
            let mut cmd = tokio::process::Command::new(&command);
            cmd.args(args);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let proc = TokioChildProcess::new(cmd).map_err(|e| format!("spawn {command}: {e}"))?;
            let running = serve_client((), proc).await.map_err(|e| format!("{e}"))?;
            let listed = running
                .peer()
                .list_tools(None)
                .await
                .map_err(|e| format!("tools/list: {e}"))?;
            Ok(wrap(spec, running, &listed.tools))
        }
        McpTransport::Http { url, headers } => {
            let config = http_config(url, headers);
            let transport = StreamableHttpClientTransport::from_config(config);
            let running = serve_client((), transport)
                .await
                .map_err(|e| format!("{e}"))?;
            let listed = running
                .peer()
                .list_tools(None)
                .await
                .map_err(|e| format!("tools/list: {e}"))?;
            Ok(wrap(spec, running, &listed.tools))
        }
    }
}

fn http_config(
    url: &str,
    headers: &HashMap<String, String>,
) -> StreamableHttpClientTransportConfig {
    let mut map = HashMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            map.insert(name, value);
        }
    }
    StreamableHttpClientTransportConfig::with_uri(url).custom_headers(map)
}

/// Wrap an MCP server's tools into `bebok_tools::Tool` instances.
fn wrap(spec: &McpServerSpec, running: Running, tools: &[McpToolInfo]) -> ServerConn {
    let peer = running.peer().clone();
    let mut wrapped: Vec<Arc<dyn Tool>> = Vec::new();
    for t in tools {
        let raw_name = t.name.clone().into_owned();
        let description = t
            .description
            .clone()
            .map(|d| d.into_owned())
            .unwrap_or_default();
        let input_schema = serde_json::Value::Object((*t.input_schema).clone());
        let read_only = t
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        wrapped.push(McpTool::new(
            &spec.name,
            raw_name,
            description,
            input_schema,
            read_only,
            peer.clone(),
        ));
    }
    ServerConn {
        running,
        tools: wrapped,
    }
}
