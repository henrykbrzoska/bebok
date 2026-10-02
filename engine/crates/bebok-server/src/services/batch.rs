//! Shared batch-job submission service.
//!
//! Used by both `POST /session/{id}/batch` (the Batch panel) and the
//! `:batch` branch of `POST /session/{id}/prompt` (chat fallback, Option A):
//! validate the model, resolve the OpenRouter provider spec, store the
//! [`BatchRecord`], publish `batch.submitted` and spawn the background
//! driver (submit inline requests -> poll -> inline results).
//!
//! A batch job never takes the session turn lock, so the interactive
//! session stays responsive while the job runs for minutes/hours.

use uuid::Uuid;

use bebok_core::batch::{BatchItem, BatchItemStatus, BatchRecord, BatchState};
use bebok_core::event::Event;
use bebok_llm::is_batch_model;

use crate::error::ApiError;
use crate::routes::batch::{BatchItemRequest, batch_provider, provider_spec, spawn_batch_driver};
use crate::state::AppState;

/// Submit a batch job: validate, store the record, emit `batch.submitted`,
/// spawn the driver. Returns the new batch id.
pub async fn submit_batch_job(
    state: &AppState,
    directory: &str,
    session_id: Uuid,
    model: &str,
    items: Vec<BatchItemRequest>,
) -> Result<String, ApiError> {
    if !is_batch_model(model) {
        return Err(ApiError::bad_request(format!(
            "model '{model}' is not a batch model: batch jobs need the ':batch' suffix (e.g. 'openrouter/deepseek/deepseek-chat:batch')"
        )));
    }
    if items.is_empty() {
        return Err(ApiError::bad_request("batch needs at least one item"));
    }
    // Batch jobs always route through the openrouter provider spec
    // because the batch client speaks OpenRouter protocol (POST /batches)
    // regardless of the model's Bebok provider prefix.
    let provider = batch_provider(model);

    // Resolve the provider spec now (fail fast on missing key / unknown
    // provider) so the background task starts from a known-good spec.
    let spec = provider_spec(state, directory, &provider).await?;

    let record = BatchRecord {
        id: String::new(),
        directory: directory.to_string(),
        session_id: session_id.to_string(),
        provider: provider.clone(),
        model: model.to_string(),
        state: BatchState::Created,
        job_id: None,
        items: items
            .into_iter()
            .enumerate()
            .map(|(index, item)| BatchItem {
                index: index as u32,
                model: model.to_string(),
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
        directory,
        &session_id.to_string(),
        &batch_id,
        count,
    ));

    spawn_batch_driver(state.clone(), batch_id.clone(), spec);
    Ok(batch_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(base: &std::path::Path) -> AppState {
        AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: std::sync::Arc::new(bebok_pty::PtyManager::new()),
            debug: std::sync::Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: std::sync::Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            command_queue: std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::state::CommandRegistry::new(),
            )),
            batches: std::sync::Arc::new(bebok_core::BatchStore::new()),
        }
    }

    fn one_item() -> Vec<crate::routes::batch::BatchItemRequest> {
        vec![crate::routes::batch::BatchItemRequest {
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
        }]
    }

    #[tokio::test]
    async fn rejects_non_batch_model() {
        let base = std::env::temp_dir().join(format!("bebok-batchsvc-{}", Uuid::new_v4()));
        let state = test_state(&base);
        let err = submit_batch_job(
            &state,
            "/tmp",
            Uuid::new_v4(),
            "openrouter/deepseek/deepseek-chat",
            one_item(),
        )
        .await
        .expect_err("non-:batch model must be rejected");
        assert!(matches!(err, ApiError::BadRequest(_)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn rejects_empty_items() {
        let base = std::env::temp_dir().join(format!("bebok-batchsvc-{}", Uuid::new_v4()));
        let state = test_state(&base);
        let err = submit_batch_job(
            &state,
            "/tmp",
            Uuid::new_v4(),
            "openrouter/deepseek/deepseek-chat:batch",
            vec![],
        )
        .await
        .expect_err("empty items must be rejected");
        assert!(matches!(err, ApiError::BadRequest(_)));
        let _ = std::fs::remove_dir_all(&base);
    }
}
