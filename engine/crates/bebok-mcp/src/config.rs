//! MCP server configuration parsing (SPEC §3.7 / §6).
//!
//! The `mcp` config section is a map of server name -> spec:
//!
//! ```jsonc
//! {
//!   "mcp": {
//!     "filesystem": {
//!       "transport": "stdio",
//!       "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
//!       "env": { "FOO": "bar" },
//!       "enabled": true
//!     },
//!     "github": {
//!       "transport": "http",
//!       "url": "https://api.githubcopilot.com/mcp/",
//!       "headers": { "Authorization": "Bearer ..." },
//!       "enabled": true
//!     }
//!   }
//! }
//! ```

use std::collections::HashMap;

use serde_json::Value;

/// Transport for one MCP server.
#[derive(Debug, Clone, PartialEq)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}

/// A parsed MCP server definition (name + transport + enable flag).
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerSpec {
    pub name: String,
    pub transport: McpTransport,
    pub enabled: bool,
}

impl McpServerSpec {
    /// Parse the whole `mcp` config section into server specs.
    pub fn parse_all(value: &Value) -> Vec<McpServerSpec> {
        let mut out = Vec::new();
        let Some(map) = value.as_object() else {
            return out;
        };
        for (name, spec) in map {
            if let Some(parsed) = Self::parse_one(name, spec) {
                out.push(parsed);
            }
        }
        out
    }

    /// Parse a single server entry. Unknown transports are skipped.
    pub fn parse_one(name: &str, spec: &Value) -> Option<McpServerSpec> {
        let obj = spec.as_object()?;
        let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let transport = match obj.get("transport").and_then(Value::as_str) {
            Some("stdio") => McpTransport::Stdio {
                command: obj.get("command").and_then(Value::as_str)?.to_string(),
                args: string_array(obj.get("args")).unwrap_or_default(),
                env: string_map(obj.get("env")).unwrap_or_default(),
            },
            Some("http") => McpTransport::Http {
                url: obj.get("url").and_then(Value::as_str)?.to_string(),
                headers: string_map(obj.get("headers")).unwrap_or_default(),
            },
            _ => {
                tracing::warn!("mcp server '{name}': unsupported transport, skipped");
                return None;
            }
        };
        Some(McpServerSpec {
            name: name.to_string(),
            transport,
            enabled,
        })
    }
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let arr = value?.as_array()?;
    Some(
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

fn string_map(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let obj = value?.as_object()?;
    let mut out = HashMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        } else {
            out.insert(k.clone(), v.to_string());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_and_http_servers() {
        let v: Value = serde_json::from_str(
            r#"{
                "filesystem": { "transport": "stdio", "command": "npx", "args": ["-y", "x"], "enabled": true },
                "github": { "transport": "http", "url": "https://x/mcp", "headers": { "A": "b" }, "enabled": false }
            }"#,
        )
        .unwrap();
        let specs = McpServerSpec::parse_all(&v);
        assert_eq!(specs.len(), 2);

        let fs = &specs[0];
        assert_eq!(fs.name, "filesystem");
        assert!(fs.enabled);
        match &fs.transport {
            McpTransport::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &vec!["-y".to_string(), "x".to_string()]);
            }
            _ => panic!("expected stdio"),
        }

        let gh = &specs[1];
        assert!(!gh.enabled);
        match &gh.transport {
            McpTransport::Http { url, headers } => {
                assert_eq!(url, "https://x/mcp");
                assert_eq!(headers.get("A").map(String::as_str), Some("b"));
            }
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn skips_unknown_transport_and_defaults_enabled_false() {
        let v: Value = serde_json::from_str(
            r#"{ "weird": { "transport": "sse", "url": "x" }, "plain": { "transport": "stdio", "command": "y" } }"#,
        )
        .unwrap();
        let specs = McpServerSpec::parse_all(&v);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "plain");
        assert!(!specs[0].enabled);
    }
}
