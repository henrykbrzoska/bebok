//! `/schedules` routes: CRUD + run-now for scheduled tasks.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::Mutex;

use bebok_core::scheduler::{self, ScheduledTask};

use crate::error::ApiError;
use crate::routes::session::PromptBody;
use crate::services::turn::prompt_turn;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    pub directory: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    pub kind: String,
    pub interval_mins: Option<u64>,
    pub hour: Option<u8>,
    pub minute: Option<u8>,
    pub weekday: Option<u8>,
    pub prompt: String,
    pub directory: String,
    pub agent: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct PatchBody {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub prompt: Option<String>,
}

/// `GET /schedules?directory=...`
pub async fn list_schedules(Query(q): Query<ListQuery>) -> impl IntoResponse {
    let mut tasks = scheduler::load_all();
    if let Some(dir) = &q.directory {
        tasks.retain(|t| t.directory == *dir);
    }
    Json(tasks)
}

/// `POST /schedules`
pub async fn create_schedule(
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<ScheduledTask>), ApiError> {
    let now = Utc::now();
    // Build a temporary task just for compute_next (needs the time fields).
    let temp = ScheduledTask {
        id: String::new(),
        name: String::new(),
        kind: body.kind.clone(),
        interval_mins: None,
        hour: body.hour,
        minute: body.minute,
        weekday: body.weekday,
        prompt: String::new(),
        directory: String::new(),
        agent: None,
        enabled: true,
        last_run_at: None,
        last_status: String::new(),
        next_run_at: String::new(),
    };
    let task = ScheduledTask {
        id: uuid::Uuid::new_v4().to_string(),
        name: body.name,
        kind: body.kind,
        interval_mins: body.interval_mins,
        hour: body.hour,
        minute: body.minute,
        weekday: body.weekday,
        prompt: body.prompt,
        directory: body.directory,
        agent: body.agent,
        enabled: body.enabled,
        last_run_at: None,
        last_status: "pending".into(),
        next_run_at: scheduler::compute_next(&temp, now).to_rfc3339(),
    };
    scheduler::save(&task).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(task)))
}

/// `PATCH /schedules/:id`
pub async fn patch_schedule(
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<ScheduledTask>, ApiError> {
    let tasks = scheduler::load_all();
    let mut task = tasks
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::not_found(format!("schedule {id} not found")))?;

    if let Some(name) = body.name {
        task.name = name;
    }
    if let Some(enabled) = body.enabled {
        task.enabled = enabled;
    }
    if let Some(prompt) = body.prompt {
        task.prompt = prompt;
    }
    scheduler::save(&task).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(task))
}

/// `DELETE /schedules/:id`
pub async fn delete_schedule(Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    scheduler::delete(&id).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /schedules/:id/run` — force immediate execution by setting next_run to now.
pub async fn run_schedule(Path(id): Path<String>) -> Result<Json<ScheduledTask>, ApiError> {
    let tasks = scheduler::load_all();
    let mut task = tasks
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::not_found(format!("schedule {id} not found")))?;
    task.next_run_at = Utc::now().to_rfc3339();
    scheduler::save(&task).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(task))
}

// ---------------------------------------------------------------------------
// Background tick loop
// ---------------------------------------------------------------------------

/// Spawn the scheduler loop. Call once from `server.rs`.
///
/// Every 30 s loads all persisted tasks, picks the enabled ones whose
/// `next_run_at ≤ now`, and for each (skipping those already in-flight)
/// spawns a background turn: `create_session` → `prompt_turn`. On
/// completion the task status is written back to disk as `done` or
/// `failed`, `next_run_at` is advanced, and a `schedule.ran` SSE event
/// is published so the frontend can react.
pub fn spawn_scheduler(state: AppState) {
    // Task IDs currently being executed — prevents double-fire when the
    // same task is still running across a tick boundary.
    let running: Arc<Mutex<HashMap<String, ()>>> = Arc::new(Mutex::new(HashMap::new()));

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            let tasks = scheduler::load_all();
            let now = Utc::now();
            let mut guard = running.lock().await;

            for task in &tasks {
                if !task.enabled {
                    continue;
                }
                let next = match chrono::DateTime::parse_from_rfc3339(&task.next_run_at) {
                    Ok(dt) => dt.with_timezone(&Utc),
                    Err(_) => continue,
                };
                if next > now {
                    continue;
                }
                if guard.contains_key(&task.id) {
                    continue;
                }

                // Mark as running in both the in-flight set and on disk.
                guard.insert(task.id.clone(), ());
                let mut updated = task.clone();
                updated.last_run_at = Some(now.to_rfc3339());
                updated.last_status = "running".to_string();
                updated.next_run_at = scheduler::compute_next(&updated, now).to_rfc3339();
                let _ = scheduler::save(&updated);

                let bus = state.store.bus();
                bus.publish(
                    bebok_core::event::Event::new("schedule.ran", &updated.directory, "")
                        .with_properties(serde_json::json!({
                            "id": updated.id,
                            "name": updated.name,
                            "status": "running",
                        })),
                );

                let task_state = state.clone();
                let running_map = running.clone();
                let task_id = task.id.clone();

                tokio::spawn(async move {
                    let result = execute_task(&task_state, &updated).await;

                    let mut status = "done".to_string();
                    if let Err(e) = &result {
                        let msg = match e {
                            crate::error::ApiError::BadRequest(m)
                            | crate::error::ApiError::NotFound(m)
                            | crate::error::ApiError::Conflict(m)
                            | crate::error::ApiError::Forbidden(m)
                            | crate::error::ApiError::BadGateway(m)
                            | crate::error::ApiError::ServiceUnavailable(m)
                            | crate::error::ApiError::Internal(m) => m.clone(),
                        };
                        tracing::error!(
                            schedule = %updated.name,
                            id = %updated.id,
                            "scheduled turn failed: {msg}"
                        );
                        status = "failed".to_string();
                    }

                    // Persist final status (last_status + next_run_at already set above).
                    let mut final_task = updated.clone();
                    final_task.last_status = status.clone();
                    let _ = scheduler::save(&final_task);

                    let bus = task_state.store.bus();
                    bus.publish(
                        bebok_core::event::Event::new("schedule.ran", &final_task.directory, "")
                            .with_properties(serde_json::json!({
                                "id": final_task.id,
                                "name": final_task.name,
                                "status": status,
                            })),
                    );

                    // Release the in-flight slot.
                    running_map.lock().await.remove(&task_id);
                });
            }
        }
    });
}

/// Create a session and issue the prompt — the simplest possible execution
/// path for a scheduled task.
async fn execute_task(state: &AppState, task: &ScheduledTask) -> Result<(), ApiError> {
    let agent = task.agent.as_deref().unwrap_or("code");

    let session = state
        .store
        .create_session(&task.directory, agent, None)
        .await
        .map_err(|e| ApiError::internal(format!("create_session failed: {e}")))?;

    let body = PromptBody {
        message: task.prompt.clone(),
        agent: Some(agent.to_string()),
        model: None,
        images: vec![],
    };

    let _ = prompt_turn(state, session.id(), body).await?;
    Ok(())
}
