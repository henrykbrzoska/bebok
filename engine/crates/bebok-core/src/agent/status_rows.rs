//! F9-7: deterministic, token-free status rows for the parent transcript.
//!
//! While sub-agents run, the main thread's chat used to stay blank for
//! minutes: the model emits no text between spawning children and collecting
//! their results, and the only signs of life lived in the Agents panel. The
//! engine now appends [`Part::Status`] rows to the parent's latest assistant
//! message on `task.started`, on progress milestones (first tool call, then
//! every [`PROGRESS_ROW_EVERY`]) and on `task.ended` — plain strings built
//! here, never sent to the model (`agent::request` ignores the variant), so
//! they cost no tokens.

use std::sync::Arc;
use std::time::Duration;

use crate::event::EventBus;
use crate::session::Part;
use crate::store::{ChildTask, SessionState};

use super::delegation::TaskProgress;
use super::observe::emit_part;

/// Minimum gap between two periodic progress rows of one child.
pub const PROGRESS_ROW_EVERY: Duration = Duration::from_secs(60);

/// Append one status row to the parent's latest assistant message and
/// announce it (`message.part.updated`) so the chat renders it live.
pub async fn push_status(
    bus: &EventBus,
    parent: &Arc<SessionState>,
    kind: &str,
    text: String,
    task: Option<&ChildTask>,
) {
    let part = Part::Status {
        kind: kind.to_string(),
        text,
        at: crate::util::now_ms(),
        task_id: task.map(|t| t.task_id.clone()),
        name: task.map(|t| t.name.clone()),
        child_session_id: task.map(|t| t.child_session_id.clone()),
    };
    if let Some(idx) = parent.append_status_part(part).await {
        emit_part(bus, parent, "message.part.updated", idx).await;
    }
}

/// `api-orders started (code · openai/gpt-5.6-luna)` / `… queued (…)`.
pub fn started_text(task: &ChildTask) -> String {
    let verb = if task.status == "queued" {
        "queued"
    } else {
        "started"
    };
    match task.model.as_deref() {
        Some(model) => format!("{} {verb} ({} · {model})", task.name, task.agent),
        None => format!("{} {verb} ({})", task.name, task.agent),
    }
}

/// `api-orders: edit_file · 12 calls · 41k tok` (or `… thinking · …`),
/// with a ` ⚠ looping` / ` ⚠ wandering` suffix when the detector flagged it.
pub fn flagged_text(task: &ChildTask, progress: &TaskProgress, tokens: u64) -> String {
    let base = progress_text(task, progress, tokens);
    match super::supervision_tools::flag_marker(progress) {
        Some(flag) => format!("{base} {flag}"),
        None => base,
    }
}
pub fn progress_text(task: &ChildTask, progress: &TaskProgress, tokens: u64) -> String {
    let tool = progress.last_tool.as_deref().unwrap_or("thinking");
    let calls = match progress.tool_calls {
        1 => "1 call".to_string(),
        n => format!("{n} calls"),
    };
    format!(
        "{}: {tool} · {calls} · {}",
        task.name,
        format_tokens(tokens)
    )
}

/// `api-orders finished in 4m20s · 151k tok · 3 files changed`,
/// `api-orders failed after 12s · 3k tok: <error>`, `… aborted after …`.
pub fn ended_text(
    name: &str,
    status: &str,
    error: Option<&str>,
    elapsed_ms: i64,
    tokens: u64,
    files_changed: usize,
) -> String {
    let elapsed = format_duration(elapsed_ms);
    let tok = format_tokens(tokens);
    match status {
        "completed" => {
            let files = match files_changed {
                0 => "no files changed".to_string(),
                1 => "1 file changed".to_string(),
                n => format!("{n} files changed"),
            };
            format!("{name} finished in {elapsed} · {tok} · {files}")
        }
        "aborted" => format!("{name} aborted after {elapsed} · {tok}"),
        _ => match error.map(str::trim).filter(|e| !e.is_empty()) {
            Some(err) => format!(
                "{name} failed after {elapsed} · {tok}: {}",
                truncate(err, 200)
            ),
            None => format!("{name} failed after {elapsed} · {tok}"),
        },
    }
}

/// `820 tok`, `41k tok`, `151k tok`, `2.1M tok`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}k tok", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k tok", n as f64 / 1000.0)
    } else {
        format!("{n} tok")
    }
}

/// `35s`, `4m20s`, `1h02m`.
pub fn format_duration(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: &str, model: Option<&str>) -> ChildTask {
        ChildTask {
            task_id: "t1".into(),
            description: "do x".into(),
            child_session_id: "c1".into(),
            name: "api-orders".into(),
            agent: "code".into(),
            model: model.map(str::to_string),
            started_at: 0,
            status: status.into(),
            background: true,
            prompt_hash: None,
        }
    }

    #[test]
    fn formats_are_compact_and_deterministic() {
        assert_eq!(format_tokens(820), "820 tok");
        assert_eq!(format_tokens(2_100), "2.1k tok");
        assert_eq!(format_tokens(41_300), "41k tok");
        assert_eq!(format_tokens(151_000), "151k tok");
        assert_eq!(format_tokens(2_140_000), "2.1M tok");
        assert_eq!(format_duration(35_000), "35s");
        assert_eq!(format_duration(260_000), "4m20s");
        assert_eq!(format_duration(3_720_000), "1h02m");
        assert_eq!(format_duration(-5), "0s");
    }

    #[test]
    fn started_and_progress_rows() {
        assert_eq!(
            started_text(&task("running", Some("openai/gpt-5.6-luna"))),
            "api-orders started (code · openai/gpt-5.6-luna)"
        );
        assert_eq!(
            started_text(&task("queued", None)),
            "api-orders queued (code)"
        );
        let progress = TaskProgress {
            last_tool: Some("edit_file".into()),
            last_tool_state: Some("completed".into()),
            summary: String::new(),
            tool_calls: 12,
            steps: 3,
            ..TaskProgress::default()
        };
        assert_eq!(
            progress_text(&task("running", None), &progress, 41_300),
            "api-orders: edit_file · 12 calls · 41k tok"
        );
        let thinking = TaskProgress::default();
        assert_eq!(
            progress_text(&task("running", None), &thinking, 0),
            "api-orders: thinking · 0 calls · 0 tok"
        );
    }

    #[test]
    fn flagged_text_appends_detector_flag() {
        use super::super::delegation::TaskProgress;
        let t = task("running", None);
        let mut p = TaskProgress {
            last_tool: Some("edit_file".into()),
            last_tool_state: Some("completed".into()),
            summary: String::new(),
            tool_calls: 3,
            steps: 2,
            ..TaskProgress::default()
        };
        assert_eq!(flagged_text(&t, &p, 1000), progress_text(&t, &p, 1000));
        p.verdict = "looping".to_string();
        assert_eq!(
            flagged_text(&t, &p, 1000),
            format!("{} ⚠ looping", progress_text(&t, &p, 1000))
        );
        p.verdict = "wandering".to_string();
        assert!(flagged_text(&t, &p, 1000).ends_with("⚠ wandering"));
    }

    #[test]
    fn ended_rows_per_outcome() {
        assert_eq!(
            ended_text("api-orders", "completed", None, 260_000, 151_000, 3),
            "api-orders finished in 4m20s · 151k tok · 3 files changed"
        );
        assert_eq!(
            ended_text("api-orders", "completed", None, 1_000, 10, 0),
            "api-orders finished in 1s · 10 tok · no files changed"
        );
        assert_eq!(
            ended_text("api-orders", "error", Some("boom"), 12_000, 3_000, 0),
            "api-orders failed after 12s · 3.0k tok: boom"
        );
        assert_eq!(
            ended_text("api-orders", "aborted", None, 12_000, 3_000, 1),
            "api-orders aborted after 12s · 3.0k tok"
        );
    }

    #[tokio::test]
    async fn status_part_lands_on_the_last_assistant_message_only() {
        use crate::session::{Message, Role, Session};
        let dir = std::env::temp_dir().join(format!("bebok-status-{}", uuid::Uuid::new_v4()));
        let state = Arc::new(SessionState::new(
            Session::new("/tmp/x", "code"),
            dir.clone(),
            dir.clone(),
            crate::config::ResolvedConfig::default(),
        ));
        // Empty transcript: nothing to attach to.
        assert!(
            state
                .append_status_part(Part::Status {
                    kind: "task.started".into(),
                    text: "x".into(),
                    at: 0,
                    task_id: None,
                    name: None,
                    child_session_id: None,
                })
                .await
                .is_none()
        );
        state.append_user_message("hi").await.unwrap();
        {
            let mut messages = state.messages.write().await;
            let mut m = Message::new(Role::Assistant);
            m.append_text("working");
            messages.push(m);
        }
        state.append_user_message("second prompt").await.unwrap();
        let bus = EventBus::default();
        push_status(
            &bus,
            &state,
            "task.ended",
            "done".into(),
            Some(&task("running", None)),
        )
        .await;
        let messages = state.messages_snapshot().await;
        assert_eq!(messages.len(), 3, "no message is created for a status row");
        assert!(matches!(
            messages[1].parts.last(),
            Some(Part::Status { kind, text, name, .. })
                if kind == "task.ended" && text == "done" && name.as_deref() == Some("api-orders")
        ));
        // Status rows are not assistant text.
        assert_eq!(messages[1].text_content(), "working");
        let _ = std::fs::remove_dir_all(dir);
    }
}
