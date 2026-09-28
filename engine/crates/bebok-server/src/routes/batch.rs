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

use bebok_core::batch::{BatchItem, BatchItemStatus, BatchRecord, BatchState, BatchStore};
use bebok_core::event::Event;
use bebok_llm::{ProviderSpec, is_batch_model};

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
/// the background driver (upload -> submit -> poll -> download) and returns
/// immediately. The interactive turn lock is never touched.
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
    if !is_batch_model(&body.model) {
        return Err(ApiError::bad_request(format!(
            "model '{}' is not a batch model: batch jobs need the ':batch' suffix (e.g. 'openrouter/deepseek/deepseek-chat:batch')",
            body.model
        ))
        .into_response());
    }
    if body.items.is_empty() {
        return Err(ApiError::bad_request("batch needs at least one item").into_response());
    }
    let provider = body
        .model
        .split_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| "openrouter".to_string());

    // Resolve the provider spec now (fail fast on missing key / unknown
    // provider) so the background task starts from a known-good spec.
    let spec = provider_spec(&state, &directory, &provider).await?;

    let record = BatchRecord {
        id: String::new(),
        directory: directory.clone(),
        session_id: id.to_string(),
        provider: provider.clone(),
        model: body.model.clone(),
        state: BatchState::Created,
        job_id: None,
        items: body
            .items
            .into_iter()
            .enumerate()
            .map(|(index, item)| BatchItem {
                index: index as u32,
                model: body.model.clone(),
                messages: item.messages,
                status: BatchItemStatus::Pending,
                result: None,
                error: None,
            })
            .collect(),
        created_at: String::new(),
        updated_at: String::new(),
        error: None,
    };
    let batch_id = state.batches.insert(record).await;
    let count = state
        .batches
        .get(&batch_id)
        .await
        .map(|r| r.items.len())
        .unwrap_or(0);
    state.store.bus().publish(Event::batch_submitted(
        &directory,
        &id.to_string(),
        &batch_id,
        count,
    ));

    spawn_batch_driver(state.clone(), batch_id.clone(), spec);
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

/// Look up the provider spec from the instance's resolved config.
async fn provider_spec(
    state: &AppState,
    directory: &str,
    provider: &str,
) -> Result<ProviderSpec, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(directory)
        .await
        .map_err(|e| ApiError::internal(format!("instance error: {e}")).into_response())?;
    let config = instance.config_snapshot();
    config
        .providers
        .iter()
        .find(|p| p.name == provider)
        .cloned()
        .ok_or_else(|| {
            ApiError::bad_request(format!("unknown provider '{provider}'")).into_response()
        })
}

/// Background driver: upload JSONL -> submit -> poll until terminal (or the
/// record is cancelled) -> download output -> fill items -> terminal events.
///
/// Each item becomes one JSONL line: `{"custom_id": "<index>",
/// "method": "POST", "url": "/v1/chat/completions",
/// "body": {"model": "<upstream>", "messages": [...]}}`.
fn spawn_batch_driver(state: AppState, batch_id: String, spec: ProviderSpec) {
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
    // Strip the provider prefix AND the :batch suffix for the upstream id.
    let upstream = upstream_model(&record.model);

    // 1. Upload the JSONL input file.
    store
        .update(batch_id, |rec| rec.state = BatchState::Uploading)
        .await;
    let mut lines = Vec::with_capacity(record.items.len());
    for item in &record.items {
        let line = serde_json::json!({
            "custom_id": item.index.to_string(),
            "method": "POST",
            "url": "/v1/chat/completions",
            "body": { "model": upstream, "messages": item.messages },
        });
        lines.push(serde_json::to_string(&line).map_err(|e| e.to_string())?);
    }
    let file_id = bebok_llm::upload_batch_file(spec, &lines.join("\n"))
        .await
        .map_err(|e| format!("file upload failed: {e}"))?;

    // 2. Submit the job.
    let job = bebok_llm::submit_batch(
        spec,
        &file_id,
        serde_json::json!({ "bebok_batch": batch_id }),
    )
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
    let total = record.items.len();
    loop {
        if store
            .get(batch_id)
            .await
            .map(|r| r.state == BatchState::Cancelled)
            .unwrap_or(true)
        {
            return Err("cancelled by user".to_string());
        }
        let job = bebok_llm::get_batch(spec, &provider_job_id)
            .await
            .map_err(|e| format!("batch poll failed: {e}"))?;
        let done = (job.counts.completed + job.counts.failed) as usize;
        state.store.bus().publish(Event::batch_progress(
            &record.directory,
            &record.session_id,
            batch_id,
            done.min(total),
            total,
        ));
        if job.status.is_terminal() {
            return finish_batch(state, batch_id, spec, &job).await;
        }
        store
            .update(batch_id, |rec| rec.state = BatchState::Polling)
            .await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Fetch the output file, map each JSONL line back onto its item by
/// `custom_id`, and publish the terminal event.
async fn finish_batch(
    state: &AppState,
    batch_id: &str,
    spec: &ProviderSpec,
    job: &bebok_llm::BatchJob,
) -> Result<(), String> {
    let store = state.batches.clone();
    if job.status == bebok_llm::BatchStatus::Completed {
        let file_id = job.output_file_id.clone().unwrap_or_default();
        let text = bebok_llm::download_output(spec, &file_id)
            .await
            .map_err(|e| format!("output download failed: {e}"))?;
        let mut ok = 0usize;
        let mut failed = 0usize;
        let mut results: std::collections::HashMap<u32, Result<String, String>> =
            std::collections::HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let index: u32 = value
                .get("custom_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(u32::MAX);
            if index == u32::MAX {
                continue;
            }
            let entry = if value.get("error").is_some() {
                failed += 1;
                Err(value.to_string())
            } else {
                let content = value
                    .pointer("/response/body/choices/0/message/content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                ok += 1;
                Ok(content)
            };
            results.insert(index, entry);
        }
        store
            .update(batch_id, |rec| {
                for item in &mut rec.items {
                    match results.remove(&item.index) {
                        Some(Ok(content)) => {
                            item.status = BatchItemStatus::Done;
                            item.result = Some(content);
                        }
                        Some(Err(err)) => {
                            item.status = BatchItemStatus::Failed;
                            item.error = Some(err);
                        }
                        None => {
                            item.status = BatchItemStatus::Failed;
                            item.error = Some("missing from batch output".to_string());
                        }
                    }
                }
                rec.state = BatchState::Completed;
            })
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

/// `openrouter/deepseek/deepseek-chat:batch` -> `deepseek/deepseek-chat`.
fn upstream_model(model: &str) -> String {
    let without_prefix = match model.split_once('/') {
        Some((_, rest)) => rest,
        None => model,
    };
    without_prefix
        .strip_suffix(":batch")
        .or_else(|| without_prefix.strip_suffix(":BATCH"))
        .unwrap_or(without_prefix)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_strips_prefix_and_suffix() {
        assert_eq!(
            upstream_model("openrouter/deepseek/deepseek-chat:batch"),
            "deepseek/deepseek-chat"
        );
        assert_eq!(upstream_model("openrouter/gpt-4o"), "gpt-4o");
        assert_eq!(upstream_model("plain-model:batch"), "plain-model");
    }
}
