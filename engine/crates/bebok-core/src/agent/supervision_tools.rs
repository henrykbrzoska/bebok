//! WP-DELEGATION (F8-2): supervision tools for the main thread.
//!
//! * `task_status` - list this session's child tasks: live ones with their
//!   queue/run state, last tool, last assistant line and tokens; finished
//!   ones with their outcome (and whether the result was collected yet).
//! * `task_wait` - block (abortably, with a timeout) until any/all of the
//!   selected background children finish, then return their reports. Results
//!   are handed over once: a second `task_wait` for the same id says so.
//! * `task_cancel` - cancel one child by task id or name. Only that child's
//!   token is cancelled; the parent turn keeps going so the model can react.
//!
//! All three are read-only for the permission gate (they never touch the
//! workspace) and operate on the calling session only (`ctx.session_id`).

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use bebok_tools::{Tool, ToolCtx, ToolOutput};

use super::delegation::{TaskProgress, summarize_progress};
use crate::store::{ChildTask, InstanceStore, SessionState, TaskResult};

/// Default / maximum `task_wait` timeout.
pub const DEFAULT_WAIT_SECS: u64 = 600;
pub const MAX_WAIT_SECS: u64 = 3600;

fn parse_uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap_or_else(|_| uuid::Uuid::nil())
}

// `ToolOutput` is the tool-result envelope; the Err path is rare.
#[allow(clippy::result_large_err)]
async fn open_parent(
    store: &Weak<InstanceStore>,
    ctx: &ToolCtx,
    tool: &str,
) -> Result<(Arc<InstanceStore>, Arc<SessionState>), ToolOutput> {
    let Some(store) = store.upgrade() else {
        return Err(ToolOutput::new(
            format!("{tool}: engine store is gone"),
            tool,
        ));
    };
    match store.open_session(parse_uuid(&ctx.session_id)).await {
        Ok(p) => Ok((store, p)),
        Err(_) => Err(ToolOutput::new(format!("{tool}: session not found"), tool)),
    }
}

/// Live view of one running/queued child, as reported by `task_status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveTaskView {
    #[serde(flatten)]
    pub task: ChildTask,
    pub progress: TaskProgress,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub elapsed_ms: i64,
}

/// Build the live view for every child in the parent's task map.
pub async fn live_task_views(store: &InstanceStore, parent: &SessionState) -> Vec<LiveTaskView> {
    let mut out = Vec::new();
    let now = crate::util::now_ms();
    for task in parent.child_tasks_snapshot().await {
        let (progress, input_tokens, output_tokens) =
            match store.open_session(parse_uuid(&task.child_session_id)).await {
                Ok(child) => {
                    let progress = summarize_progress(&child.messages_snapshot().await);
                    let usage = child.meta_snapshot().await.usage;
                    (progress, usage.input_tokens, usage.output_tokens)
                }
                Err(_) => (TaskProgress::default(), 0, 0),
            };
        out.push(LiveTaskView {
            elapsed_ms: now - task.started_at,
            task,
            progress,
            input_tokens,
            output_tokens,
        });
    }
    out.sort_by_key(|v| v.task.started_at);
    out
}

/// Marker appended to a live child line when the supervision detector has
/// flagged it (`TaskProgress.verdict` is `looping` or `wandering`).
/// Returns `None` for a healthy child.
pub fn flag_marker(progress: &TaskProgress) -> Option<String> {
    match progress.verdict.as_str() {
        "looping" => Some("⚠ looping".to_string()),
        "wandering" => Some("⚠ wandering".to_string()),
        _ => None,
    }
}

/// Human-readable status report (what the model reads).
pub fn render_status(live: &[LiveTaskView], finished: &[TaskResult]) -> String {
    if live.is_empty() && finished.is_empty() {
        return "No child tasks in this session yet.".to_string();
    }
    let mut lines = Vec::new();
    if !live.is_empty() {
        lines.push(format!("Live child tasks ({}):", live.len()));
        for v in live {
            let tool = match (&v.progress.last_tool, &v.progress.last_tool_state) {
                (Some(t), Some(s)) => format!("{t} ({s})"),
                (Some(t), None) => t.clone(),
                _ => "-".to_string(),
            };
            let summary = if v.progress.summary.is_empty() {
                "-".to_string()
            } else {
                v.progress.summary.clone()
            };
            let flag = flag_marker(&v.progress)
                .map(|f| format!(" {f}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {name} [{agent}] {status} · taskID={id} · {secs}s · tools={calls} last={tool} · tokens in/out {ti}/{to}{flag}\n  last line: {summary}",
                name = v.task.name,
                agent = v.task.agent,
                status = v.task.status,
                id = v.task.task_id,
                secs = (v.elapsed_ms / 1000).max(0),
                calls = v.progress.tool_calls,
                ti = v.input_tokens,
                to = v.output_tokens,
            ));
        }
    }
    if !finished.is_empty() {
        lines.push(format!("Finished child tasks ({}):", finished.len()));
        for r in finished {
            let secs = ((r.ended_at - r.started_at) / 1000).max(0);
            let collected = if r.collected {
                "result collected"
            } else {
                "result NOT collected yet - call task_wait"
            };
            let err = r
                .error
                .as_deref()
                .map(|e| format!(" · error: {e}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {} [{}] {} · taskID={} · {}s · tokens in/out {}/{} · {}{}",
                r.name,
                r.agent,
                r.status,
                r.task_id,
                secs,
                r.input_tokens,
                r.output_tokens,
                collected,
                err
            ));
        }
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// task_status
// ---------------------------------------------------------------------------

pub struct TaskStatusTool {
    store: Weak<InstanceStore>,
}

impl TaskStatusTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskStatusTool {
    fn name(&self) -> &str {
        "task_status"
    }

    fn description(&self) -> &str {
        "List the sub-agents (child tasks) of this session: queued/running ones with their \
         last tool call, last assistant line, elapsed time and tokens, plus finished ones with \
         their outcome and whether the result was already collected via task_wait. Use it to \
         supervise background tasks without waiting for them."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, _args: Value) -> ToolOutput {
        let (store, parent) = match open_parent(&self.store, &ctx, "task_status").await {
            Ok(v) => v,
            Err(out) => return out,
        };
        let live = live_task_views(&store, &parent).await;
        let finished = parent.task_results_snapshot().await;
        let mut out = ToolOutput::new(render_status(&live, &finished), "task_status");
        out.structured = Some(json!({
            "live": live,
            "finished": finished,
        }));
        out
    }
}

// ---------------------------------------------------------------------------
// task_wait
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct WaitArgs {
    /// Task ids (or names) to wait for; empty = every background child.
    #[serde(default, alias = "taskIDs", alias = "task_ids", alias = "tasks")]
    ids: Vec<String>,
    /// `all` (default) or `any`.
    #[serde(default)]
    mode: Option<String>,
    /// Seconds before giving up (default 600, max 3600).
    #[serde(default, alias = "timeout")]
    timeout_seconds: Option<u64>,
}

/// Which of the selected ids are still live / finished-uncollected.
struct WaitSet {
    /// Ids the caller asked for (or every known background child).
    wanted: Vec<String>,
}

/// Resolve the caller's ids/names to task ids against live + finished tasks.
/// Unknown entries are kept verbatim so the report can name them.
async fn resolve_wait_set(parent: &SessionState, ids: &[String]) -> WaitSet {
    let live = parent.child_tasks_snapshot().await;
    let finished = parent.task_results_snapshot().await;
    if ids.is_empty() {
        let mut wanted: Vec<String> = live
            .iter()
            .filter(|t| t.background)
            .map(|t| t.task_id.clone())
            .collect();
        wanted.extend(
            finished
                .iter()
                .filter(|r| !r.collected)
                .map(|r| r.task_id.clone()),
        );
        return WaitSet { wanted };
    }
    let wanted = ids
        .iter()
        .map(|id| {
            let id = id.trim();
            live.iter()
                .find(|t| t.task_id == id || t.name == id)
                .map(|t| t.task_id.clone())
                .or_else(|| {
                    finished
                        .iter()
                        .rev()
                        .find(|r| r.task_id == id || r.name == id)
                        .map(|r| r.task_id.clone())
                })
                .unwrap_or_else(|| id.to_string())
        })
        .collect();
    WaitSet { wanted }
}

/// Wait outcome (pure part of `task_wait`, unit-tested).
#[derive(Debug, PartialEq, Eq)]
pub enum WaitVerdict {
    /// Every wanted id has a result.
    Complete,
    /// `any` mode and at least one finished.
    Partial,
    /// Nothing usable yet.
    Keep,
}

/// Decide whether the wait is satisfied given which wanted ids are finished.
pub fn wait_verdict(wanted: &[String], finished_ids: &[String], any: bool) -> WaitVerdict {
    let done = wanted.iter().filter(|w| finished_ids.contains(w)).count();
    if done == wanted.len() {
        WaitVerdict::Complete
    } else if any && done > 0 {
        WaitVerdict::Partial
    } else {
        WaitVerdict::Keep
    }
}

/// Render collected results for the model.
pub fn render_results(results: &[TaskResult], pending: &[String], timed_out: bool) -> String {
    let mut sections = Vec::new();
    for r in results {
        let secs = ((r.ended_at - r.started_at) / 1000).max(0);
        let head = format!(
            "# {} [{}] {} · taskID={} · {}s · tokens in/out {}/{}",
            r.name, r.agent, r.status, r.task_id, secs, r.input_tokens, r.output_tokens
        );
        let body = match r.status.as_str() {
            "completed" => {
                if r.text.trim().is_empty() {
                    "(sub-agent produced no output)".to_string()
                } else {
                    r.text.clone()
                }
            }
            _ => format!("(no report) {}", r.error.clone().unwrap_or_default()),
        };
        sections.push(format!("{head}\n{body}"));
    }
    if !pending.is_empty() {
        let note = if timed_out {
            "Timed out; still running (call task_wait again or task_cancel):"
        } else {
            "Still running:"
        };
        sections.push(format!("{note} {}", pending.join(", ")));
    }
    if sections.is_empty() {
        "No results.".to_string()
    } else {
        sections.join("\n\n---\n\n")
    }
}

pub struct TaskWaitTool {
    store: Weak<InstanceStore>,
}

impl TaskWaitTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskWaitTool {
    fn name(&self) -> &str {
        "task_wait"
    }

    fn description(&self) -> &str {
        "Wait for background sub-agents (spawned with task background=true) and return their \
         final reports. `ids` selects task ids or names (default: every uncollected background \
         task); `mode` is `all` (default: wait until every selected task finished) or `any` \
         (return as soon as one finished); `timeout_seconds` (default 600) returns whatever is \
         done plus the list of tasks still running. Each result is handed over once."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Task ids or names to wait for. Empty/omitted = all uncollected background tasks."
                },
                "mode": {
                    "type": "string",
                    "enum": ["all", "any"],
                    "description": "all (default): wait for every selected task; any: return once at least one finished."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Give up after this many seconds (default 600, max 3600) and report what is still running."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let args: WaitArgs = if args.is_null() {
            WaitArgs::default()
        } else {
            match serde_json::from_value(args) {
                Ok(a) => a,
                Err(e) => {
                    return ToolOutput::new(
                        format!("task_wait: invalid arguments: {e}"),
                        "task_wait",
                    );
                }
            }
        };
        let (_store, parent) = match open_parent(&self.store, &ctx, "task_wait").await {
            Ok(v) => v,
            Err(out) => return out,
        };
        let any = args
            .mode
            .as_deref()
            .map(|m| m.trim().eq_ignore_ascii_case("any"))
            .unwrap_or(false);
        let timeout = Duration::from_secs(
            args.timeout_seconds
                .unwrap_or(DEFAULT_WAIT_SECS)
                .clamp(1, MAX_WAIT_SECS),
        );
        let set = resolve_wait_set(&parent, &args.ids).await;
        if set.wanted.is_empty() {
            return ToolOutput::new(
                "task_wait: no background child tasks to wait for (spawn some with `task` \
                 background=true first, or they were all collected already)",
                "task_wait",
            );
        }

        let deadline = tokio::time::Instant::now() + timeout;
        let mut timed_out = false;
        loop {
            // Arm the wake-up before inspecting, so a result that lands
            // between the check and the await is not missed.
            let notified = parent.task_done_notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let finished = parent.task_results_snapshot().await;
            // Already-collected results count as finished too (the caller
            // gets a note instead of a second copy, and never blocks on them).
            let finished_ids: Vec<String> = finished.iter().map(|r| r.task_id.clone()).collect();
            let verdict = wait_verdict(&set.wanted, &finished_ids, any);
            let live = parent.child_tasks_snapshot().await;
            let unknown: Vec<String> = set
                .wanted
                .iter()
                .filter(|w| {
                    !live.iter().any(|t| &t.task_id == *w)
                        && !finished.iter().any(|r| &r.task_id == *w)
                })
                .cloned()
                .collect();
            let all_known_done = set
                .wanted
                .iter()
                .all(|w| finished_ids.contains(w) || unknown.contains(w));
            if verdict != WaitVerdict::Keep || all_known_done || timed_out {
                let results = parent.collect_task_results(&set.wanted).await;
                let pending: Vec<String> = set
                    .wanted
                    .iter()
                    .filter(|w| !results.iter().any(|r| &r.task_id == *w))
                    .map(|w| {
                        live.iter()
                            .find(|t| &t.task_id == w)
                            .map(|t| format!("{} ({})", t.name, t.task_id))
                            .unwrap_or_else(|| {
                                if let Some(r) = finished.iter().find(|r| &r.task_id == w) {
                                    format!("{} ({w}) already collected earlier", r.name)
                                } else if unknown.contains(w) {
                                    format!("{w} (unknown task)")
                                } else {
                                    w.clone()
                                }
                            })
                    })
                    .collect();
                let mut out =
                    ToolOutput::new(render_results(&results, &pending, timed_out), "task_wait");
                out.structured = Some(json!({
                    "results": results,
                    "pending": pending,
                    "timedOut": timed_out,
                }));
                return out;
            }

            tokio::select! {
                _ = ctx.abort.cancelled() => {
                    return ToolOutput::new("task_wait: aborted", "task_wait");
                }
                _ = tokio::time::sleep_until(deadline) => {
                    timed_out = true;
                }
                _ = &mut notified => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// task_cancel
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CancelArgs {
    /// Task id or name.
    #[serde(alias = "taskID", alias = "task_id", alias = "name")]
    id: String,
    #[serde(default)]
    reason: Option<String>,
}

pub struct TaskCancelTool {
    store: Weak<InstanceStore>,
}

impl TaskCancelTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskCancelTool {
    fn name(&self) -> &str {
        "task_cancel"
    }

    fn description(&self) -> &str {
        "Cancel one running or queued sub-agent of this session by task id or name. Only that \
         child stops (its result is recorded as aborted); this turn continues. Use it when a \
         sub-agent went off-track or its work is no longer needed. When a live child is \
         flagged as looping or wandering, cancel it with a matching reason and re-task with \
         a tighter brief."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task id (from task/task_status) or the child's name." },
                "reason": { "type": "string", "description": "Optional note recorded in the log." }
            },
            "required": ["id"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let args: CancelArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutput::new(
                    format!("task_cancel: invalid arguments: {e}"),
                    "task_cancel",
                );
            }
        };
        let (store, parent) = match open_parent(&self.store, &ctx, "task_cancel").await {
            Ok(v) => v,
            Err(out) => return out,
        };
        let Some(task) = parent.find_child_task(args.id.trim()).await else {
            return ToolOutput::new(
                format!(
                    "task_cancel: no running child task '{}' (see task_status)",
                    args.id.trim()
                ),
                "task_cancel",
            );
        };
        let cancelled = parent.abort_child_task(&task.task_id).await;
        if cancelled {
            tracing::info!(
                "session {}: task_cancel {} ({}): {}",
                ctx.session_id,
                task.name,
                task.task_id,
                args.reason.as_deref().unwrap_or("no reason given")
            );
            store.bus().publish(
                crate::event::Event::new("task.aborted", parent.directory(), &ctx.session_id)
                    .with_properties(json!({
                        "taskID": task.task_id,
                        "childSessionID": task.child_session_id,
                        "name": task.name,
                        "by": "parent",
                        "reason": args.reason,
                    })),
            );
        }
        let mut out = ToolOutput::new(
            format!(
                "task_cancel: cancellation requested for '{}' (taskID={}); its result will be \
                 reported as aborted by task_wait/task_status.",
                task.name, task.task_id
            ),
            "task_cancel",
        );
        out.structured = Some(json!({
            "taskID": task.task_id,
            "name": task.name,
            "cancelled": cancelled,
        }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, status: &str, text: &str, collected: bool) -> TaskResult {
        TaskResult {
            task_id: id.into(),
            name: format!("n-{id}"),
            agent: "code".into(),
            child_session_id: "c".into(),
            status: status.into(),
            error: (status != "completed").then(|| "boom".to_string()),
            text: text.into(),
            input_tokens: 10,
            output_tokens: 5,
            started_at: 1_000,
            ended_at: 4_000,
            collected,
        }
    }

    #[test]
    fn verdict_all_vs_any() {
        let wanted = vec!["a".to_string(), "b".to_string()];
        assert_eq!(wait_verdict(&wanted, &[], false), WaitVerdict::Keep);
        assert_eq!(wait_verdict(&wanted, &[], true), WaitVerdict::Keep);
        assert_eq!(
            wait_verdict(&wanted, &["a".into()], false),
            WaitVerdict::Keep
        );
        assert_eq!(
            wait_verdict(&wanted, &["a".into()], true),
            WaitVerdict::Partial
        );
        assert_eq!(
            wait_verdict(&wanted, &["b".into(), "a".into()], false),
            WaitVerdict::Complete
        );
        assert_eq!(
            wait_verdict(&wanted, &["b".into(), "a".into()], true),
            WaitVerdict::Complete
        );
    }

    #[test]
    fn render_results_reports_text_errors_and_pending() {
        let out = render_results(
            &[
                result("1", "completed", "changed a.ts", false),
                result("2", "error", "", false),
            ],
            &["n-3 (3)".into()],
            true,
        );
        assert!(
            out.contains("# n-1 [code] completed · taskID=1 · 3s"),
            "{out}"
        );
        assert!(out.contains("changed a.ts"), "{out}");
        assert!(out.contains("# n-2 [code] error"), "{out}");
        assert!(out.contains("(no report) boom"), "{out}");
        assert!(out.contains("Timed out; still running"), "{out}");
        assert!(out.contains("n-3 (3)"), "{out}");
        assert_eq!(render_results(&[], &[], false), "No results.");
    }

    #[test]
    fn flag_marker_reports_verdict() {
        let looping = TaskProgress {
            verdict: "looping".to_string(),
            ..TaskProgress::default()
        };
        assert_eq!(flag_marker(&looping).as_deref(), Some("⚠ looping"));
        let wandering = TaskProgress {
            verdict: "wandering".to_string(),
            ..TaskProgress::default()
        };
        assert_eq!(flag_marker(&wandering).as_deref(), Some("⚠ wandering"));
        assert_eq!(flag_marker(&TaskProgress::default()), None);
    }

    #[test]
    fn render_status_lists_live_and_finished() {
        let live = vec![LiveTaskView {
            task: ChildTask {
                task_id: "t1".into(),
                description: "d".into(),
                child_session_id: "c1".into(),
                name: "api-tests".into(),
                agent: "code".into(),
                model: None,
                started_at: 0,
                status: "running".into(),
                background: true,
                prompt_hash: None,
            },
            progress: TaskProgress {
                last_tool: Some("write_file".into()),
                last_tool_state: Some("running".into()),
                summary: "Adding the spec".into(),
                tool_calls: 4,
                steps: 2,
                ..TaskProgress::default()
            },
            input_tokens: 100,
            output_tokens: 20,
            elapsed_ms: 12_500,
        }];
        let finished = vec![result("t0", "completed", "done", false)];
        let out = render_status(&live, &finished);
        assert!(out.contains("Live child tasks (1)"), "{out}");
        assert!(
            out.contains(
                "api-tests [code] running · taskID=t1 · 12s · tools=4 last=write_file (running)"
            ),
            "{out}"
        );
        assert!(out.contains("last line: Adding the spec"), "{out}");
        assert!(out.contains("Finished child tasks (1)"), "{out}");
        assert!(out.contains("result NOT collected yet"), "{out}");
        assert_eq!(
            render_status(&[], &[]),
            "No child tasks in this session yet."
        );
    }

    #[test]
    fn wait_args_accept_aliases_and_defaults() {
        let a: WaitArgs = serde_json::from_value(json!({})).unwrap();
        assert!(a.ids.is_empty());
        assert!(a.mode.is_none());
        let a: WaitArgs =
            serde_json::from_value(json!({ "taskIDs": ["x"], "mode": "any", "timeout": 5 }))
                .unwrap();
        assert_eq!(a.ids, vec!["x".to_string()]);
        assert_eq!(a.mode.as_deref(), Some("any"));
        assert_eq!(a.timeout_seconds, Some(5));
        let c: CancelArgs = serde_json::from_value(json!({ "taskID": "abc" })).unwrap();
        assert_eq!(c.id, "abc");
        let c: CancelArgs = serde_json::from_value(json!({ "name": "api-tests" })).unwrap();
        assert_eq!(c.id, "api-tests");
    }

    #[test]
    fn supervision_tools_are_read_only_with_schemas() {
        let s = TaskStatusTool::new(Weak::new());
        let w = TaskWaitTool::new(Weak::new());
        let c = TaskCancelTool::new(Weak::new());
        assert!(s.is_read_only() && w.is_read_only() && c.is_read_only());
        assert_eq!(s.name(), "task_status");
        assert_eq!(w.name(), "task_wait");
        assert_eq!(c.name(), "task_cancel");
        assert!(w.parameters_schema()["properties"]["mode"]["enum"].is_array());
        assert_eq!(c.parameters_schema()["required"], json!(["id"]),);
    }
}
