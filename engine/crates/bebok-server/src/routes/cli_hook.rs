//! CLI hook bridge: `POST /cli-hook/{session}/{pre|post|stop}`.
//!
//! A CLI provider (Claude Code, see `crates/bebok-cli-agents`) is configured —
//! by the managed profile generated elsewhere — to run
//!
//! ```text
//! "<bebok-server>" cli-hook pre
//! ```
//!
//! before every tool call, `post` after it and `stop` when the turn ends. The
//! `cli-hook` subcommand (see [`crate::cli`]) reads the CLI's hook JSON from
//! stdin, POSTs it here and prints our answer on stdout, which is exactly the
//! contract a Claude Code hook speaks.
//!
//! Why the detour: CLI tool use never touches Bebok's permission engine, so a
//! `claude` that runs `rm -rf` would do it unasked, and its edits would be
//! invisible to the Changes panel. This route closes that hole without
//! rewriting the CLI's tools into MCP servers — the CLI keeps using its own
//! tools, but every call passes through Bebok's gate, popup and tracker.
//!
//! Authorisation (see `bebok_core::cli_turn_token`):
//!
//! - **turn token** — `Authorization: Bearer <token>`, issued per turn to the
//!   CLI provider, revocable, scoped to one session. Missing/unknown → `401`,
//!   token of another session → `403`.
//! - **loopback only** — the route is exempt from the engine capability token
//!   (a hook command line cannot carry it), so it additionally refuses
//!   non-loopback peers whenever the transport reports one.
//!
//! Fail-closed: every failure path answers `deny` (never a silent allow) — the
//! handler's answer is what `cli-hook` prints, and `cli-hook` itself denies when
//! the engine is unreachable.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_core::agent::exec::ToolOutcome;
use bebok_core::agent::gate::{GateCtx, resolve_permission};
use bebok_core::change_tracking::Tracker;
use bebok_core::cli_turn_token::TurnTokenStore;
use bebok_core::permission::CompiledLayer;

use crate::state::AppState;

/// How long Bebok waits for a user decision on a `pre` hook.
///
/// The generated profile asks the CLI for a 600 s hook timeout; answering 5 s
/// earlier means the CLI gets a real `deny` with a readable reason instead of
/// "hook timed out", which it reports as a tool failure. Overridable for tests
/// and for users who want a shorter leash.
pub const PRE_ANSWER_TIMEOUT: Duration = Duration::from_secs(595);

/// Environment variable overriding [`PRE_ANSWER_TIMEOUT`] (whole seconds).
pub const PRE_TIMEOUT_ENV: &str = "BEBOK_CLI_HOOK_TIMEOUT_SECS";

/// The engine's own listening URL, e.g. `http://127.0.0.1:8787`. Set once by
/// `server::serve` after binding (the desktop sidecar binds a random port).
///
/// The registry itself lives in `bebok_core::cli_turn_token`, so the delegation
/// path (`fleet` / `task`, which is core-side) can issue hook credentials too.
pub fn set_engine_base_url(url: impl Into<String>) {
    bebok_core::cli_turn_token::set_engine_base_url(url);
}

/// Issue the per-turn hook environment for a CLI session.
///
/// Returns `(token, env)`: the token so the caller can revoke it when the turn
/// ends, and the environment (`BEBOK_HOOK_URL`, `BEBOK_HOOK_TOKEN`) the CLI
/// child must see. `None` when the engine does not know its own address — then
/// no hooks are wired at all, which is safer than a URL pointing nowhere.
pub fn issue_hook_env(session: &str) -> Option<(String, Vec<(String, String)>)> {
    bebok_core::cli_turn_token::issue_hook_env(session)
}

/// Revoke a turn token issued by [`issue_hook_env`].
pub fn revoke_hook_token(token: &str) {
    bebok_core::cli_turn_token::revoke_hook_token(token);
}

/// One hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookEvent {
    /// Before a tool call: gate it (build-test policy, permission engine, jev,
    /// popup) and snapshot files about to be written.
    Pre,
    /// After a tool call: record/publish the change.
    Post,
    /// End of the CLI turn.
    Stop,
}

impl HookEvent {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pre" => Some(Self::Pre),
            "post" => Some(Self::Post),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }
}

/// A CLI tool call translated into Bebok's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MappedCall {
    /// Tool name as the permission engine sees it, e.g. `cli:Bash`.
    engine_tool: String,
    /// Arguments shaped so the engine's canonical call string reads naturally:
    /// `cli:Bash(npm test)`, `cli:Edit(src/x.rs)`.
    args: Value,
    /// Read-only tools default to `Allow`; anything that can mutate does not.
    read_only: bool,
    /// Project-relative path the CLI is about to write, for the change tracker.
    tracked_path: Option<String>,
}

/// Tools that can change the tree (everything else is treated as read-only).
const MUTATING: &[&str] = &["Bash", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Tools whose `tool_input` carries a single project path.
const PATH_TOOLS: &[&str] = &[
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Read",
    "NotebookRead",
    "Glob",
    "Grep",
    "LS",
];

/// Translate `(tool_name, tool_input)` into the engine's rule text.
///
/// The `cli:` prefix is deliberate: a rule written for Bebok's own `bash` tool
/// must not silently authorise an arbitrary command line the CLI assembled, and
/// vice versa. Users who want one rule for both write it twice.
fn map_call(tool_name: &str, input: &Value, root: &std::path::Path) -> MappedCall {
    let read_only = !MUTATING.contains(&tool_name);
    let engine_tool = format!("cli:{tool_name}");

    if tool_name == "Bash" {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return MappedCall {
            engine_tool,
            args: json!({ "command": command }),
            read_only,
            tracked_path: None,
        };
    }

    if PATH_TOOLS.contains(&tool_name) {
        let raw = ["file_path", "notebook_path", "path"]
            .iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
            .unwrap_or_default();
        let rel = project_relative(raw, root);
        return MappedCall {
            engine_tool,
            args: json!({ "path": rel }),
            read_only,
            tracked_path: (!read_only).then_some(rel),
        };
    }

    if let Some(url) = input.get("url").and_then(Value::as_str) {
        return MappedCall {
            engine_tool,
            args: json!({ "url": url }),
            read_only,
            tracked_path: None,
        };
    }

    // Anything else: match on the serialised input, so a rule can still pin a
    // specific call (`cli:WebSearch({"query":"…"})`).
    MappedCall {
        engine_tool,
        args: Value::String(serde_json::to_string(input).unwrap_or_default()),
        read_only,
        tracked_path: None,
    }
}

/// The tracker and the permission engine work with project-relative paths; the
/// CLI hands us absolute ones. Already-relative inputs pass through unchanged.
///
/// Both sides are canonicalized before comparison: the CLI may hand us a
/// spelling with symlinks or 8.3 components (`RUNNER~1`) while the session
/// root is already canonical (and vice versa on Windows vs Unix temp dirs).
fn project_relative(raw: &str, root: &std::path::Path) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let path = std::path::Path::new(raw);
    if path.is_absolute() {
        let canon_raw = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if let Ok(stripped) = canon_raw.strip_prefix(&canon_root) {
            return stripped.to_string_lossy().replace('\\', "/");
        }
    }
    raw.to_string()
}

/// The answer a hook prints. Shape of `PreToolUse`'s decision output
/// ([verify] against the installed CLI; JSON decision output is the documented
/// alternative to exit code 2).
fn decision_answer(decision: &str, reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    })
}

fn allow(reason: &str) -> Value {
    decision_answer("allow", reason)
}

fn deny(reason: &str) -> Value {
    decision_answer("deny", reason)
}

fn status_json(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn pre_answer_timeout() -> Duration {
    std::env::var(PRE_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(PRE_ANSWER_TIMEOUT)
}

/// Largest hook payload we accept. `tool_input` can carry a whole file (the CLI
/// sends `Write`'s new contents), so this is generous but still bounded.
const MAX_HOOK_BODY: usize = 32 * 1024 * 1024;

/// `POST /cli-hook/{session}/{event}`.
///
/// Takes the raw [`Request`] (rather than a `String` body extractor) so the
/// peer address can be read from the connection info the server installs.
pub async fn hook(
    State(state): State<AppState>,
    Path((session, event)): Path<(String, String)>,
    req: Request,
) -> Response {
    // 1. Transport check: the turn token is a per-turn credential, but a
    //    non-loopback peer has no business here at all. When the transport
    //    does not report a peer (unit tests, a Unix socket) this is skipped —
    //    the turn token still gates the call.
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>()
        && !is_loopback(addr)
    {
        tracing::warn!(addr = %addr, "rejected CLI hook from a non-loopback peer");
        return status_json(StatusCode::FORBIDDEN, "CLI hooks are loopback only");
    }

    // 2. Turn token: unknown/expired -> 401, wrong session -> 403.
    let token = bearer(req.headers());
    let Some(scope) = token.and_then(|t| TurnTokenStore::global().validate(t)) else {
        return status_json(StatusCode::UNAUTHORIZED, "unknown or expired hook token");
    };
    if scope.session != session {
        return status_json(
            StatusCode::FORBIDDEN,
            "hook token is scoped to another session",
        );
    }

    let Some(event) = HookEvent::parse(&event) else {
        return status_json(StatusCode::NOT_FOUND, "unknown hook event");
    };

    let Ok(session_id) = Uuid::parse_str(&session) else {
        return status_json(StatusCode::BAD_REQUEST, "invalid session id");
    };

    let body = match axum::body::to_bytes(req.into_body(), MAX_HOOK_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return status_json(StatusCode::PAYLOAD_TOO_LARGE, "hook payload too large"),
    };
    // An empty or unparsable body is not fatal for `post`/`stop`; a `pre` hook
    // without a tool name simply denies (fail-closed).
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    match event {
        HookEvent::Pre => match run_pre(&state, session_id, &payload).await {
            Ok(answer) => (StatusCode::OK, Json(answer)).into_response(),
            Err(reason) => {
                tracing::warn!(%session_id, %reason, "CLI pre hook denied");
                (StatusCode::OK, Json(deny(&reason))).into_response()
            }
        },
        HookEvent::Post => match run_post(&state, session_id, &payload).await {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(reason) => status_json(StatusCode::BAD_REQUEST, &reason),
        },
        HookEvent::Stop => match run_stop(&state, session_id, &payload).await {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(reason) => status_json(StatusCode::BAD_REQUEST, &reason),
        },
    }
}

/// Gate one CLI tool call. `Err` is the deny reason.
async fn run_pre(state: &AppState, session_id: Uuid, payload: &Value) -> Result<Value, String> {
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| "malformed PreToolUse payload: no tool_name".to_string())?
        .to_string();
    let input = payload
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let session = state
        .store
        .open_session(session_id)
        .await
        .map_err(|e| format!("session {session_id} is not open: {e}"))?;
    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(|e| format!("no instance for the session directory: {e}"))?;
    let config = instance.config_snapshot();
    let mapped = map_call(
        &tool_name,
        &input,
        std::path::Path::new(session.directory()),
    );

    // 1. Build & test policy: a hard gate that runs before the permission
    //    engine, exactly like the in-process agent loop does (`turn.rs`).
    if tool_name == "Bash"
        && let Some(denied) =
            bebok_core::agent::check_build_test_policy(&config, "bash", &mapped.args)
    {
        return Err(match denied {
            ToolOutcome::Denied(message) => message.to_string(),
            _ => "blocked by the build & test policy".to_string(),
        });
    }

    // 2. Change tracking: capture the pre-write state so the Changes panel can
    //    diff (and revert) what the CLI is about to do.
    if let Some(path) = mapped.tracked_path.as_deref().filter(|p| !p.is_empty()) {
        let tracker = Tracker::new(session.directory(), session.disk_dir());
        match tracker.snapshot_before_write(path) {
            Ok(_) => {}
            // Refusing the call because tracking failed would be overreach: the
            // user still gets the diff baseline from git in most projects.
            Err(e) => tracing::warn!(%session_id, path, "CLI pre hook could not snapshot: {e}"),
        }
    }

    // 3. Permission engine -> jev plugin -> user popup, synchronously (the hook
    //    blocks the CLI until we answer).
    let bus = state.store.bus();
    let abort = CancellationToken::new();
    let meta = session.meta_snapshot().await;
    let agent = instance.resolve_agent(&meta.agent);
    let agent_layer = if agent.permissions.is_empty() {
        None
    } else {
        Some(CompiledLayer::compile(&agent.permissions))
    };
    let gate = GateCtx {
        state: &session,
        bus: &bus,
        permission: &instance.permission,
        agent_layer: agent_layer.as_ref(),
        tools: &instance.tools,
        abort: &abort,
        agent_name: &agent.name,
        // CLI tool calls are not message parts: nothing to stamp the verdict on.
        assistant_idx: 0,
    };
    let call_id = format!("cli-hook-{tool_name}-{}", Uuid::new_v4());
    let timeout = pre_answer_timeout();
    let timer = {
        let abort = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            abort.cancel();
        })
    };
    let outcome = resolve_permission(&gate, &call_id, &mapped.engine_tool, &mapped.args).await;
    timer.abort();

    match outcome {
        ToolOutcome::Run => Ok(allow("allowed by Bebok")),
        ToolOutcome::Denied(message) => Err(message.to_string()),
        ToolOutcome::Aborted if abort.is_cancelled() => Err(format!(
            "no answer within {}s — denied (the Bebok window may be closed)",
            timeout.as_secs()
        )),
        ToolOutcome::Aborted => Err("denied: the turn was aborted".to_string()),
    }
}

/// Register a finished CLI tool call: the tracker already holds the baseline
/// from `pre`, so this publishes the change and reports it.
async fn run_post(state: &AppState, session_id: Uuid, payload: &Value) -> Result<Value, String> {
    let session = state
        .store
        .open_session(session_id)
        .await
        .map_err(|e| format!("session {session_id} is not open: {e}"))?;
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let input = payload
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let response = payload.get("tool_response").cloned().unwrap_or(Value::Null);

    let tracker = Tracker::new(session.directory(), session.disk_dir());
    let changed: Vec<Value> = tracker
        .list()
        .into_iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "added": entry.added,
                "removed": entry.removed,
                "baseline": entry.baseline,
            })
        })
        .collect();

    // Presentation only: a real `ExternalTool` stream event is phase 05's job
    // (`StreamEvent` has no such variant yet), so this is an SSE event the
    // client can ignore today and render later.
    state.store.bus().publish(
        bebok_core::event::Event::new(
            "tool.external",
            session.directory(),
            &session_id.to_string(),
        )
        .with_properties(json!({
            "toolName": tool_name,
            "input": input,
            "response": response,
            "changed": changed,
            "source": "cli-hook",
        })),
    );

    Ok(json!({}))
}

/// End of the CLI turn: report what the user has to look at.
async fn run_stop(state: &AppState, session_id: Uuid, _payload: &Value) -> Result<Value, String> {
    let session = state
        .store
        .open_session(session_id)
        .await
        .map_err(|e| format!("session {session_id} is not open: {e}"))?;
    let tracker = Tracker::new(session.directory(), session.disk_dir());
    let changed = tracker.tracked_count();
    tracing::debug!(%session_id, changed, "CLI turn finished");
    state.store.bus().publish(
        bebok_core::event::Event::new(
            "cli.turn.stopped",
            session.directory(),
            &session_id.to_string(),
        )
        .with_properties(json!({ "changedFiles": changed })),
    );
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root that is absolute on every platform (`/proj` is not absolute
    /// on Windows, so `strip_prefix` never matches there).
    fn test_root() -> std::path::PathBuf {
        std::env::temp_dir().join("proj")
    }

    fn test_path(name: &str) -> String {
        test_root()
            .join(name)
            .display()
            .to_string()
            .replace('\\', "/")
    }

    #[test]
    fn bash_maps_to_a_rule_friendly_call_string() {
        let mapped = map_call(
            "Bash",
            &json!({ "command": "npm test", "description": "run tests" }),
            &test_root(),
        );
        assert_eq!(mapped.engine_tool, "cli:Bash");
        assert_eq!(mapped.args, json!({ "command": "npm test" }));
        assert!(!mapped.read_only);
        assert_eq!(mapped.tracked_path, None);
    }

    #[test]
    fn edit_maps_absolute_paths_to_project_relative_ones() {
        let mapped = map_call(
            "Edit",
            &json!({ "file_path": test_path("src/x.rs"), "old_string": "a", "new_string": "b" }),
            &test_root(),
        );
        assert_eq!(mapped.engine_tool, "cli:Edit");
        assert_eq!(mapped.args, json!({ "path": "src/x.rs" }));
        assert!(!mapped.read_only);
        assert_eq!(mapped.tracked_path.as_deref(), Some("src/x.rs"));
    }

    #[test]
    fn read_tools_are_read_only_and_never_tracked() {
        let mapped = map_call(
            "Read",
            &json!({ "file_path": test_path("src/x.rs") }),
            &test_root(),
        );
        assert!(mapped.read_only);
        assert_eq!(mapped.tracked_path, None);
        assert_eq!(mapped.args, json!({ "path": "src/x.rs" }));
    }

    #[test]
    fn web_fetch_maps_to_its_url() {
        let mapped = map_call(
            "WebFetch",
            &json!({ "url": "http://127.0.0.1:8787/session" }),
            &test_root(),
        );
        assert_eq!(
            mapped.args,
            json!({ "url": "http://127.0.0.1:8787/session" })
        );
    }

    #[test]
    fn unknown_tools_match_on_their_serialised_input() {
        let mapped = map_call("TodoWrite", &json!({ "todos": [] }), &test_root());
        assert_eq!(mapped.engine_tool, "cli:TodoWrite");
        assert_eq!(mapped.args, Value::String("{\"todos\":[]}".to_string()));
    }

    #[test]
    fn project_relative_leaves_foreign_and_relative_paths_alone() {
        let root = test_root();
        assert_eq!(project_relative(&test_path("a/b.rs"), &root), "a/b.rs");
        assert_eq!(project_relative("a/b.rs", &root), "a/b.rs");
        // A path outside the root keeps its `..` escape visible (whatever
        // the platform's absolute form is); the permission engine and
        // pathguard judge the result downstream.
        let outside = root.join("..").join("elsewhere").join("b.rs");
        let outside_str = outside.to_string_lossy().replace('\\', "/");
        assert_eq!(project_relative(&outside_str, &root), "../elsewhere/b.rs");
        assert_eq!(project_relative("", &root), "");
    }

    #[test]
    fn hook_event_parses_only_the_three_events() {
        assert_eq!(HookEvent::parse("pre"), Some(HookEvent::Pre));
        assert_eq!(HookEvent::parse("post"), Some(HookEvent::Post));
        assert_eq!(HookEvent::parse("stop"), Some(HookEvent::Stop));
        assert_eq!(HookEvent::parse("pre-tool"), None);
    }

    #[test]
    fn bearer_extracts_only_the_authorization_scheme() {
        let mut headers = HeaderMap::new();
        assert!(bearer(&headers).is_none());
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(bearer(&headers), Some("abc123"));
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic abc123".parse().unwrap(),
        );
        assert!(bearer(&headers).is_none());
    }

    // ── end to end over the router ────────────────────────────────────────

    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request as HttpRequest, header};
    use tower::ServiceExt as _;

    struct Fixture {
        base: std::path::PathBuf,
        root: std::path::PathBuf,
        state: AppState,
    }

    impl Fixture {
        /// A project whose permission rules make CLI calls decidable without a
        /// popup: `cli:*` is allowed unless a test overrides `rules`.
        fn new(tag: &str, rules: &str) -> Self {
            let base =
                std::env::temp_dir().join(format!("bebok-cli-hook-{tag}-{}", uuid::Uuid::new_v4()));
            let root = base.join("root");
            std::fs::create_dir_all(&root).unwrap();
            let store = bebok_core::InstanceStore::with_data_dir(base.join("data"));
            // Config lives in the store's database (no config file any more).
            bebok_core::config::store::write_delta_at(
                &store.config_db_path(),
                &bebok_core::config::project_scope(&root),
                &serde_json::json!({ "permission": { "rules": serde_json::from_str::<serde_json::Value>(&format!("[{rules}]")).unwrap() } }),
            )
            .unwrap();
            let state = AppState {
                store,
                #[cfg(not(target_os = "android"))]
                ptys: Arc::new(bebok_pty::PtyManager::new()),
                debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
                llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
                remote_extensions: Arc::new(tokio::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
                command_queue: Arc::new(tokio::sync::Mutex::new(
                    crate::state::CommandRegistry::new(),
                )),
                batches: Arc::new(bebok_core::BatchStore::new()),
            };
            Self { base, root, state }
        }

        fn router(&self) -> axum::Router {
            crate::routes::build_api_router().with_state(self.state.clone())
        }

        async fn session(&self) -> Arc<bebok_core::SessionState> {
            let session = self
                .state
                .store
                .create_session(&self.root.to_string_lossy(), "code", None)
                .await
                .unwrap();
            // Materialise the instance (config, permission engine, tools), as
            // the first hook would.
            self.state
                .store
                .get_or_create_instance(session.directory())
                .await
                .unwrap();
            session
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn hook_request(
        session: &str,
        event: &str,
        token: Option<&str>,
        body: &str,
    ) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder()
            .method(Method::POST)
            .uri(format!("/cli-hook/{session}/{event}"))
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn send(router: axum::Router, req: HttpRequest<Body>) -> (StatusCode, serde_json::Value) {
        let res = router.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn decision(value: &Value) -> &str {
        value["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or_default()
    }

    /// A project path embedded in a JSON test body. Backslashes would form
    /// invalid JSON escapes on Windows, so they become forward slashes (the
    /// hook normalises them the same way in `project_relative`).
    fn json_path(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    #[tokio::test]
    async fn missing_token_is_401() {
        let fx = Fixture::new("no-token", r#""#);
        let session = fx.session().await;
        let (status, _) = send(
            fx.router(),
            hook_request(
                &session.id().to_string(),
                "pre",
                None,
                r#"{"tool_name":"Read","tool_input":{}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unknown_token_is_401() {
        let fx = Fixture::new("bad-token", r#""#);
        let session = fx.session().await;
        let (status, _) = send(
            fx.router(),
            hook_request(
                &session.id().to_string(),
                "pre",
                Some("not-a-token"),
                r#"{"tool_name":"Read","tool_input":{}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_of_another_session_is_403() {
        let fx = Fixture::new("wrong-session", r#""#);
        let session = fx.session().await;
        let (other_token, _) = TurnTokenStore::global().issue("some-other-session");
        let (status, _) = send(
            fx.router(),
            hook_request(
                &session.id().to_string(),
                "pre",
                Some(&other_token),
                r#"{"tool_name":"Read","tool_input":{}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        TurnTokenStore::global().revoke(&other_token);
    }

    #[tokio::test]
    async fn a_revoked_token_is_401() {
        let fx = Fixture::new("revoked", r#""#);
        let session = fx.session().await;
        let (token, _) = TurnTokenStore::global().issue(&session.id().to_string());
        TurnTokenStore::global().revoke(&token);
        let (status, _) = send(
            fx.router(),
            hook_request(
                &session.id().to_string(),
                "pre",
                Some(&token),
                r#"{"tool_name":"Read","tool_input":{}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unknown_event_is_404() {
        let fx = Fixture::new("bad-event", r#""#);
        let session = fx.session().await;
        let (token, _) = TurnTokenStore::global().issue(&session.id().to_string());
        let (status, _) = send(
            fx.router(),
            hook_request(&session.id().to_string(), "pre-tool", Some(&token), "{}"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn pre_allows_a_rule_matched_call_and_denies_a_denied_one() {
        let fx = Fixture::new(
            "decide",
            // Rule order matters (first match wins within a layer), so the
            // narrow deny comes first — exactly as a user would write it.
            r#"{ "pattern": "cli:Bash(rm *)", "action": "deny" },
               { "pattern": "cli:*", "action": "allow" }"#,
        );
        let session = fx.session().await;
        let id = session.id().to_string();
        let (token, _) = TurnTokenStore::global().issue(&id);

        let (status, value) = send(
            fx.router(),
            hook_request(
                &id,
                "pre",
                Some(&token),
                r#"{"tool_name":"Bash","tool_input":{"command":"npm run build"}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(decision(&value), "allow", "{value}");

        // The deny rule is matched on `cli:Bash(...)` — Bebok's own `bash(...)`
        // rules deliberately do not apply to a CLI-assembled command line.
        let (status, value) = send(
            fx.router(),
            hook_request(
                &id,
                "pre",
                Some(&token),
                r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf src"}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(decision(&value), "deny", "{value}");
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn pre_without_an_answer_denies_after_the_timeout() {
        // No rule for `cli:Edit`, so the engine would ask the user — and there
        // is nobody to answer in a test. The hook must deny instead of hanging.
        let fx = Fixture::new("timeout", r#""#);
        let session = fx.session().await;
        let id = session.id().to_string();
        let (token, _) = TurnTokenStore::global().issue(&id);
        // SAFETY: test-only env var; the value only shortens a timeout.
        unsafe { std::env::set_var(PRE_TIMEOUT_ENV, "1") };
        let (status, value) = send(
            fx.router(),
            hook_request(
                &id,
                "pre",
                Some(&token),
                r#"{"tool_name":"Edit","tool_input":{"file_path":"REPLACED","old_string":"a","new_string":"b"}}"#
                    .replace("REPLACED", &json_path(&fx.root.join("src/x.rs")))
                    .as_str(),
            ),
        )
        .await;
        unsafe { std::env::remove_var(PRE_TIMEOUT_ENV) };
        assert_eq!(status, StatusCode::OK);
        assert_eq!(decision(&value), "deny", "{value}");
        let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default();
        assert!(reason.contains("no answer"), "{reason}");
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn pre_snapshots_a_file_before_the_cli_writes_it() {
        let fx = Fixture::new("snapshot", r#"{ "pattern": "cli:*", "action": "allow" }"#);
        let session = fx.session().await;
        let id = session.id().to_string();
        std::fs::create_dir_all(fx.root.join("src")).unwrap();
        std::fs::write(fx.root.join("src/x.rs"), "before\n").unwrap();

        let (token, _) = TurnTokenStore::global().issue(&id);
        let (status, value) = send(
            fx.router(),
            hook_request(
                &id,
                "pre",
                Some(&token),
                &format!(
                    r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
                    json_path(&fx.root.join("src/x.rs"))
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(decision(&value), "allow", "{value}");

        // The baseline exists before the edit happens: the tracker recorded the
        // pre-write bytes, so the Changes panel can diff what the CLI does.
        let tracker = Tracker::new(session.directory(), session.disk_dir());
        assert!(
            tracker.tracks("src/x.rs"),
            "pre hook must snapshot the file"
        );
        assert_eq!(tracker.list().len(), 1);
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn post_reports_the_change_the_cli_made() {
        let fx = Fixture::new("post", r#"{ "pattern": "cli:*", "action": "allow" }"#);
        let session = fx.session().await;
        let id = session.id().to_string();
        std::fs::create_dir_all(fx.root.join("src")).unwrap();
        std::fs::write(fx.root.join("src/x.rs"), "before\n").unwrap();
        let path = fx.root.join("src/x.rs");
        let (token, _) = TurnTokenStore::global().issue(&id);

        let pre_body = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}"}}}}"#,
            json_path(&path)
        );
        let (_, value) = send(
            fx.router(),
            hook_request(&id, "pre", Some(&token), &pre_body),
        )
        .await;
        assert_eq!(decision(&value), "allow", "{value}");

        // The CLI does the edit; the post hook records it.
        std::fs::write(&path, "before\nafter\n").unwrap();
        let mut events = fx.state.store.bus().subscribe();
        let (status, _) = send(
            fx.router(),
            hook_request(&id, "post", Some(&token), &pre_body),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let tracker = Tracker::new(session.directory(), session.disk_dir());
        let entry = tracker.list().into_iter().next().expect("tracked");
        assert_eq!(entry.path, "src/x.rs");
        assert_eq!(entry.added, 1, "the new line is counted");

        // The change is announced so a client (phase 05 will render it as a
        // tool row) can show it.
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("an event arrives")
            .expect("bus open");
        assert_eq!(event.kind, "tool.external");
        assert_eq!(event.properties["toolName"], "Edit");
        assert_eq!(event.properties["source"], "cli-hook");
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn pre_denies_a_test_command_when_the_build_test_policy_is_off() {
        let fx = Fixture::new("buildtest", r#"{ "pattern": "cli:*", "action": "allow" }"#);
        // Re-write the project scope with `verify.buildTest = off` before the
        // instance is created.
        bebok_core::config::store::write_delta_at(
            &fx.state.store.config_db_path(),
            &bebok_core::config::project_scope(&fx.root),
            &serde_json::json!({
                "verify": { "buildTest": "off" },
                "permission": { "rules": [ { "pattern": "cli:*", "action": "allow" } ] }
            }),
        )
        .unwrap();
        let session = fx.session().await;
        let id = session.id().to_string();
        let (token, _) = TurnTokenStore::global().issue(&id);
        let (status, value) = send(
            fx.router(),
            hook_request(
                &id,
                "pre",
                Some(&token),
                r#"{"tool_name":"Bash","tool_input":{"command":"cargo test --workspace"}}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            decision(&value),
            "deny",
            "an allow rule must not bypass the build & test policy: {value}"
        );
        TurnTokenStore::global().revoke(&token);
    }

    #[tokio::test]
    async fn stop_closes_the_turn_with_the_change_count() {
        let fx = Fixture::new("stop", r#""#);
        let session = fx.session().await;
        let id = session.id().to_string();
        let (token, _) = TurnTokenStore::global().issue(&id);
        let (status, value) = send(
            fx.router(),
            hook_request(&id, "stop", Some(&token), r#"{"hook_event_name":"Stop"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({}));
        TurnTokenStore::global().revoke(&token);
    }

    #[test]
    fn answers_carry_the_decision_shape() {
        let Value::Object(map) = deny("nope") else {
            panic!("expected an object");
        };
        let out = map.get("hookSpecificOutput").expect("hookSpecificOutput");
        assert_eq!(out["hookEventName"], "PreToolUse");
        assert_eq!(out["permissionDecision"], "deny");
        assert_eq!(out["permissionDecisionReason"], "nope");
        assert_eq!(
            allow("ok")["hookSpecificOutput"]["permissionDecision"],
            "allow"
        );
    }
}
