//! Integration test: McpManager against a real stdio MCP server (a small
//! embedded Python script speaking newline-delimited JSON-RPC).
#![cfg(unix)]

use bebok_mcp::{McpManager, McpServerSpec, McpTransport};
use bebok_tools::{Runtimes, ToolCtx};
use tokio_util::sync::CancellationToken;

const SERVER_PY: &str = r#"import sys, json

def respond(req_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n")
    sys.stdout.flush()

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        method = msg.get("method")
        req_id = msg.get("id")
        if method == "initialize":
            params = msg.get("params", {})
            respond(req_id, {
                "protocolVersion": params.get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "test-server", "version": "1.0.0"},
            })
        elif method == "tools/list":
            respond(req_id, {
                "tools": [
                    {"name": "echo", "description": "Echo text back",
                     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
                    {"name": "mutate", "description": "Pretend to mutate",
                     "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}}, "required": ["target"]}},
                ]
            })
        elif method == "tools/call":
            params = msg.get("params", {})
            tool = params.get("name")
            args = params.get("arguments", {}) or {}
            if tool == "echo":
                respond(req_id, {"content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}]})
            elif tool == "mutate":
                respond(req_id, {"content": [{"type": "text", "text": "mutated " + str(args.get("target", ""))}]})
            else:
                respond(req_id, {"content": [{"type": "text", "text": "unknown tool"}], "isError": True})
        # notifications (no response)

if __name__ == "__main__":
    main()
"#;

fn stdio_spec(script: &str, enabled: bool) -> McpServerSpec {
    McpServerSpec {
        name: "test".to_string(),
        transport: McpTransport::Stdio {
            command: "python3".to_string(),
            args: vec![script.to_string()],
            env: Default::default(),
        },
        enabled,
    }
}

#[tokio::test]
async fn connects_lists_calls_and_toggles_off() {
    let base = std::env::temp_dir().join(format!("bebok-mcp-it-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let script = base.join("server.py");
    std::fs::write(&script, SERVER_PY).unwrap();

    let manager = McpManager::new();

    // Connect.
    let tools = manager
        .sync(
            &[stdio_spec(script.to_str().unwrap(), true)],
            &Runtimes::default(),
        )
        .await;
    let status = manager.status();
    assert_eq!(status.len(), 1);
    assert!(status[0].connected, "server should be connected");
    assert_eq!(status[0].tool_count, 2);

    let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    assert!(names.contains(&"mcp__test__echo".to_string()));
    assert!(names.contains(&"mcp__test__mutate".to_string()));

    // Call the echo tool through the Tool trait.
    let echo = tools
        .iter()
        .find(|t| t.name() == "mcp__test__echo")
        .unwrap()
        .clone();
    let ctx = ToolCtx {
        root: base.clone(),
        session_id: "sess".to_string(),
        abort: CancellationToken::new(),
    };
    let out = echo
        .execute(ctx, serde_json::json!({ "text": "hello" }))
        .await;
    assert!(out.text.contains("echo: hello"), "got: {}", out.text);

    // The mutate tool is not annotated read-only -> default mutating (Ask).
    let mutate = tools
        .iter()
        .find(|t| t.name() == "mcp__test__mutate")
        .unwrap()
        .clone();
    assert!(!mutate.is_read_only());

    // Toggle off -> tools disappear and status reflects disconnected.
    let tools_off = manager
        .sync(
            &[stdio_spec(script.to_str().unwrap(), false)],
            &Runtimes::default(),
        )
        .await;
    assert!(tools_off.is_empty(), "disabled server must drop its tools");
    let status = manager.status();
    assert_eq!(status.len(), 1);
    assert!(!status[0].connected);
    assert_eq!(status[0].tool_count, 0);

    std::fs::remove_dir_all(&base).unwrap();
}
