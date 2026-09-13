#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::agent::{run_turn, spawn_agent_watcher};
    use crate::event::Event;
    use crate::event::EventBus;
    use crate::permission::{PermissionAnswer, ResolveOutcome};
    use crate::session::Part;
    use crate::session::ToolState;
    use crate::store::{InstanceStore, SessionState};
    use crate::{Agent, AgentCatalog, PermissionEngine, PluginHost, Verdict};
    use bebok_llm::{ChatRequest, Provider, StreamEvent, ToolCall, Usage};
    use bebok_mcp::{McpManager, McpServerSpec, McpTransport};
    use bebok_tools::{Runtimes, ToolRegistry, builtin_tools};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    /// Minimal stdio MCP server (JSON-RPC over stdin/stdout) used by the
    /// permission-gate acceptance test.
    const MCP_SERVER_PY: &str = r#"import sys, json

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
            respond(req_id, {"protocolVersion": params.get("protocolVersion", "2024-11-05"),
                             "capabilities": {"tools": {}},
                             "serverInfo": {"name": "test-server", "version": "1.0.0"}})
        elif method == "tools/list":
            respond(req_id, {"tools": [
                {"name": "echo", "description": "Echo text",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "mutate", "description": "Pretend to mutate",
                 "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}}}},
            ]})
        elif method == "tools/call":
            params = msg.get("params", {})
            args = params.get("arguments", {}) or {}
            if params.get("name") == "mutate":
                respond(req_id, {"content": [{"type": "text", "text": "mutated " + str(args.get("target", ""))}]})
            else:
                respond(req_id, {"content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}]})

if __name__ == "__main__":
    main()
"#;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// A provider that replays a fixed script of event groups; group n is
    /// streamed on the n-th `stream` request.
    struct ScriptProvider {
        calls: AtomicUsize,
        script: Vec<Vec<StreamEvent>>,
    }

    impl ScriptProvider {
        fn new(script: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                script,
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for ScriptProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(
            &self,
            _req: ChatRequest,
        ) -> bebok_llm::StreamResult<
            futures::stream::BoxStream<'static, bebok_llm::StreamResult<StreamEvent>>,
        > {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            let events: Vec<bebok_llm::StreamResult<StreamEvent>> = self
                .script
                .get(n)
                .cloned()
                .unwrap_or_else(|| text_step("(no more steps)"))
                .into_iter()
                .map(Ok)
                .collect();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn tool_step(name: &str, input: serde_json::Value, id: u32) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: format!("toolu_{id}"),
                name: name.to_string(),
                input,
            }),
            StreamEvent::Done(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        ]
    }

    fn text_step(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::Text(text.to_string()),
            StreamEvent::Done(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        ]
    }

    /// Write a `permission` section to `<project>/.bebok/config.json`.
    fn write_project_permission(project: &Path, permission_json: &str) {
        let dir = project.join(".bebok");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            format!("{{ \"permission\": {permission_json} }}\n"),
        )
        .unwrap();
    }

    /// Drain a broadcast receiver for `quiet` ms (used to collect events).
    async fn drain_events(rx: &mut broadcast::Receiver<Event>, quiet_ms: u64) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = tokio::time::timeout(Duration::from_millis(quiet_ms), rx.recv()).await {
            if let Ok(ev) = ev {
                out.push(ev);
            }
        }
        out
    }

    /// Resolve the first `permission.asked` seen on `rx`.
    async fn resolve_first_ask(
        session: Arc<crate::store::SessionState>,
        mut rx: broadcast::Receiver<Event>,
        answer: PermissionAnswer,
    ) {
        loop {
            let ev = rx
                .recv()
                .await
                .expect("stream closed before a permission.asked event");
            if ev.kind == "permission.asked" {
                let request_id = ev.properties["requestID"]
                    .as_str()
                    .expect("requestID must be a string")
                    .to_string();
                let outcome = session
                    .resolve_permission_request(&request_id, answer)
                    .await;
                assert_eq!(outcome, ResolveOutcome::Resolved, "first ask must resolve");
                return;
            }
        }
    }

    // ------------------------------------------------------------------
    // M1 baseline
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn runs_tool_turn_end_to_end() {
        // temp project + data dirs
        let base = std::env::temp_dir().join(format!("bebok-test-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        // write_file is mutating -> would Ask by default; allow it via config.
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "write_file(*)", "action": "allow" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        // Append the prompt (like the server does) before running the turn.
        session
            .append_user_message("create a file hello.txt with the content hello")
            .await
            .unwrap();
        session.set_title_if_empty("create a file hello.txt").await;

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "write_file",
                serde_json::json!({ "path": "hello.txt", "content": "hello" }),
                1,
            ),
            text_step("hello.txt created"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let abort = CancellationToken::new();

        // subscribe before the turn to capture events
        let mut events = bus.subscribe();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        // hello.txt must exist with content "hello"
        let content = tokio::fs::read_to_string(project.join("hello.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello", "write_file tool must create hello.txt");

        // Transcript: [user, assistant(write_file tool), assistant(final text)].
        // The tool result is carried by the Tool part (Completed), not by a
        // separate persisted message.
        let messages = session.messages_snapshot().await;
        assert_eq!(
            messages.len(),
            3,
            "expected 3 messages, got {}",
            messages.len()
        );
        assert!(messages[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text == "create a file hello.txt with the content hello")));

        // Tool part must be Completed.
        let tool_completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { .. }, .. } if name == "write_file")
        });
        assert!(tool_completed, "tool part must be in Completed state");

        // Final assistant text present.
        let final_text = messages[2].text_content();
        assert!(
            final_text.contains("hello.txt created"),
            "got: {final_text}"
        );

        // Events observed.
        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(kinds.contains("message.updated"));
        assert!(kinds.contains("message.part.updated"));
        assert!(kinds.contains("session.updated"));
        assert!(
            !kinds.contains("permission.asked"),
            "allow rule must avoid ask"
        );

        // Disk: msg files + session.json + index.jsonl.
        let disk = session.disk_dir().to_path_buf();
        assert!(disk.join("msg-000000.json").exists());
        assert!(disk.join("msg-000002.json").exists());
        assert!(disk.join("session.json").exists());
        let idx = data
            .join("instances")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("sessions/index.jsonl");
        assert!(idx.exists(), "index.jsonl missing: {idx:?}");

        // Cleanup
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn restart_reloads_sessions_from_disk() {
        let base = std::env::temp_dir().join(format!("bebok-restart-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let id = {
            let store = InstanceStore::with_data_dir(data.clone());
            let s = store
                .create_session(project.to_str().unwrap(), "code", None)
                .await
                .unwrap();
            s.id()
        };

        // Simulate restart: a fresh store over the same data dir.
        let store = InstanceStore::with_data_dir(data.clone());
        let list = store.list_sessions(project.to_str().unwrap()).await;
        assert_eq!(list.len(), 1, "session must survive restart");
        assert_eq!(list[0].id, id);

        // And the transcript opens.
        let s = store.open_session(id).await.unwrap();
        assert_eq!(s.directory(), crate::util::normalize_path(&project));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn session_directory_has_no_verbatim_prefix() {
        let base = std::env::temp_dir().join(format!("bebok-path-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let store = InstanceStore::with_data_dir(base.join("data"));
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        assert!(!session.directory().starts_with(r"\\?\"));
        let _ = std::fs::remove_dir_all(base);
    }

    // ------------------------------------------------------------------
    // M2 permission gate
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn deny_rule_blocks_bash_without_executing() {
        let base = std::env::temp_dir().join(format!("bebok-deny-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let sentinel = project.join("must-survive.txt");
        tokio::fs::write(&sentinel, "do not delete").await.unwrap();
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "bash(rm *)", "action": "deny" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("delete must-survive.txt")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "bash",
                serde_json::json!({ "command": "rm -f must-survive.txt" }),
                1,
            ),
            text_step("rm is blocked"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();
        let abort = CancellationToken::new();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        // The command must never have run.
        assert!(
            tokio::fs::read_to_string(&sentinel).await.is_ok(),
            "rm must not execute when denied by policy"
        );

        let messages = session.messages_snapshot().await;
        let tool_err = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by policy")
        });
        assert!(tool_err, "tool must be Error with 'denied by policy'");

        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(
            !kinds.contains("permission.asked"),
            "a deny rule must not ask"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn unknown_bash_asks_then_executes_when_allowed() {
        let base = std::env::temp_dir().join(format!("bebok-ask-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("print the project marker")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "echo project" }), 1),
            text_step("done"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        // A second, already-subscribed receiver answers the ask.
        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish after the ask is resolved")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // Tool executed successfully.
        let messages = session.messages_snapshot().await;
        let completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "bash" && output.contains("project"))
        });
        assert!(completed, "allowed bash call must execute");

        // Events: asked + resolved(allow).
        let evs = drain_events(&mut events, 300).await;
        assert_eq!(
            evs.iter().filter(|e| e.kind == "permission.asked").count(),
            1
        );
        let resolved = evs
            .iter()
            .find(|e| e.kind == "permission.resolved")
            .expect("permission.resolved event");
        assert_eq!(resolved.properties["decision"], "allow");
        assert_eq!(resolved.properties["always"], false);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn ask_denied_by_user_marks_error() {
        let base = std::env::temp_dir().join(format!("bebok-askdeny-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("print the working directory")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 1),
            text_step("user denied"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();

        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: false,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish after the ask is denied")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        let messages = session.messages_snapshot().await;
        let tool_err = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by user")
        });
        assert!(
            tool_err,
            "user-denied call must be Error with 'denied by user'"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn always_allow_persists_rule_and_skips_repeat_ask() {
        let base = std::env::temp_dir().join(format!("bebok-always-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("print the working directory twice")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        // Two identical calls back to back; the second must not re-ask.
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 1),
            tool_step("bash", serde_json::json!({ "command": "pwd" }), 2),
            text_step("done twice"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: true,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission.clone(),
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // Only one ask for the identical (tool, pattern) within the session.
        let evs = drain_events(&mut events, 300).await;
        let asked = evs.iter().filter(|e| e.kind == "permission.asked").count();
        assert_eq!(asked, 1, "repeat call must go through without asking");
        let resolved = evs
            .iter()
            .filter(|e| e.kind == "permission.resolved")
            .count();
        assert_eq!(resolved, 1);

        // A rule `ask -> allow` was persisted to the project config — for
        // the TOOL (`bash(*)`), not the exact call (F9-5).
        let config_text = tokio::fs::read_to_string(project.join(".bebok").join("config.json"))
            .await
            .unwrap();
        assert!(
            config_text.contains("bash(*)"),
            "rule persisted: {config_text}"
        );
        assert!(
            !config_text.contains("bash(pwd)"),
            "rule must not be per call: {config_text}"
        );
        assert!(
            config_text.contains("allow"),
            "rule persisted: {config_text}"
        );

        // The engine's in-memory layer was recompiled, so the next identical
        // call resolves straight to Allow.
        let eval = permission.evaluate(
            None,
            "bash",
            &serde_json::json!({ "command": "pwd" }),
            false,
        );
        assert_eq!(eval.verdict, Verdict::Allow);

        // Both tool calls completed.
        let messages = session.messages_snapshot().await;
        let completed = messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter(|p| matches!(p, Part::Tool { name, .. } if name == "bash"))
            .count();
        assert_eq!(completed, 2, "both identical calls must have executed");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn read_only_tool_runs_by_default_without_asking() {
        let base = std::env::temp_dir().join(format!("bebok-readonly-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        tokio::fs::write(project.join("data.txt"), "hello world")
            .await
            .unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session.append_user_message("read data.txt").await.unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step("read_file", serde_json::json!({ "path": "data.txt" }), 1),
            text_step("read done"),
        ]));
        // No permission config at all.
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();
        let abort = CancellationToken::new();

        session.try_begin_turn();
        run_turn(
            session.clone(),
            Agent::code(),
            tools,
            provider,
            permission,
            bus,
            abort,
            crate::config::DEFAULT_MODEL,
        )
        .await
        .unwrap();
        session.end_turn();

        let messages = session.messages_snapshot().await;
        let completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "read_file" && output.contains("hello world"))
        });
        assert!(completed, "read-only tool must run by default (Allow)");

        let kinds: std::collections::HashSet<String> = drain_events(&mut events, 300)
            .await
            .into_iter()
            .map(|ev| ev.kind)
            .collect();
        assert!(!kinds.contains("permission.asked"));

        let _ = std::fs::remove_dir_all(&base);
    }

    // ------------------------------------------------------------------
    // M4: agents, skills, MCP
    // ------------------------------------------------------------------

    #[test]
    fn agent_catalog_loads_file_presets() {
        let base = std::env::temp_dir().join(format!("bebok-agents-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let agent_dir = project.join(".bebok").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("my.md"),
            "---\nname: my\ndescription: custom\nmodel: zai/glm-5.3\ntools: [\"read_file\", \"grep\"]\npermissions:\n  - pattern: \"bash(git *)\"\n    action: \"allow\"\n---\nYou are my custom agent.\n",
        )
        .unwrap();

        let catalog = AgentCatalog::load(&project);
        let agent = catalog.resolve("my");
        assert_eq!(agent.name, "my");
        assert_eq!(agent.model.as_deref(), Some("zai/glm-5.3"));
        assert_eq!(agent.tools, vec!["read_file", "grep"]);
        assert_eq!(agent.permissions.len(), 1);
        assert_eq!(agent.permissions[0].pattern, "bash(git *)");
        assert!(agent.prompt.contains("custom agent"));
        assert!(!agent.builtin);

        // Built-ins resolve, and unknown names fall back to `code`.
        assert!(catalog.contains("code"));
        assert_eq!(catalog.resolve("ask").name, "ask");
        assert_eq!(catalog.resolve("nope").name, "code");

        let infos = catalog.list();
        assert!(infos.iter().any(|a| a.name == "my" && !a.builtin));
        assert!(infos.iter().any(|a| a.name == "code" && a.builtin));

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn hot_reload_picks_up_new_agent_file() {
        let base = std::env::temp_dir().join(format!("bebok-hot-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();

        let catalog = Arc::new(std::sync::RwLock::new(AgentCatalog::load(&project)));
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let _watcher = spawn_agent_watcher(project.clone(), catalog.clone(), bus.clone()).unwrap();

        // Give the watcher a moment to register before writing the file.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let agent_dir = project.join(".bebok").join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("my.md"), "---\nname: my\n---\nhello\n").unwrap();

        let mut saw_event = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ev = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
            match ev {
                Ok(Ok(ev)) if ev.kind == "agent.list.changed" => {
                    saw_event = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        assert!(saw_event, "expected agent.list.changed event");
        assert!(catalog.read().unwrap().contains("my"));

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn mcp_tool_goes_through_permission_gate() {
        let python = ["python3", "python"].into_iter().find(|command| {
            std::process::Command::new(command)
                .arg("--version")
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && (String::from_utf8_lossy(&output.stdout).starts_with("Python 3")
                            || String::from_utf8_lossy(&output.stderr).starts_with("Python 3"))
                })
        });
        let Some(python) = python else {
            eprintln!("skipping MCP gate test: Python 3 is not available");
            return;
        };
        let base = std::env::temp_dir().join(format!("bebok-mcp-gate-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let script = base.join("server.py");
        std::fs::write(&script, MCP_SERVER_PY).unwrap();

        let data = base.join("data");
        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("mutate target.txt")
            .await
            .unwrap();

        // Connect the MCP server and register its tools alongside built-ins.
        let manager = McpManager::new();
        let runtimes = Runtimes::default();
        let spec = McpServerSpec {
            name: "test".to_string(),
            transport: McpTransport::Stdio {
                command: python.to_string(),
                args: vec![script.to_str().unwrap().to_string()],
                env: Default::default(),
            },
            enabled: true,
        };
        let mcp_tools = manager.sync(&[spec], &runtimes).await;
        assert_eq!(mcp_tools.len(), 2);
        let registry = ToolRegistry::new(builtin_tools());
        registry.set_mcp_tools(mcp_tools);
        assert!(registry.get("mcp__test__mutate").is_some());

        let tools = Arc::new(registry);
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "mcp__test__mutate",
                serde_json::json!({ "target": "target.txt" }),
                1,
            ),
            text_step("done"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();

        // Answer the resulting ask with allow.
        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(15),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        // The MCP tool (non-read-only) must have been gated by `ask`, then run.
        let evs = drain_events(&mut events, 300).await;
        assert_eq!(
            evs.iter().filter(|e| e.kind == "permission.asked").count(),
            1,
            "MCP tool must go through the permission gate"
        );
        let messages = session.messages_snapshot().await;
        let completed = messages.iter().flat_map(|m| &m.parts).any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { output, .. }, .. }
                if name == "mcp__test__mutate" && output.contains("mutated target.txt"))
        });
        assert!(completed, "MCP tool must execute after allow");

        std::fs::remove_dir_all(&base).ok();
    }

    // ------------------------------------------------------------------
    // Plugin hooks (event-observer extension points)
    // ------------------------------------------------------------------

    /// Marker that makes the veto plugin's match unique to its own test.
    ///
    /// The plugin host is global, so a fixture that vetoed *every* `bash` call
    /// would also veto bash calls made by other tests running concurrently
    /// (e.g. `unknown_bash_asks_then_executes_when_allowed`). Matching on this
    /// marker keeps the veto scoped to the command this test issues.
    const VETO_BASH_MARKER: &str = "must-survive.txt";

    /// A plugin that vetoes only its own `bash` call at the `before.tool` hook.
    struct VetoBash;

    #[async_trait::async_trait]
    impl crate::plugin::BebokPlugin for VetoBash {
        fn name(&self) -> &str {
            "test-veto-bash"
        }
        async fn on_hook(
            &self,
            hook: crate::plugin::Hook,
            payload: &mut serde_json::Value,
        ) -> crate::plugin::HookResult {
            use crate::plugin::HookResult;
            if hook != crate::plugin::Hook::BEFORE_TOOL
                || payload.get("tool").and_then(|v| v.as_str()) != Some("bash")
            {
                return HookResult::Continue;
            }
            let mine = payload
                .get("input")
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str())
                .is_some_and(|cmd| cmd.contains(VETO_BASH_MARKER));
            if !mine {
                return HookResult::Continue;
            }
            if let Some(serde_json::Value::Bool(allowed)) = payload.get_mut("allowed") {
                *allowed = false;
            }
            HookResult::Changed
        }
    }

    #[tokio::test]
    async fn plugin_hook_vetoes_tool_during_turn() {
        let base = std::env::temp_dir().join(format!("bebok-plugin-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let sentinel = project.join("must-survive.txt");
        tokio::fs::write(&sentinel, "do not delete").await.unwrap();
        // The permission config *allows* bash; only the plugin veto stops it.
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "bash(*)", "action": "allow" } ] }"#,
        );
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("delete must-survive.txt")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "bash",
                serde_json::json!({ "command": "rm -f must-survive.txt" }),
                1,
            ),
            text_step("rm is vetoed"),
        ]));
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let abort = CancellationToken::new();

        // Register the plugin on the global host (what run_turn reads), run the
        // turn, then unregister so no other test sees it.
        let host = PluginHost::global();
        host.register(Arc::new(VetoBash)).await;
        {
            session.try_begin_turn();
            tokio::time::timeout(
                Duration::from_secs(10),
                run_turn(
                    session.clone(),
                    Agent::code(),
                    tools,
                    provider,
                    permission,
                    bus,
                    abort,
                    crate::config::DEFAULT_MODEL,
                ),
            )
            .await
            .expect("turn must finish")
            .unwrap();
            session.end_turn();
        };
        host.unregister("test-veto-bash").await;

        // The plugin vetoed the call before execution: file untouched, part is
        // an Error carrying "denied by plugin".
        assert!(
            tokio::fs::read_to_string(&sentinel).await.is_ok(),
            "plugin veto must prevent execution"
        );
        let messages = session.messages_snapshot().await;
        let vetoed = messages.iter().flat_map(|m| &m.parts).any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Error { error, .. }, .. }
                if name == "bash" && error == "denied by plugin")
        });
        assert!(vetoed, "tool part must be Error with 'denied by plugin'");

        std::fs::remove_dir_all(&base).ok();
    }

    // ------------------------------------------------------------------
    // Naming & alias (orchestrator sub-agent names)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn allocate_child_name_preferred_unique() {
        let base = std::env::temp_dir().join(format!("bebok-name-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        let name1 = session
            .allocate_child_name(Some("auth-flow-audit"), "code")
            .await;
        assert_eq!(name1, "auth-flow-audit");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn allocate_child_name_preferred_normalized() {
        let base = std::env::temp_dir().join(format!("bebok-name-norm-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        // Uppercase + spaces + special chars -> normalized kebab-case
        let name = session
            .allocate_child_name(Some("Auth Flow AUDIT!!"), "code")
            .await;
        assert_eq!(name, "auth-flow-audit");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn allocate_child_name_duplicate_appends_counter() {
        let base = std::env::temp_dir().join(format!("bebok-name-dup-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        let name1 = session.allocate_child_name(Some("fix-ci"), "code").await;
        assert_eq!(name1, "fix-ci");

        let name2 = session.allocate_child_name(Some("fix-ci"), "code").await;
        assert_eq!(name2, "fix-ci-2");

        let name3 = session.allocate_child_name(Some("fix-ci"), "code").await;
        assert_eq!(name3, "fix-ci-3");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn allocate_child_name_fallback_counter() {
        let base = std::env::temp_dir().join(format!("bebok-name-fb-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        // No preferred name -> fallback to <role>-<n> (global counter per session)
        let name1 = session.allocate_child_name(None, "code").await;
        assert_eq!(name1, "code-1");

        let name2 = session.allocate_child_name(None, "code").await;
        assert_eq!(name2, "code-2");

        // Counter is global; next allocation gets 3.
        let name3 = session.allocate_child_name(None, "plan").await;
        assert_eq!(name3, "plan-3");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn allocate_child_name_empty_preferred_falls_back() {
        let base = std::env::temp_dir().join(format!("bebok-name-empty-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        let name = session.allocate_child_name(Some(""), "debug").await;
        assert_eq!(name, "debug-1");

        let name = session.allocate_child_name(Some("   "), "debug").await;
        assert_eq!(name, "debug-2");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn allocate_child_name_long_preferred_truncated() {
        let base = std::env::temp_dir().join(format!("bebok-name-long-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();

        let long = "a-very-very-very-long-name-that-exceeds-the-limit-of-thirty-two-chars";
        let name = session.allocate_child_name(Some(long), "code").await;
        assert!(
            name.len() <= 32,
            "name must be <= 32 chars, got {} ({name})",
            name.len()
        );
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn child_task_serde_uses_camel_case() {
        let task = crate::store::session_state::ChildTask {
            task_id: "abc-123".to_string(),
            description: "do something".to_string(),
            child_session_id: "uuid-child".to_string(),
            name: "fix-ci".to_string(),
            agent: "code".to_string(),
            model: None,
            started_at: 0,
            status: "running".to_string(),
            background: false,
        };
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("taskID").is_some(), "expected camelCase taskID");
        assert!(
            json.get("childSessionID").is_some(),
            "expected camelCase childSessionID"
        );
        assert!(json.get("name").is_some());
        assert!(json.get("agent").is_some());
        // Must NOT have snake_case keys
        assert!(json.get("task_id").is_none());
        assert!(json.get("child_session_id").is_none());
    }

    #[test]
    fn session_alias_serde_compat() {
        // Session without alias (legacy JSON) must deserialize.
        let legacy = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "directory": "/tmp",
            "agent": "code",
            "created_at": 0,
            "updated_at": 0
        });
        let session: crate::session::Session = serde_json::from_value(legacy).unwrap();
        assert_eq!(session.alias, None);
        assert_eq!(session.title, None);

        // Session with alias.
        let with_alias = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "directory": "/tmp",
            "agent": "code",
            "alias": "auth-flow-audit",
            "created_at": 0,
            "updated_at": 0
        });
        let session: crate::session::Session = serde_json::from_value(with_alias).unwrap();
        assert_eq!(session.alias.as_deref(), Some("auth-flow-audit"));

        // Serialization omits None alias.
        let json = serde_json::to_value(&session).unwrap();
        assert!(json.get("alias").is_some()); // present when set

        let session_no_alias = crate::session::Session::new("/tmp", "code");
        let json = serde_json::to_value(&session_no_alias).unwrap();
        assert!(
            json.get("alias").is_none(),
            "None alias must be skipped in serialization"
        );
    }

    #[test]
    fn tool_state_completed_structured_serde_compat() {
        // Completed without structured (legacy) must deserialize.
        let legacy = serde_json::json!({
            "state": "completed",
            "input": {},
            "output": "done",
            "title": "task: code-1"
        });
        let ts: ToolState = serde_json::from_value(legacy).unwrap();
        match &ts {
            ToolState::Completed { structured, .. } => {
                assert!(structured.is_none());
            }
            _ => panic!("expected Completed"),
        }

        // Completed with structured.
        let with_struct = serde_json::json!({
            "state": "completed",
            "input": {},
            "output": "done",
            "title": "task: fix-ci",
            "structured": { "taskID": "abc", "name": "fix-ci" }
        });
        let ts: ToolState = serde_json::from_value(with_struct).unwrap();
        match &ts {
            ToolState::Completed { structured, .. } => {
                assert!(structured.is_some());
                assert_eq!(structured.as_ref().unwrap()["taskID"], "abc");
            }
            _ => panic!("expected Completed"),
        }
    }

    /// WP-DELEGATION: under an active delegation policy a child session gets
    /// no `task`/`fleet`/`task_*` tool definitions; the main thread keeps
    /// them; `off` restores the old behaviour for children.
    #[tokio::test]
    async fn subagents_get_no_delegation_tools_under_a_policy() {
        use crate::agent::request::{DELEGATION_TOOLS, RequestBuilder};
        use crate::config::DelegationMode;

        let base = std::env::temp_dir().join(format!("bebok-deleg-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(project.join(".bebok"))
            .await
            .unwrap();
        let data = base.join("data");

        let build = |store: &Arc<InstanceStore>, agent: &'static str| {
            let store = store.clone();
            let project = project.clone();
            async move {
                let parent = store
                    .create_session(project.to_str().unwrap(), agent, None)
                    .await
                    .unwrap();
                parent.append_user_message("hi").await.unwrap();
                let child = store
                    .create_subagent_session(&parent, "code", None, Some("worker"))
                    .await
                    .unwrap();
                child.append_user_message("do it").await.unwrap();
                let tools = Arc::new(ToolRegistry::new(builtin_tools()));
                for t in [
                    Arc::new(crate::agent::TaskTool::new(Arc::downgrade(&store)))
                        as Arc<dyn bebok_tools::Tool>,
                    Arc::new(crate::agent::FleetTool::new(Arc::downgrade(&store))),
                    Arc::new(crate::agent::TaskWaitTool::new(Arc::downgrade(&store))),
                    Arc::new(crate::agent::TaskStatusTool::new(Arc::downgrade(&store))),
                    Arc::new(crate::agent::TaskCancelTool::new(Arc::downgrade(&store))),
                ] {
                    tools.register_tool(t);
                }
                let preset = Agent::code();
                let names = |state: &Arc<SessionState>| {
                    let tools = tools.clone();
                    let preset = preset.clone();
                    let state = state.clone();
                    async move {
                        let req = RequestBuilder::new(
                            &state,
                            &preset,
                            &tools,
                            "test/model",
                            1024,
                            bebok_llm::Thinking::Off,
                        )
                        .build()
                        .await
                        .unwrap();
                        req.tools.into_iter().map(|t| t.name).collect::<Vec<_>>()
                    }
                };
                (names(&parent).await, names(&child).await)
            }
        };

        // Default policy (auto): parent has the tools, child does not.
        let store = InstanceStore::with_data_dir(data.clone());
        let (parent_tools, child_tools) = build(&store, "code").await;
        for t in ["task", "task_wait", "task_status", "task_cancel"] {
            assert!(parent_tools.iter().any(|n| n == t), "parent lacks {t}");
            assert!(!child_tools.iter().any(|n| n == t), "child got {t}");
        }
        assert!(
            !parent_tools.iter().any(|n| n == "fleet"),
            "fleet is orchestrator-only"
        );
        assert!(child_tools.iter().any(|n| n == "read_file"));
        assert_eq!(DELEGATION_TOOLS.len(), 5);

        // Policy off: the child gets `task` again (depth guard is the backstop).
        std::fs::write(
            project.join(".bebok").join("config.json"),
            r#"{ "delegation": { "mode": "off" } }"#,
        )
        .unwrap();
        let store = InstanceStore::with_data_dir(base.join("data2"));
        let cfg = store
            .get_or_create_instance(project.to_str().unwrap())
            .await
            .unwrap()
            .config_snapshot();
        assert_eq!(cfg.delegation.mode, DelegationMode::Off);
        let (_, child_tools) = build(&store, "code").await;
        assert!(child_tools.iter().any(|n| n == "task"), "{child_tools:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// F9-7: running a child through `prepare_child` + `run_child` writes
    /// token-free status rows into the PARENT transcript — started, a
    /// first-tool progress row, and the closing "finished in … · N tok ·
    /// 1 file changed" line — and none of them reaches the provider request.
    #[tokio::test]
    async fn child_run_writes_status_rows_into_the_parent_transcript() {
        use crate::agent::delegation::{ChildSpec, prepare_child, run_child};
        use crate::agent::request::RequestBuilder;

        let base = std::env::temp_dir().join(format!("bebok-status-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        write_project_permission(
            &project,
            r#"{ "rules": [ { "pattern": "write_file(*)", "action": "allow" } ] }"#,
        );
        let store = InstanceStore::with_data_dir(base.join("data"));
        let instance = store
            .get_or_create_instance(project.to_str().unwrap())
            .await
            .unwrap();
        let parent = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        parent.append_user_message("build it").await.unwrap();
        // The parent is mid-turn: its assistant message holds the `task` call.
        {
            let mut messages = parent.messages.write().await;
            let mut m = crate::session::Message::new(crate::session::Role::Assistant);
            m.add_tool_call("t1".into(), "task".into(), serde_json::json!({}));
            messages.push(m);
        }

        let provider: Arc<dyn Provider> = Arc::new(ScriptProvider::new(vec![
            tool_step(
                "write_file",
                serde_json::json!({ "path": "orders.ts", "content": "export {};" }),
                1,
            ),
            text_step(
                "Status: PASS
wrote orders.ts",
            ),
        ]));
        let name = parent.allocate_child_name(Some("api-orders"), "code").await;
        let spec = ChildSpec {
            store: store.clone(),
            instance: instance.clone(),
            parent: parent.clone(),
            agent: Agent::code(),
            agent_name: "code".into(),
            model: "mock/model".into(),
            provider,
            name,
            prompt: "write orders.ts".into(),
            images: Vec::new(),
            parent_abort: CancellationToken::new(),
            parent_session_id: parent.id().to_string(),
            directory: parent.directory().to_string(),
            max_concurrent: 3,
            background: true,
            origin: "task",
        };
        let prepared = prepare_child(spec).await.unwrap();
        let outcome = run_child(prepared).await;
        assert_eq!(outcome.status, "completed");
        assert!(outcome.text.starts_with("Status: PASS"));

        let messages = parent.messages_snapshot().await;
        assert_eq!(messages.len(), 2, "status rows never add messages");
        let rows: Vec<(String, String)> = messages[1]
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Status { kind, text, .. } => Some((kind.clone(), text.clone())),
                _ => None,
            })
            .collect();
        assert!(
            rows.iter()
                .any(|(k, t)| k == "task.started" && t == "api-orders started (code · mock/model)"),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(k, t)| k == "task.progress" && t.starts_with("api-orders: write_file")),
            "first-tool milestone: {rows:?}"
        );
        let ended = rows
            .iter()
            .find(|(k, _)| k == "task.ended")
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        assert!(ended.starts_with("api-orders finished in "), "{ended}");
        assert!(ended.ends_with(" · 1 file changed"), "{ended}");
        // Rows stay out of the model's context: the provider request built
        // from the parent transcript has only the tool call, no status text.
        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let req = RequestBuilder::new(
            &parent,
            &Agent::code(),
            &tools,
            "mock/model",
            1024,
            bebok_llm::Thinking::Off,
        )
        .build()
        .await
        .unwrap();
        let dumped = serde_json::to_string(&req.messages).unwrap();
        assert!(!dumped.contains("finished in"), "{dumped}");
        assert!(!dumped.contains("api-orders started"), "{dumped}");
        let _ = std::fs::remove_dir_all(&base);
    }
    // ------------------------------------------------------------------
    // WP-M1 (F10-5): the runtime mock provider through the real turn runner
    // ------------------------------------------------------------------

    /// `[write]` -> `write_file` tool call -> `permission.asked` (default
    /// rules ask for a mutating tool) -> allowed -> `mock-1.txt` exists ->
    /// the deterministic reply lands in the transcript.
    #[tokio::test]
    async fn mock_provider_writes_a_file_after_a_permission_ask() {
        let base = std::env::temp_dir().join(format!("bebok-mock-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        tokio::fs::create_dir_all(&project).await.unwrap();
        let data = base.join("data");

        let store = InstanceStore::with_data_dir(data.clone());
        let session = store
            .create_session(project.to_str().unwrap(), "code", None)
            .await
            .unwrap();
        session
            .append_user_message("[write] create a note")
            .await
            .unwrap();

        let tools = Arc::new(ToolRegistry::new(builtin_tools()));
        let provider: Arc<dyn Provider> = Arc::new(bebok_llm::MockProvider::new());
        let permission = Arc::new(PermissionEngine::load_with_global(&project, None));
        let bus = store.bus();
        let mut events = bus.subscribe();
        let resolver = tokio::spawn(resolve_first_ask(
            session.clone(),
            bus.subscribe(),
            PermissionAnswer {
                allow: true,
                always: false,
            },
        ));

        let abort = CancellationToken::new();
        session.try_begin_turn();
        tokio::time::timeout(
            Duration::from_secs(10),
            run_turn(
                session.clone(),
                Agent::code(),
                tools,
                provider,
                permission,
                bus,
                abort,
                crate::config::DEFAULT_MODEL,
            ),
        )
        .await
        .expect("turn must finish after the ask is resolved")
        .unwrap();
        session.end_turn();
        resolver.await.unwrap();

        let content = tokio::fs::read_to_string(project.join("mock-1.txt"))
            .await
            .expect("mock-1.txt written by the mock's write_file call");
        assert_eq!(content, "mock write #1\n");

        let messages = session.messages_snapshot().await;
        let completed = messages[1].parts.iter().any(|p| {
            matches!(p, Part::Tool { name, state: ToolState::Completed { .. }, .. }
                if name == "write_file")
        });
        assert!(completed, "write_file must complete once allowed");
        assert_eq!(
            messages.last().unwrap().text_content(),
            "Mock reply to: [write] create a note"
        );

        let evs = drain_events(&mut events, 300).await;
        let asked = evs
            .iter()
            .find(|e| e.kind == "permission.asked")
            .expect("permission.asked for the mock write");
        assert_eq!(asked.properties["toolName"], "write_file");
        assert!(evs.iter().any(|e| e.kind == "permission.resolved"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
