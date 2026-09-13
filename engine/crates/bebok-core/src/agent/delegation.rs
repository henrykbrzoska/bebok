//! WP-DELEGATION (F8-2): shared sub-agent runner + supervision plumbing.
//!
//! Before this module the `task` and `fleet` tools each carried their own
//! copy of "create child session -> register task -> run turn -> persist
//! outcome -> `task.ended`". Both now build a [`ChildSpec`] and call
//! [`run_child`], which adds three things on top of the old sequence:
//!
//! * **Concurrency cap** - a per-session [`SlotGate`] sized by
//!   `delegation.max_concurrent`. A child registers (and is announced with
//!   `task.started`) immediately, but its turn loop only starts once a slot is
//!   free; until then it is `queued` (visible as such in the Agents panel).
//! * **Live progress** - a bus subscriber per child turns the child's own
//!   `message.*` events into `task.progress` events on the *parent* session,
//!   throttled to at most one per second per child ([`ProgressThrottle`]).
//!   Each carries the last tool called, a one-line summary and token totals,
//!   so the Agents panel, the transcript viewer and the parent's tool row can
//!   show what a child is doing without opening its transcript.
//! * **Result capture** - background children (`task` with
//!   `background: true`) push a [`TaskResult`] onto the parent so `task_wait`
//!   / `task_status` can hand it to the model later.
//!
//! Abort propagation is unchanged: every child token is
//! `ctx.abort.child_token()`, so cancelling the parent turn cancels every
//! running or queued child; `task_cancel` cancels one child token only.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::turn::run_turn;
use crate::event::{Event, EventBus};
use crate::session::{Message, Part, Role, ToolState};
use crate::store::{ChildTask, InstanceStore, SessionState, TaskResult};

// ---------------------------------------------------------------------------
// Concurrency gate
// ---------------------------------------------------------------------------

/// Counting gate with a *dynamic* limit: the cap is passed on every acquire,
/// so a `delegation.max_concurrent` change applies to the next child without
/// rebuilding anything. Unlike a `tokio::sync::Semaphore` it cannot be
/// over-subscribed by lowering the limit while permits are out.
pub struct SlotGate {
    running: std::sync::Mutex<usize>,
    freed: Notify,
}

impl Default for SlotGate {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotGate {
    pub fn new() -> Self {
        Self {
            running: std::sync::Mutex::new(0),
            freed: Notify::new(),
        }
    }

    /// Number of slots currently held.
    pub fn running(&self) -> usize {
        *self.running.lock().unwrap()
    }

    /// Non-blocking attempt to take a slot under `max`.
    pub fn try_acquire(&self, max: usize) -> bool {
        let mut n = self.running.lock().unwrap();
        if *n < max.max(1) {
            *n += 1;
            true
        } else {
            false
        }
    }

    /// Wait until a slot under `max` is free, or `abort` fires (returns
    /// `false` in that case, holding nothing).
    pub async fn acquire(&self, max: usize, abort: &CancellationToken) -> bool {
        loop {
            // Arm the wake-up *before* checking, so a release that happens
            // between the check and the await is not lost.
            let notified = self.freed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.try_acquire(max) {
                return true;
            }
            tokio::select! {
                _ = abort.cancelled() => return false,
                _ = &mut notified => {}
            }
        }
    }

    /// Give a slot back and wake one waiter.
    pub fn release(&self) {
        let mut n = self.running.lock().unwrap();
        *n = n.saturating_sub(1);
        drop(n);
        self.freed.notify_one();
    }
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// Minimum gap between two `task.progress` events of one child.
pub const PROGRESS_MIN_GAP: Duration = Duration::from_secs(1);

/// What to do with a progress trigger (pure; testable without a runtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleAction {
    /// Enough time has passed: emit right away.
    EmitNow,
    /// Too soon: emit at the given instant (a trailing edge). Only returned
    /// once per burst - later triggers in the same window get `Pending`.
    Defer(std::time::Instant),
    /// A trailing emit is already scheduled; nothing to do.
    Pending,
}

/// Leading + trailing throttle: the first trigger after a quiet period emits
/// immediately, further triggers inside `min_gap` collapse into one trailing
/// emit at `last + min_gap`. Nothing is ever dropped silently - the trailing
/// emit always reflects the newest state because the emitter re-reads the
/// transcript when it fires.
#[derive(Debug)]
pub struct ProgressThrottle {
    min_gap: Duration,
    last_emit: Option<std::time::Instant>,
    pending: bool,
}

impl ProgressThrottle {
    pub fn new(min_gap: Duration) -> Self {
        Self {
            min_gap,
            last_emit: None,
            pending: false,
        }
    }

    /// Register a trigger at `now`.
    pub fn mark(&mut self, now: std::time::Instant) -> ThrottleAction {
        if self.pending {
            return ThrottleAction::Pending;
        }
        match self.last_emit {
            Some(last) if now.duration_since(last) < self.min_gap => {
                self.pending = true;
                ThrottleAction::Defer(last + self.min_gap)
            }
            _ => {
                self.last_emit = Some(now);
                ThrottleAction::EmitNow
            }
        }
    }

    /// The deferred emit fired at `now`.
    pub fn emitted(&mut self, now: std::time::Instant) {
        self.last_emit = Some(now);
        self.pending = false;
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }
}

/// One-line view of what a child is doing right now (also the payload of
/// `task.progress` and of `AgentEntry.progress`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TaskProgress {
    /// Name of the most recent tool call in the child's transcript.
    #[serde(rename = "lastTool", skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
    /// `pending` | `running` | `completed` | `error` of that call.
    #[serde(rename = "lastToolState", skip_serializing_if = "Option::is_none")]
    pub last_tool_state: Option<String>,
    /// Last non-empty line of the child's latest assistant text (<= 160 chars).
    pub summary: String,
    /// Total tool calls so far.
    #[serde(rename = "toolCalls")]
    pub tool_calls: usize,
    /// Assistant messages so far (LLM round-trips).
    pub steps: usize,
}

/// Maximum length of `TaskProgress::summary`.
pub const SUMMARY_MAX_CHARS: usize = 160;

/// Derive a [`TaskProgress`] from a child transcript.
pub fn summarize_progress(messages: &[Message]) -> TaskProgress {
    let mut progress = TaskProgress::default();
    let mut last_tool: Option<(String, String)> = None;
    let mut last_text = String::new();
    for m in messages.iter().filter(|m| m.role == Role::Assistant) {
        progress.steps += 1;
        for part in &m.parts {
            if let Part::Tool { name, state, .. } = part {
                progress.tool_calls += 1;
                let kind = match state {
                    ToolState::Pending { .. } => "pending",
                    ToolState::Running { .. } => "running",
                    ToolState::Completed { .. } => "completed",
                    ToolState::Error { .. } => "error",
                };
                last_tool = Some((name.clone(), kind.to_string()));
            }
        }
        let text = m.text_content();
        if let Some(line) = text.lines().map(str::trim).rev().find(|l| !l.is_empty()) {
            last_text = line.to_string();
        }
    }
    if let Some((name, state)) = last_tool {
        progress.last_tool = Some(name);
        progress.last_tool_state = Some(state);
    }
    progress.summary = truncate_chars(&last_text, SUMMARY_MAX_CHARS);
    progress
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Progress event payload for one child (`task.progress` on the parent).
async fn progress_properties(child: &SessionState, task: &ChildTask) -> Value {
    let messages = child.messages_snapshot().await;
    let meta = child.meta_snapshot().await;
    let progress = summarize_progress(&messages);
    json!({
        "taskID": task.task_id,
        "childSessionID": task.child_session_id,
        "name": task.name,
        "agent": task.agent,
        "status": task.status,
        "progress": progress,
        "tokens": {
            "input": meta.usage.input_tokens,
            "output": meta.usage.output_tokens,
        },
        "at": crate::util::now_ms(),
    })
}

/// Publish one `task.progress` for `task` on the parent session.
pub async fn emit_progress(
    bus: &EventBus,
    child: &SessionState,
    parent_session_id: &str,
    task: &ChildTask,
) {
    let props = progress_properties(child, task).await;
    bus.publish(
        Event::new("task.progress", child.directory(), parent_session_id).with_properties(props),
    );
}

/// F9-7: decides when a progress *row* (a transcript line, not an SSE
/// event) is due: on the first tool call, then every
/// [`status_rows::PROGRESS_ROW_EVERY`]. Pure, so it is unit-testable.
#[derive(Debug)]
pub struct MilestoneTracker {
    first_tool_seen: bool,
    last_row: Option<std::time::Instant>,
    every: Duration,
}

impl MilestoneTracker {
    pub fn new(every: Duration) -> Self {
        Self {
            first_tool_seen: false,
            last_row: None,
            every,
        }
    }

    /// Whether a row should be written for the given progress at `now`.
    pub fn due(&mut self, progress: &TaskProgress, now: std::time::Instant) -> bool {
        if !self.first_tool_seen && progress.tool_calls > 0 {
            self.first_tool_seen = true;
            self.last_row = Some(now);
            return true;
        }
        match self.last_row {
            Some(last) if now.duration_since(last) >= self.every => {
                self.last_row = Some(now);
                true
            }
            Some(_) => false,
            // Nothing happened yet: no row, no clock.
            None if progress.tool_calls == 0 && progress.steps == 0 => false,
            // The child answered without a tool: start the clock silently.
            None => {
                self.last_row = Some(now);
                false
            }
        }
    }
}

/// Follow `child`'s own bus events and re-publish them as throttled
/// `task.progress` events on the parent until `stop` fires. Also writes
/// the F9-7 progress rows into the parent transcript at milestones.
pub fn spawn_progress_reporter(
    bus: EventBus,
    child: Arc<SessionState>,
    parent: Arc<SessionState>,
    task: ChildTask,
    stop: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = bus.subscribe();
        let child_id = child.id().to_string();
        let parent_session_id = parent.id().to_string();
        let mut throttle = ProgressThrottle::new(PROGRESS_MIN_GAP);
        let mut deadline: Option<tokio::time::Instant> = None;
        let mut milestones = MilestoneTracker::new(super::status_rows::PROGRESS_ROW_EVERY);
        // Periodic tick so the "every ~60 s" row fires even when the child
        // is quiet (a long build, a slow provider call).
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let sleep = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tick.tick() => {
                    maybe_progress_row(&bus, &child, &parent, &task, &mut milestones).await;
                }
                _ = sleep => {
                    deadline = None;
                    throttle.emitted(std::time::Instant::now());
                    emit_progress(&bus, &child, &parent_session_id, &task).await;
                    maybe_progress_row(&bus, &child, &parent, &task, &mut milestones).await;
                }
                recv = rx.recv() => {
                    let relevant = match recv {
                        Ok(ev) => {
                            ev.session_id == child_id
                                && (ev.kind == "message.part.updated" || ev.kind == "message.updated")
                        }
                        // Fell behind the bus: treat it as "something changed".
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if !relevant {
                        continue;
                    }
                    match throttle.mark(std::time::Instant::now()) {
                        ThrottleAction::EmitNow => {
                            emit_progress(&bus, &child, &parent_session_id, &task).await;
                            maybe_progress_row(&bus, &child, &parent, &task, &mut milestones).await;
                        }
                        ThrottleAction::Defer(at) => {
                            deadline = Some(tokio::time::Instant::from_std(at));
                        }
                        ThrottleAction::Pending => {}
                    }
                }
            }
        }
    })
}

/// Write a `task.progress` row into the parent transcript when a milestone
/// is due (F9-7).
async fn maybe_progress_row(
    bus: &EventBus,
    child: &Arc<SessionState>,
    parent: &Arc<SessionState>,
    task: &ChildTask,
    milestones: &mut MilestoneTracker,
) {
    let progress = summarize_progress(&child.messages_snapshot().await);
    if !milestones.due(&progress, std::time::Instant::now()) {
        return;
    }
    let usage = child.meta_snapshot().await.usage;
    let text = super::status_rows::progress_text(
        task,
        &progress,
        usage.input_tokens + usage.output_tokens,
    );
    super::status_rows::push_status(bus, parent, "task.progress", text, Some(task)).await;
}

// ---------------------------------------------------------------------------
// Depth
// ---------------------------------------------------------------------------

/// Nesting depth of `session_id` in the delegation tree (0 = a user session,
/// 1 = its direct sub-agent, ...). Walks the persisted `parent` links, so it
/// counts *nesting*, not how many siblings are running at the same time.
pub async fn delegation_depth(store: &InstanceStore, session_id: uuid::Uuid) -> usize {
    let mut depth = 0;
    let mut current = session_id;
    // Bounded walk: a corrupt cycle must not hang the tool.
    for _ in 0..16 {
        let Ok(meta) = store.session_meta(current).await else {
            break;
        };
        match meta.parent {
            Some((parent, _)) => {
                depth += 1;
                current = parent;
            }
            None => break,
        }
    }
    depth
}

// ---------------------------------------------------------------------------
// Child runner
// ---------------------------------------------------------------------------

/// Everything needed to run one child to completion.
pub struct ChildSpec {
    pub store: Arc<InstanceStore>,
    pub instance: Arc<crate::store::Instance>,
    pub parent: Arc<SessionState>,
    /// Resolved preset, prompt already assembled.
    pub agent: super::preset::Agent,
    /// Preset name as the model asked for it (`code`, `ask`, ...).
    pub agent_name: String,
    pub model: String,
    pub provider: Arc<dyn bebok_llm::Provider>,
    /// Unique child name (already allocated on the parent).
    pub name: String,
    pub prompt: String,
    pub images: Vec<Part>,
    /// Parent turn's abort token; the child gets a child token of it.
    pub parent_abort: CancellationToken,
    pub parent_session_id: String,
    pub directory: String,
    pub max_concurrent: usize,
    pub background: bool,
    /// `task` | `fleet` - only used in messages.
    pub origin: &'static str,
}

/// What happened to a child (also what `task_wait` hands back).
pub struct ChildOutcome {
    pub task_id: String,
    pub name: String,
    pub child_session_id: String,
    /// `completed` | `error` | `aborted`.
    pub status: String,
    pub error: Option<String>,
    /// Final assistant text (empty unless `completed`).
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub started_at: i64,
    pub ended_at: i64,
}

impl ChildOutcome {
    pub fn ok(&self) -> bool {
        self.status == "completed" && !self.text.trim().is_empty()
    }

    pub fn to_result(&self, agent: &str) -> TaskResult {
        TaskResult {
            task_id: self.task_id.clone(),
            name: self.name.clone(),
            agent: agent.to_string(),
            child_session_id: self.child_session_id.clone(),
            status: self.status.clone(),
            error: self.error.clone(),
            text: self.text.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            started_at: self.started_at,
            ended_at: self.ended_at,
            collected: false,
        }
    }
}

/// Prepared child: session created, task registered and announced. Split
/// from [`run_child`] so a background `task` can return the task id to the
/// model *before* the turn runs.
pub struct PreparedChild {
    pub spec: ChildSpec,
    pub child: Arc<SessionState>,
    pub info: ChildTask,
    pub abort: CancellationToken,
}

/// Create the child session, record the brief, claim its turn slot and
/// register + announce it on the parent (`task.started`, status `queued` or
/// `running` depending on whether a concurrency slot is free right now).
pub async fn prepare_child(spec: ChildSpec) -> Result<PreparedChild, String> {
    let child = spec
        .store
        .create_subagent_session(
            &spec.parent,
            &spec.agent.name,
            Some(&spec.model),
            Some(&spec.name),
        )
        .await
        .map_err(|e| format!("cannot create sub-session: {e}"))?;

    child
        .append_user_message_with_images(&spec.prompt, spec.images.clone())
        .await
        .map_err(|e| format!("cannot record subtask: {e}"))?;

    if !child.try_begin_turn() {
        return Err("sub-session is already busy".to_string());
    }
    let abort = spec.parent_abort.child_token();
    child.set_abort(abort.clone()).await;

    let task_id = uuid::Uuid::new_v4().to_string();
    let description: String = spec.prompt.chars().take(80).collect();
    let child_session_id = child.id().to_string();
    let slot_now = spec.parent.child_slots().try_acquire(spec.max_concurrent);
    let status = if slot_now { "running" } else { "queued" };

    let info = spec
        .parent
        .register_child_task_full(
            &task_id,
            &description,
            &child_session_id,
            &spec.name,
            &spec.agent_name,
            Some(&spec.model),
            status,
            spec.background,
            abort.clone(),
        )
        .await;

    spec.store.bus().publish(
        Event::new("task.started", &spec.directory, &spec.parent_session_id)
            .with_properties(serde_json::to_value(&info).unwrap_or_default()),
    );
    // F9-7: a visible line in the parent's chat, right away.
    super::status_rows::push_status(
        &spec.store.bus(),
        &spec.parent,
        "task.started",
        super::status_rows::started_text(&info),
        Some(&info),
    )
    .await;

    // `prepare_child` took the slot when one was free; `run_child` takes it
    // otherwise. Encode that in the status so `run_child` knows.
    Ok(PreparedChild {
        spec,
        child,
        info,
        abort,
    })
}

/// Run a prepared child to completion: wait for a slot if queued, stream
/// progress, run the turn, persist the outcome, emit `task.ended`, and record
/// the result on the parent when it was a background task.
pub async fn run_child(prepared: PreparedChild) -> ChildOutcome {
    let PreparedChild {
        spec,
        child,
        mut info,
        abort,
    } = prepared;
    let bus = spec.store.bus();
    let started_at = info.started_at;
    let task_id = info.task_id.clone();
    let child_session_id = info.child_session_id.clone();
    let name = info.name.clone();

    // Concurrency cap: a queued child waits here (abortable).
    let mut holds_slot = info.status == "running";
    if !holds_slot {
        holds_slot = spec
            .parent
            .child_slots()
            .acquire(spec.max_concurrent, &abort)
            .await;
        if holds_slot {
            if let Some(updated) = spec.parent.set_child_task_status(&task_id, "running").await {
                info = updated;
            }
            emit_progress(&bus, &child, &spec.parent_session_id, &info).await;
        }
    }

    let result = if abort.is_cancelled() {
        Ok(())
    } else {
        let stop = CancellationToken::new();
        let reporter = spawn_progress_reporter(
            bus.clone(),
            child.clone(),
            spec.parent.clone(),
            info.clone(),
            stop.clone(),
        );
        let result = run_turn(
            child.clone(),
            spec.agent,
            spec.instance.tools.clone(),
            spec.provider,
            spec.instance.permission.clone(),
            bus.clone(),
            abort.clone(),
            &spec.model,
        )
        .await;
        stop.cancel();
        let _ = reporter.await;
        result
    };

    if holds_slot {
        spec.parent.child_slots().release();
    }
    child.clear_abort().await;
    child.end_turn();

    let (status, error_msg) = match &result {
        Ok(()) => {
            if abort.is_cancelled() {
                (
                    "aborted".to_string(),
                    Some("sub-task was cancelled".to_string()),
                )
            } else {
                ("completed".to_string(), None)
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if abort.is_cancelled() {
                (
                    "aborted".to_string(),
                    Some(format!("sub-task was cancelled: {msg}")),
                )
            } else {
                ("error".to_string(), Some(msg))
            }
        }
    };

    // Persist the outcome on the child (F6-12: `GET /session/{id}/agents`
    // reads it back once the task is no longer in the live map).
    child.set_task_status(&status, error_msg.as_deref()).await;

    let text = if status == "completed" {
        let messages = child.messages_snapshot().await;
        messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.text_content())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let usage = child.meta_snapshot().await.usage;
    let outcome = ChildOutcome {
        task_id: task_id.clone(),
        name: name.clone(),
        child_session_id: child_session_id.clone(),
        status: status.clone(),
        error: error_msg.clone(),
        text,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        started_at,
        ended_at: crate::util::now_ms(),
    };

    // Background results are collected later via `task_wait`; blocking
    // callers get the outcome back directly, but the parent keeps a copy so
    // `task_status` can list it too.
    let mut record = outcome.to_result(&spec.agent_name);
    record.collected = !spec.background;
    spec.parent.push_task_result(record).await;

    spec.parent.unregister_child_task(&task_id).await;
    // F9-7: the closing line ("finished in 4m20s · 151k tok · 3 files changed").
    let files_changed =
        crate::change_tracking::Tracker::new(child.directory(), child.disk_dir()).tracked_count();
    super::status_rows::push_status(
        &bus,
        &spec.parent,
        "task.ended",
        super::status_rows::ended_text(
            &name,
            &status,
            error_msg.as_deref(),
            outcome.ended_at - started_at,
            usage.input_tokens + usage.output_tokens,
            files_changed,
        ),
        Some(&info),
    )
    .await;
    bus.publish(
        Event::new("task.ended", &spec.directory, &spec.parent_session_id).with_properties(json!({
            "taskID": task_id,
            "status": status,
            "error": error_msg,
            "childSessionID": child_session_id,
            "name": name,
            "background": spec.background,
            "tokens": { "input": usage.input_tokens, "output": usage.output_tokens },
            "origin": spec.origin,
        })),
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn throttle_leading_then_trailing_edge() {
        let t0 = Instant::now();
        let mut t = ProgressThrottle::new(Duration::from_secs(1));
        assert_eq!(t.mark(t0), ThrottleAction::EmitNow);
        // Burst inside the window: one deferred emit, later ones pending.
        let d = match t.mark(t0 + Duration::from_millis(200)) {
            ThrottleAction::Defer(at) => at,
            other => panic!("expected Defer, got {other:?}"),
        };
        assert_eq!(d, t0 + Duration::from_secs(1));
        assert_eq!(
            t.mark(t0 + Duration::from_millis(400)),
            ThrottleAction::Pending
        );
        assert!(t.is_pending());
        // The trailing emit fires: window restarts from there.
        t.emitted(d);
        assert!(!t.is_pending());
        assert!(matches!(
            t.mark(d + Duration::from_millis(100)),
            ThrottleAction::Defer(_)
        ));
        // A quiet period -> immediate again.
        let mut t = ProgressThrottle::new(Duration::from_secs(1));
        assert_eq!(t.mark(t0), ThrottleAction::EmitNow);
        assert_eq!(t.mark(t0 + Duration::from_secs(3)), ThrottleAction::EmitNow);
    }

    /// F9-7: a progress row on the first tool call, then one per interval,
    /// never for a child that has done nothing yet.
    #[test]
    fn milestones_first_tool_then_every_interval() {
        let t0 = Instant::now();
        let mut m = MilestoneTracker::new(Duration::from_secs(60));
        let idle = TaskProgress::default();
        assert!(!m.due(&idle, t0));
        assert!(!m.due(&idle, t0 + Duration::from_secs(120)));
        let one = TaskProgress {
            tool_calls: 1,
            ..TaskProgress::default()
        };
        assert!(m.due(&one, t0 + Duration::from_secs(1)), "first tool");
        assert!(!m.due(&one, t0 + Duration::from_secs(30)));
        assert!(m.due(&one, t0 + Duration::from_secs(61)), "interval");
        assert!(!m.due(&one, t0 + Duration::from_secs(90)));
        assert!(m.due(&one, t0 + Duration::from_secs(122)));
        // A child that only talks (no tool) starts the clock silently.
        let mut m = MilestoneTracker::new(Duration::from_secs(60));
        let talk = TaskProgress {
            steps: 1,
            ..TaskProgress::default()
        };
        assert!(!m.due(&talk, t0));
        assert!(!m.due(&talk, t0 + Duration::from_secs(30)));
        assert!(m.due(&talk, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn throttle_never_exceeds_one_per_second() {
        // 50 triggers spread over 2.5 s at 50 ms -> at most 3 emits
        // (t=0 leading, t=1 trailing, t=2 trailing) and never two closer
        // than the gap.
        let t0 = Instant::now();
        let mut t = ProgressThrottle::new(Duration::from_secs(1));
        let mut emits: Vec<Instant> = Vec::new();
        let mut deferred: Option<Instant> = None;
        for i in 0..50u64 {
            let now = t0 + Duration::from_millis(i * 50);
            if let Some(at) = deferred
                && now >= at
            {
                emits.push(at);
                t.emitted(at);
                deferred = None;
            }
            match t.mark(now) {
                ThrottleAction::EmitNow => emits.push(now),
                ThrottleAction::Defer(at) => deferred = Some(at),
                ThrottleAction::Pending => {}
            }
        }
        assert!(emits.len() <= 3, "{} emits", emits.len());
        for w in emits.windows(2) {
            assert!(w[1].duration_since(w[0]) >= Duration::from_secs(1));
        }
    }

    #[test]
    fn summarize_reports_last_tool_and_last_line() {
        let mut m1 = Message::assistant_with("code", "m");
        m1.append_text("Looking at the controller.\n\nFirst line\nlast line of first turn ");
        m1.add_tool_call("c1".into(), "read_file".into(), json!({"path": "a.ts"}));
        assert!(m1.mark_tool_completed("c1", "ok".into(), "read_file".into(), None));
        let mut m2 = Message::assistant_with("code", "m");
        m2.add_tool_call("c2".into(), "write_file".into(), json!({"path": "b.ts"}));
        assert!(m2.mark_tool_running("c2", 1));
        let user = Message::user("brief");
        let p = summarize_progress(&[user, m1, m2]);
        assert_eq!(p.last_tool.as_deref(), Some("write_file"));
        assert_eq!(p.last_tool_state.as_deref(), Some("running"));
        assert_eq!(p.summary, "last line of first turn");
        assert_eq!(p.tool_calls, 2);
        assert_eq!(p.steps, 2);
    }

    #[test]
    fn summarize_truncates_long_lines_and_handles_empty() {
        let p = summarize_progress(&[]);
        assert_eq!(p, TaskProgress::default());
        let mut m = Message::assistant_with("code", "m");
        m.append_text(&"x".repeat(400));
        let p = summarize_progress(&[m]);
        assert_eq!(p.summary.chars().count(), SUMMARY_MAX_CHARS);
        assert!(p.summary.ends_with('\u{2026}'));
        assert!(p.last_tool.is_none());
    }

    #[tokio::test]
    async fn slot_gate_caps_concurrency_and_queues_extra() {
        let gate = Arc::new(SlotGate::new());
        let abort = CancellationToken::new();
        // Two slots taken immediately, the third has to wait.
        assert!(gate.acquire(2, &abort).await);
        assert!(gate.acquire(2, &abort).await);
        assert!(!gate.try_acquire(2));
        assert_eq!(gate.running(), 2);

        let waiter = {
            let gate = gate.clone();
            let abort = abort.clone();
            tokio::spawn(async move { gate.acquire(2, &abort).await })
        };
        // Not done until a slot is released.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        gate.release();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("waiter woke")
                .expect("join")
        );
        assert_eq!(gate.running(), 2);

        // Raising the limit admits more without releasing anything.
        assert!(gate.try_acquire(3));
        assert_eq!(gate.running(), 3);
        gate.release();
        gate.release();
        gate.release();
        assert_eq!(gate.running(), 0);
    }

    #[tokio::test]
    async fn slot_gate_acquire_gives_up_on_abort() {
        let gate = SlotGate::new();
        let abort = CancellationToken::new();
        assert!(gate.acquire(1, &abort).await);
        let cancel = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        });
        assert!(!gate.acquire(1, &abort).await);
        assert_eq!(gate.running(), 1, "an aborted acquire holds no slot");
    }

    #[tokio::test]
    async fn slot_gate_release_before_wait_is_not_lost() {
        // Regression guard for the classic lost-wakeup: release happens
        // between try_acquire failing and the await.
        let gate = Arc::new(SlotGate::new());
        let abort = CancellationToken::new();
        assert!(gate.try_acquire(1));
        let g2 = gate.clone();
        let a2 = abort.clone();
        let waiter = tokio::spawn(async move { g2.acquire(1, &a2).await });
        gate.release();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("no lost wakeup")
                .unwrap()
        );
    }
}
