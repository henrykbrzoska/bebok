//! `bebok-server mcp`: expose the engine's tools as an MCP server over stdio.
//!
//! Lets an external agent (Claude Code: `claude mcp add bebok -- bebok-server
//! mcp --project .`) call Bebok's tools - the coreutils-style natives, `bash`
//! with background processes, the browser, `ssh`, and the code-navigation
//! tools (`code_index_*`, `code_ast`, `code_graph_*`) when the project enables
//! them.
//!
//! Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout (MCP stdio). In
//! this mode stdout is the MCP channel, so there is no `BEBOK_READY` line and
//! no HTTP listener; logs go to stderr as usual.
//!
//! Hand-rolled instead of rmcp's server side: the surface is four methods
//! (`initialize`, `ping`, `tools/list`, `tools/call`) plus cancellation, and
//! the workspace only builds rmcp's client features.
//!
//! What is exposed: every tool of the project instance except the delegation
//! tools (`task`, `fleet`, `task_*` need an LLM session) and tools bridged from
//! the project's own MCP servers (the client can connect to those directly).
//! Permissions: YOLO - Bebok's permission engine (allow/ask/deny rules) is not
//! consulted; the client is the only gatekeeper (Claude Code asks before an MCP
//! call). Narrow the surface with `--read-only` / `--exclude`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bebok_core::{Instance, InstanceStore};
use bebok_tools::{Tool, ToolCtx, ToolSource};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{OnceCell, mpsc};
use tokio_util::sync::CancellationToken;

/// MCP protocol revisions this server speaks; the newest is the default.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Tools that drive sub-agent LLM sessions; meaningless without a Bebok turn.
const DELEGATION_TOOLS: &[&str] = &[
    "task",
    "fleet",
    "task_status",
    "task_wait",
    "task_cancel",
    "task_report",
];

const INSTRUCTIONS: &str = "Bebok engine tools for this project. Paths are relative to the \
project root and confined to it. For finding code prefer code_index_search (full-text index) \
and code_ast (structural queries) over grep when they are listed. bash with background:true \
plus ready_port/ready_text starts dev servers; stop them with bash_kill.";

/// `bebok-server mcp` flags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpSpec {
    /// Project root (default: current directory).
    pub project: Option<PathBuf>,
    /// Expose only read-only tools.
    pub read_only: bool,
    /// Tool names to hide.
    pub exclude: Vec<String>,
}

pub fn parse_mcp_cli(args: &[String]) -> anyhow::Result<McpSpec> {
    let mut spec = McpSpec::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--project" | "-C" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--project requires a value"))?;
                spec.project = Some(PathBuf::from(v));
            }
            "--read-only" => spec.read_only = true,
            "--exclude" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--exclude requires a value"))?;
                spec.exclude.extend(
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                );
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: bebok-server mcp [--project DIR] [--read-only] [--exclude tool1,tool2]\n\
                     Serves Bebok's tools over MCP (stdio). Register with Claude Code:\n  \
                     claude mcp add bebok -- bebok-server mcp --project /path/to/project"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument '{other}' (see bebok-server mcp --help)"),
        }
    }
    Ok(spec)
}

/// Shared server state: lazily created project instance + exposure filter.
struct McpState {
    store: Arc<InstanceStore>,
    directory: String,
    instance: OnceCell<Arc<Instance>>,
    read_only: bool,
    exclude: HashSet<String>,
    /// Stable per-process session id: the browser and `ssh` tools key their
    /// state by session, so one MCP connection = one Bebok session.
    session_id: String,
    /// In-flight `tools/call` requests by JSON-RPC id (for cancellation).
    inflight: Mutex<HashMap<String, CancellationToken>>,
    shutdown: CancellationToken,
}

impl McpState {
    async fn instance(&self) -> Result<Arc<Instance>, String> {
        self.instance
            .get_or_try_init(|| async {
                self.store
                    .get_or_create_instance(&self.directory)
                    .await
                    .map_err(|e| format!("cannot open project {}: {e}", self.directory))
            })
            .await
            .cloned()
    }

    /// Whether `tool` (with its registry slice) is offered to the client.
    fn exposes(&self, source: ToolSource, tool: &dyn Tool) -> bool {
        let name = tool.name();
        if source == ToolSource::Mcp
            || DELEGATION_TOOLS.contains(&name)
            || self.exclude.contains(name)
        {
            return false;
        }
        !self.read_only || tool.is_read_only()
    }

    async fn exposed_tools(&self) -> Result<Vec<Arc<dyn Tool>>, String> {
        let inst = self.instance().await?;
        let mut tools: Vec<Arc<dyn Tool>> = inst
            .tools
            .list_with_source()
            .into_iter()
            .filter(|(src, t)| {
                self.exposes(*src, t.as_ref()) && inst.tools.source_of(t.name()) == Some(*src)
            })
            .map(|(_, t)| t)
            .collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(tools)
    }
}

/// Run the MCP server on the process's stdin/stdout until stdin closes.
pub async fn run(spec: McpSpec) -> anyhow::Result<()> {
    bebok_core::config::ensure_global_runtimes();
    let store = InstanceStore::new();
    // Same in-process plugin wiring as the HTTP server, so the index is
    // rebuilt after file writes made through this server.
    bebok_core::PluginHost::global().attach(&store.bus()).await;
    bebok_core::PluginHost::global()
        .register(Arc::new(
            bebok_core::plugin::index_rescan::IndexRescanPlugin::new(),
        ))
        .await;
    let project = match spec.project.clone() {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let project = std::path::absolute(&project)?;
    tracing::info!("bebok MCP server for {}", project.display());
    serve_io(
        store,
        project,
        spec,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await;
    let killed = bebok_tools::processes::ProcessRegistry::global()
        .kill_all()
        .await;
    if !killed.is_empty() {
        tracing::info!("stopped {} background process(es)", killed.len());
    }
    Ok(())
}

/// Serve MCP over any line-oriented reader/writer pair until EOF.
pub async fn serve_io<R, W>(
    store: Arc<InstanceStore>,
    project: PathBuf,
    spec: McpSpec,
    reader: R,
    mut writer: W,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let state = Arc::new(McpState {
        store,
        directory: project.to_string_lossy().into_owned(),
        instance: OnceCell::new(),
        read_only: spec.read_only,
        exclude: spec.exclude.into_iter().collect(),
        session_id: format!("mcp-{}", uuid::Uuid::new_v4()),
        inflight: Mutex::new(HashMap::new()),
        shutdown: CancellationToken::new(),
    });

    // Single writer task: responses from concurrent handlers never interleave.
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut line = serde_json::to_vec(&msg).unwrap_or_default();
            line.push(b'\n');
            if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });

    // Warm the instance up while the client is still handshaking.
    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = state.instance().await {
                tracing::warn!("{e}");
            }
        });
    }

    let mut lines = BufReader::new(reader).lines();
    let mut handlers = tokio::task::JoinSet::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ));
                continue;
            }
        };
        let state = state.clone();
        let tx = tx.clone();
        handlers.spawn(async move {
            if let Some(resp) = handle_message(&state, msg).await {
                let _ = tx.send(resp);
            }
        });
        // Reap finished handlers so the set does not grow without bound.
        while handlers.try_join_next().is_some() {}
    }

    // stdin closed: the client is gone. Let already-accepted requests finish
    // first (their responses are still collected below); trip shutdown
    // afterwards only as cleanup for anything still holding a child token.
    while handlers.join_next().await.is_some() {}
    state.shutdown.cancel();
    drop(tx);
    let _ = writer_task.await;
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Handle one JSON-RPC message; `None` for notifications and responses.
async fn handle_message(state: &McpState, msg: Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?.to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = msg.get("id").cloned() else {
        if method == "notifications/cancelled"
            && let Some(req) = params.get("requestId")
            && let Some(token) = state.inflight.lock().unwrap().get(&req.to_string())
        {
            token.cancel();
        }
        return None;
    };

    let result = match method.as_str() {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => list_tools(state).await,
        "tools/call" => return Some(call_tool(state, id, params).await),
        _ => {
            return Some(error_response(
                id,
                -32601,
                &format!("method not found: {method}"),
            ));
        }
    };
    Some(match result {
        Ok(v) => ok_response(id, v),
        Err(e) => error_response(id, -32603, &e),
    })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "bebok", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

async fn list_tools(state: &McpState) -> Result<Value, String> {
    let tools: Vec<Value> = state
        .exposed_tools()
        .await?
        .iter()
        .map(|t| {
            let mut schema = t.parameters_schema();
            if !schema.is_object() {
                schema = json!({"type": "object", "properties": {}});
            } else if schema.get("type").is_none() {
                schema["type"] = json!("object");
            }
            let mut annotations = json!({"readOnlyHint": t.is_read_only()});
            if let Some(d) = t.destructive_hint() {
                annotations["destructiveHint"] = json!(d);
            }
            json!({
                "name": t.name(),
                "description": t.description(),
                "inputSchema": schema,
                "annotations": annotations,
            })
        })
        .collect();
    Ok(json!({"tools": tools}))
}

/// `tools/call`. Tool-level failures are results with `isError: true` (as
/// MCP specifies), protocol failures are JSON-RPC errors.
async fn call_tool(state: &McpState, id: Value, params: Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "tools/call: missing 'name'");
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(a) => a.clone(),
    };
    let inst = match state.instance().await {
        Ok(i) => i,
        Err(e) => return error_response(id, -32603, &e),
    };
    let tool = match inst.tools.get(name) {
        Some(t)
            if inst
                .tools
                .source_of(name)
                .is_some_and(|src| state.exposes(src, t.as_ref())) =>
        {
            t
        }
        _ => return error_response(id, -32602, &format!("unknown tool: {name}")),
    };
    if state.read_only && !tool.is_read_only_for(&args) {
        return ok_response(
            id,
            tool_error(&format!("{name}: refused, server is read-only")),
        );
    }
    // YOLO: Bebok's permission engine is bypassed in MCP mode; the client
    // (Claude Code) is the only gatekeeper. Use --read-only / --exclude to
    // narrow what the client can call.

    let abort = state.shutdown.child_token();
    let key = id.to_string();
    state
        .inflight
        .lock()
        .unwrap()
        .insert(key.clone(), abort.clone());
    let ctx = ToolCtx {
        root: inst.root.clone(),
        session_id: state.session_id.clone(),
        abort: abort.clone(),
    };
    let out = tokio::select! {
        out = tool.execute(ctx, args) => Some(out),
        _ = abort.cancelled() => None,
    };
    state.inflight.lock().unwrap().remove(&key);

    let Some(out) = out else {
        return ok_response(id, tool_error(&format!("{name}: cancelled")));
    };
    let mut content = vec![json!({"type": "text", "text": out.text})];
    if let Some(img) = out.image {
        content.push(json!({"type": "image", "data": img.data, "mimeType": img.media_type}));
    }
    let mut result = json!({"content": content, "isError": false});
    if let Some(s) = out.structured.filter(Value::is_object) {
        result["structuredContent"] = s;
    }
    ok_response(id, result)
}

fn tool_error(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("bebok-mcp-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Feed `requests` (one JSON value per line) and collect the responses.
    async fn roundtrip(project: &std::path::Path, spec: McpSpec, requests: &[Value]) -> Vec<Value> {
        let base = project.parent().unwrap().to_path_buf();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let mut input = String::new();
        for r in requests {
            input.push_str(&r.to_string());
            input.push('\n');
        }
        let (client, server_out) = tokio::io::duplex(1 << 20);
        serve_io(
            store,
            project.to_path_buf(),
            spec,
            input.as_bytes(),
            server_out,
        )
        .await;
        let mut buf = String::new();
        let mut client = client;
        tokio::io::AsyncReadExt::read_to_string(&mut client, &mut buf)
            .await
            .unwrap();
        let mut out: Vec<Value> = buf
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        out.sort_by_key(|v| v["id"].as_i64().unwrap_or(-1));
        out
    }

    fn tool_names(resp: &Value) -> Vec<String> {
        resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn parses_flags() {
        let args: Vec<String> = ["--project", "/x", "--read-only", "--exclude", "bash, ssh"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let spec = parse_mcp_cli(&args).unwrap();
        assert_eq!(spec.project, Some(PathBuf::from("/x")));
        assert!(spec.read_only);
        assert_eq!(spec.exclude, vec!["bash", "ssh"]);
        assert!(parse_mcp_cli(&["--bogus".to_string()]).is_err());
    }

    #[tokio::test]
    async fn initialize_list_and_call() {
        let base = temp_base("basic");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.txt"), "hi from bebok\n").unwrap();

        let resp = roundtrip(
            &project,
            McpSpec::default(),
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"hello.txt"}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"task","arguments":{}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"nope"}),
            ],
        )
        .await;
        assert_eq!(resp.len(), 5, "{resp:#?}");
        assert_eq!(resp[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(resp[0]["result"]["serverInfo"]["name"], "bebok");

        let names = tool_names(&resp[1]);
        assert!(names.contains(&"read_file".to_string()), "{names:?}");
        assert!(names.contains(&"bash".to_string()));
        assert!(!names.iter().any(|n| DELEGATION_TOOLS.contains(&n.as_str())));
        for t in resp[1]["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }

        assert_eq!(resp[2]["result"]["isError"], false);
        let text = resp[2]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("hi from bebok"), "{text}");

        assert_eq!(resp[3]["error"]["code"], -32602);
        assert_eq!(resp[4]["error"]["code"], -32601);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn read_only_and_exclude_filter() {
        let base = temp_base("ro");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let spec = McpSpec {
            project: None,
            read_only: true,
            exclude: vec!["grep".into()],
        };
        let resp = roundtrip(
            &project,
            spec,
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"x.txt","content":"x"}}}),
            ],
        )
        .await;
        let names = tool_names(&resp[0]);
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"bash".to_string()));
        assert!(!names.contains(&"grep".to_string()));
        assert_eq!(resp[1]["error"]["code"], -32602);
        assert!(!project.join("x.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
