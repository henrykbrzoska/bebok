//! OpenRouter Batch API (option B): `POST /session/{id}/batch`,
//! `GET /session/{id}/batches`, `GET /batch/{id}`, `POST /batch/{id}/cancel`.
//!
//! A batch is an asynchronous bulk job that runs *alongside* the interactive
//! session turn — it never takes the session turn lock, so `prompt` keeps
//! answering 409/200 exactly as before. Progress surfaces on the engine
//! [`EventBus`] as `batch.submitted` / `batch.progress` / `batch.completed` /
//! `batch.failed` (see `bebok_core::event::Event` constructors).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use bebok_core::batch::{BatchItem, BatchItemStatus, BatchState, BatchStore};
use bebok_core::event::Event;
use bebok_llm::{LlmError, ProviderSpec};

use crate::error::ApiError;
use crate::state::AppState;

/// Poll interval while a provider-side job is not yet terminal.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// `?directory=` — every batch endpoint is scoped to an instance directory.
#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub directory: Option<String>,
}

fn require_directory(q: &DirectoryQuery) -> Result<String, axum::response::Response> {
    q.directory
        .clone()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("missing ?directory=").into_response())
}

/// One item of `POST /session/{id}/batch`: ready chat messages for one
/// completion inside the batch.
#[derive(Debug, Deserialize)]
pub struct BatchItemRequest {
    pub messages: Vec<serde_json::Value>,
}

/// Body of `POST /session/{id}/batch`.
#[derive(Debug, Deserialize)]
pub struct SubmitBatchRequest {
    pub directory: Option<String>,
    pub model: String,
    pub items: Vec<BatchItemRequest>,
}

/// `POST /session/{id}/batch` -> `{ batchID }`.
///
/// Validates the model carries the `:batch` suffix, stores the record, spawns
/// the background driver (submit inline requests -> poll -> inline results)
/// and returns immediately. The interactive turn lock is never touched.
pub async fn submit_batch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SubmitBatchRequest>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let directory = body
        .directory
        .clone()
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("missing directory").into_response())?;
    // Shared submission path (also used by the `:batch` chat fallback in
    // `services::turn`): validate, store, emit `batch.submitted`, spawn.
    let batch_id =
        crate::services::batch::submit_batch_job(&state, &directory, id, &body.model, body.items)
            .await
            .map_err(|e| e.into_response())?;
    Ok(Json(serde_json::json!({ "batchID": batch_id })))
}

/// `GET /session/{id}/batches?directory=` -> `{ batches: BatchRecord[] }`.
pub async fn list_batches(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let directory = require_directory(&q)?;
    let batches = state
        .batches
        .list_for_session(&directory, &id.to_string())
        .await;
    Ok(Json(serde_json::json!({ "batches": batches })))
}

/// `GET /batch/{id}?directory=` -> the record.
pub async fn get_batch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let _directory = require_directory(&q)?;
    let record = state
        .batches
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found(format!("unknown batch {id}")).into_response())?;
    Ok(Json(serde_json::to_value(record).unwrap_or_default()))
}

/// `POST /batch/{id}/cancel?directory=` -> `{ cancelled: bool }`.
pub async fn cancel_batch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let directory = require_directory(&q)?;
    let record = state
        .batches
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found(format!("unknown batch {id}")).into_response())?;
    if !state.batches.cancel(&id).await {
        return Ok(Json(serde_json::json!({ "cancelled": false })));
    }
    state.store.bus().publish(Event::batch_failed(
        &directory,
        &record.session_id,
        &id,
        "cancelled by user",
    ));
    Ok(Json(serde_json::json!({ "cancelled": true })))
}

/// Look up the provider spec from the instance's resolved config (built-ins
/// merged with global/project `providers`, so `openai`, `openrouter`, … work
/// even with no explicit config entry).
///
/// Shared with `services::batch::submit_batch_job` (the `:batch` chat
/// fallback); route handlers map the `ApiError` into a response.
pub(crate) async fn provider_spec(
    state: &AppState,
    directory: &str,
    provider: &str,
) -> Result<ProviderSpec, ApiError> {
    let instance = state
        .store
        .get_or_create_instance(directory)
        .await
        .map_err(|e| ApiError::internal(format!("instance error: {e}")))?;
    let config = instance.config_snapshot();
    config
        .provider_spec(provider)
        .ok_or_else(|| ApiError::bad_request(format!("unknown provider '{provider}'")))
}

/// Background driver: submit inline requests -> poll until terminal (or the
/// record is cancelled) -> read inline results -> fill items -> terminal events.
///
/// Each item becomes one entry of the `requests` array:
/// `{"custom_id": "<index>", "body": {"messages": [...]}}` — the model is
/// declared once, at batch level, as an OpenRouter slug.
pub(crate) fn spawn_batch_driver(state: AppState, batch_id: String, spec: ProviderSpec) {
    tokio::spawn(async move {
        let store: Arc<BatchStore> = state.batches.clone();
        if let Err(err) = drive_batch(&state, &batch_id, &spec).await {
            store
                .update(&batch_id, |rec| {
                    rec.state = BatchState::Failed;
                    rec.error = Some(err.clone());
                })
                .await;
            if let Some(rec) = store.get(&batch_id).await {
                state.store.bus().publish(Event::batch_failed(
                    &rec.directory,
                    &rec.session_id,
                    &batch_id,
                    &err,
                ));
            }
        }
    });
}

async fn drive_batch(state: &AppState, batch_id: &str, spec: &ProviderSpec) -> Result<(), String> {
    let store = state.batches.clone();
    let record = store
        .get(batch_id)
        .await
        .ok_or_else(|| "batch record vanished".to_string())?;
    // Strip the :batch suffix for the upstream model slug sent at batch level.
    // The full vendor/model path is preserved (e.g. "openai/gpt-6.1-sol-pro").
    let upstream = upstream_model(&record.model);

    // 1. Submit every item inline — OpenRouter persists the JSONL itself.
    store
        .update(batch_id, |rec| rec.state = BatchState::Uploading)
        .await;
    let requests: Vec<bebok_llm::BatchRequest> = record
        .items
        .iter()
        .map(|item| bebok_llm::BatchRequest {
            custom_id: item.index.to_string(),
            body: bebok_llm::BatchRequestBody {
                messages: item.messages.clone(),
            },
        })
        .collect();

    // 2. Submit the job.
    let job = bebok_llm::submit_batch(spec, &upstream, requests)
        .await
        .map_err(|e| format!("batch submit failed: {e}"))?;
    let provider_job_id = job.id.clone();
    store
        .update(batch_id, |rec| {
            rec.state = BatchState::Submitted;
            rec.job_id = Some(provider_job_id.clone());
        })
        .await;

    // 3. Poll until the provider job is terminal or we are cancelled.
    // OpenRouter may answer 404 ("not found") for a few polls right after
    // the submit, before the job materializes — tolerate those briefly
    // instead of failing the whole batch on the first poll.
    const MAX_POLL_NOT_FOUND: u32 = 5;
    let total = record.items.len();
    let mut not_found_streak: u32 = 0;
    loop {
        if store
            .get(batch_id)
            .await
            .map(|r| r.state == BatchState::Cancelled)
            .unwrap_or(true)
        {
            return Err("cancelled by user".to_string());
        }
        let job = match bebok_llm::get_batch(spec, &provider_job_id).await {
            Ok(job) => {
                not_found_streak = 0;
                job
            }
            Err(e)
                if matches!(&e, LlmError::Http { status: 404, .. })
                    && not_found_streak < MAX_POLL_NOT_FOUND =>
            {
                not_found_streak += 1;
                store
                    .update(batch_id, |rec| rec.state = BatchState::Polling)
                    .await;
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            Err(e) => return Err(format!("batch poll failed: {e}")),
        };
        let done = (job.request_counts.completed + job.request_counts.failed) as usize;
        state.store.bus().publish(Event::batch_progress(
            &record.directory,
            &record.session_id,
            batch_id,
            done.min(total),
            total,
        ));
        if job.status.is_terminal() {
            return finish_batch(state, batch_id, &job).await;
        }
        store
            .update(batch_id, |rec| rec.state = BatchState::Polling)
            .await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Map a completed job's inline `results[]` back onto batch items by
/// `custom_id` (the item index as a string).
///
/// Returns `(ok, failed)`. A result counts as `ok` when its response has
/// `status_code == 200` and carries assistant content; anything else
/// (provider `error`, non-200 status, missing content or a `custom_id`
/// with no matching item) marks the item `Failed` with a diagnostic.
pub fn apply_results(
    items: &mut [BatchItem],
    results: &[bebok_llm::BatchResult],
) -> (usize, usize) {
    use std::collections::HashMap;

    let mut by_id: HashMap<&str, &bebok_llm::BatchResult> = HashMap::new();
    for result in results {
        by_id.insert(result.custom_id.as_str(), result);
    }

    let mut ok = 0usize;
    let mut failed = 0usize;
    for item in items.iter_mut() {
        match by_id.get(item.index.to_string().as_str()) {
            Some(result) => {
                let status = result.response.as_ref().map(|r| r.status_code).unwrap_or(0);
                if status == 200 {
                    match result.content() {
                        Some(content) => {
                            item.status = BatchItemStatus::Done;
                            item.result = Some(content);
                            item.error = None;
                            ok += 1;
                        }
                        None => {
                            item.status = BatchItemStatus::Failed;
                            item.error = Some("batch result has no assistant content".to_string());
                            failed += 1;
                        }
                    }
                } else if let Some(err) = result.error.as_ref() {
                    item.status = BatchItemStatus::Failed;
                    item.error = Some(err.to_string());
                    failed += 1;
                } else {
                    item.status = BatchItemStatus::Failed;
                    item.error = Some(format!("batch request failed with status {status}"));
                    failed += 1;
                }
            }
            None => {
                item.status = BatchItemStatus::Failed;
                item.error = Some("missing from batch output".to_string());
                failed += 1;
            }
        }
    }
    (ok, failed)
}

/// Map a completed job's inline `results[]` back onto its items by
/// `custom_id` and publish the terminal event.
///
/// Pure logic lives in [`apply_results`]; this wrapper only touches the
/// store and the event bus.
async fn finish_batch(
    state: &AppState,
    batch_id: &str,
    job: &bebok_llm::BatchJob,
) -> Result<(), String> {
    let store = state.batches.clone();
    if job.status == bebok_llm::BatchStatus::Completed {
        let results = job.results.clone().unwrap_or_default();
        let (ok, failed) = store
            .update_and_map(batch_id, |rec| apply_results(&mut rec.items, &results))
            .await;
        store
            .update(batch_id, |rec| rec.state = BatchState::Completed)
            .await;
        if let Some(rec) = store.get(batch_id).await {
            state.store.bus().publish(Event::batch_completed(
                &rec.directory,
                &rec.session_id,
                batch_id,
                ok,
                failed,
            ));
        }
        Ok(())
    } else {
        Err(format!("batch ended with status {}", job.status))
    }
}

/// Strip the `:batch` / `:BATCH` suffix from a model identifier, preserving
/// the full upstream model id (including any vendor prefix the provider needs).
///
/// Batch jobs always route through the OpenRouter provider spec, so the
/// full model path must be preserved for OpenRouter to resolve the upstream:
///
/// - `openai/gpt-6.1-sol-pro:batch`       -> `openai/gpt-6.1-sol-pro`
/// - `openrouter/deepseek/deepseek-chat:batch` -> `deepseek/deepseek-chat`
/// - `openrouter/openai/gpt-6.1-sol-pro:batch` -> `openai/gpt-6.1-sol-pro`
/// - `plain-model:batch`                    -> `plain-model`
fn upstream_model(model: &str) -> String {
    let without_suffix = model
        .strip_suffix(":batch")
        .or_else(|| model.strip_suffix(":BATCH"))
        .unwrap_or(model);
    // A leading `openrouter/` names the Bebok provider, not the upstream
    // vendor — OpenRouter itself expects `vendor/model`.
    without_suffix
        .strip_prefix("openrouter/")
        .unwrap_or(without_suffix)
        .to_string()
}

/// Resolve the provider name for batch routing.
///
/// The batch client speaks the native OpenRouter batch protocol
/// (`POST /batches` with inline `requests`), so batch jobs are always
/// routed through the `openrouter` provider spec regardless of the model's
/// Bebok provider prefix.
pub(crate) fn batch_provider(_model: &str) -> String {
    "openrouter".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_strips_suffix_preserves_path() {
        // Bare vendor/model with :batch
        assert_eq!(
            upstream_model("openrouter/deepseek/deepseek-chat:batch"),
            "deepseek/deepseek-chat"
        );
        // Direct vendor model routed through openrouter
        assert_eq!(
            upstream_model("openai/gpt-6.1-sol-pro:batch"),
            "openai/gpt-6.1-sol-pro"
        );
        // Nested openrouter prefix (e.g. openrouter/openai/...)
        assert_eq!(
            upstream_model("openrouter/openai/gpt-6.1-sol-pro:batch"),
            "openai/gpt-6.1-sol-pro"
        );
        // No :batch suffix — pass through unchanged
        assert_eq!(upstream_model("openrouter/gpt-4o"), "gpt-4o");
        // Plain model with :batch
        assert_eq!(upstream_model("plain-model:batch"), "plain-model");
        // Case-insensitive suffix
        assert_eq!(
            upstream_model("openrouter/deepseek/deepseek-chat:BATCH"),
            "deepseek/deepseek-chat"
        );
    }

    #[test]
    fn batch_provider_always_returns_openrouter() {
        assert_eq!(batch_provider("openai/gpt-6.1-sol-pro:batch"), "openrouter");
        assert_eq!(
            batch_provider("openrouter/deepseek/deepseek-chat:batch"),
            "openrouter"
        );
    }

    fn sample_items() -> Vec<BatchItem> {
        (0..3)
            .map(|index| BatchItem {
                index,
                model: "openai/gpt-4o:batch".into(),
                messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                status: BatchItemStatus::Pending,
                result: None,
                error: None,
            })
            .collect()
    }

    fn ok_result(custom_id: &str, content: &str) -> bebok_llm::BatchResult {
        bebok_llm::BatchResult {
            id: Some(format!("batch_req_{custom_id}")),
            custom_id: custom_id.into(),
            response: Some(bebok_llm::BatchResponse {
                status_code: 200,
                request_id: Some("request_1".into()),
                body: serde_json::json!({
                    "choices": [{ "message": { "role": "assistant", "content": content } }]
                }),
            }),
            error: None,
        }
    }

    #[test]
    fn apply_results_maps_done_by_custom_id() {
        let mut items = sample_items();
        let results = vec![ok_result("0", "hello"), ok_result("2", "world")];

        let (ok, failed) = apply_results(&mut items, &results);

        assert_eq!((ok, failed), (2, 1));
        assert_eq!(items[0].status, BatchItemStatus::Done);
        assert_eq!(items[0].result.as_deref(), Some("hello"));
        assert_eq!(items[2].status, BatchItemStatus::Done);
        assert_eq!(items[2].result.as_deref(), Some("world"));
        // Item 1 has no matching custom_id.
        assert_eq!(items[1].status, BatchItemStatus::Failed);
        assert_eq!(items[1].error.as_deref(), Some("missing from batch output"));
    }

    #[test]
    fn apply_results_marks_errors_failed() {
        let mut items = sample_items();
        let results = vec![
            bebok_llm::BatchResult {
                custom_id: "0".into(),
                response: None,
                error: Some(serde_json::json!({"message": "boom"})),
                ..bebok_llm::BatchResult::default()
            },
            bebok_llm::BatchResult {
                custom_id: "1".into(),
                response: Some(bebok_llm::BatchResponse {
                    status_code: 429,
                    request_id: None,
                    body: serde_json::json!({}),
                }),
                error: None,
                ..bebok_llm::BatchResult::default()
            },
            bebok_llm::BatchResult {
                custom_id: "2".into(),
                response: Some(bebok_llm::BatchResponse {
                    status_code: 200,
                    request_id: None,
                    body: serde_json::json!({"choices": []}),
                }),
                error: None,
                ..bebok_llm::BatchResult::default()
            },
        ];

        let (ok, failed) = apply_results(&mut items, &results);

        assert_eq!((ok, failed), (0, 3));
        assert!(items.iter().all(|i| i.status == BatchItemStatus::Failed));
        assert!(items[0].error.as_deref().unwrap().contains("boom"));
        assert!(items[1].error.as_deref().unwrap().contains("status 429"));
        assert_eq!(
            items[2].error.as_deref(),
            Some("batch result has no assistant content")
        );
    }
}
