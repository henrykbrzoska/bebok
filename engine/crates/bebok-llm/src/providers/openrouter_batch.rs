//! OpenRouter Batch API client (native shape).
//!
//! OpenRouter's native Batch API takes the work inline: one
//! `POST {base}/batches` carrying `endpoint`, `model`, `completion_window`
//! and a `requests` array of `{custom_id, body}` — no JSONL upload, no
//! result download. Results come back inline in the completed batch object
//! (`results[]`), so the lifecycle is submit → poll → read results.
//!
//! This module is *not* a [`crate::Provider`] implementation — the `Provider`
//! trait exposes a single `stream` method that does not fit the batch REST
//! lifecycle. Instead it provides standalone async functions driving the
//! batch job, using the same authentication and base-URL resolution as the
//! streaming provider.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

use crate::ProviderSpec;
use crate::provider::{LlmError, retry_after_from_headers};
use crate::spec::resolve_api_key;

/// The endpoint a batch job runs against.
const BATCH_ENDPOINT: &str = "/v1/chat/completions";
/// The only completion window OpenRouter accepts.
const COMPLETION_WINDOW: &str = "24h";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Batch job status as reported by `POST /batches` / `GET /batches/{id}`.
///
/// Unknown strings the provider may add later are preserved in
/// [`BatchStatus::Other`] instead of failing the whole deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchStatus {
    Validating,
    Failed,
    InProgress,
    Finalizing,
    Completed,
    Expired,
    Cancelling,
    Cancelled,
    Other(String),
}

impl BatchStatus {
    /// Wire representation of the status.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Validating => "validating",
            Self::Failed => "failed",
            Self::InProgress => "in_progress",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Expired => "expired",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
            Self::Other(raw) => raw.as_str(),
        }
    }

    fn from_raw(raw: &str) -> Self {
        match raw {
            "validating" => Self::Validating,
            "failed" => Self::Failed,
            "in_progress" => Self::InProgress,
            "finalizing" => Self::Finalizing,
            "completed" => Self::Completed,
            "expired" => Self::Expired,
            "cancelling" => Self::Cancelling,
            "cancelled" => Self::Cancelled,
            other => Self::Other(other.to_string()),
        }
    }

    /// True once the job reached a terminal state (no further polling useful).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Expired | Self::Cancelled
        )
    }
}

impl std::fmt::Display for BatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for BatchStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BatchStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_raw(&raw))
    }
}

impl Default for BatchStatus {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

/// Per-request counts tracked by the batch (`request_counts` on the wire).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchCounts {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub failed: u64,
}

/// One inline request of the `requests` array: a `custom_id` (Bebok sends
/// the item index) plus the request body (chat messages only — the model is
/// declared once, at batch level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    pub custom_id: String,
    pub body: BatchRequestBody,
}

/// The body of a single batched request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequestBody {
    pub messages: Vec<serde_json::Value>,
}

/// Body of `POST /batches`.
///
/// The field order is load-bearing: OpenRouter stream-parses the payload and
/// returns `400` if `requests` is serialized before the scalar fields, so
/// this must stay a struct with `requests` last.
#[derive(Debug, Clone, Serialize)]
pub struct SubmitBatchBody {
    pub endpoint: &'static str,
    pub model: String,
    pub completion_window: &'static str,
    pub requests: Vec<BatchRequest>,
}

/// The successful HTTP response wrapper of one result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    #[serde(default)]
    pub status_code: u16,
    #[serde(default)]
    pub request_id: Option<String>,
    /// The chat-completion body.
    #[serde(default)]
    pub body: serde_json::Value,
}

/// One entry of the completed batch's inline `results` array.
///
/// Exactly one of `response` / `error` is populated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchResult {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub custom_id: String,
    #[serde(default)]
    pub response: Option<BatchResponse>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

impl BatchResult {
    /// The assistant message content of a successful result, if present.
    ///
    /// Usually a plain string; when the provider returns content parts,
    /// their `text` fields are concatenated.
    pub fn content(&self) -> Option<String> {
        let content = self
            .response
            .as_ref()?
            .body
            .pointer("/choices/0/message/content")?;
        match content {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Array(parts) => {
                let mut out = String::new();
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        out.push_str(text);
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }
}

/// Token usage of a finished batch (`null` until it completes).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// A batch job returned by `POST /batches` / `GET /batches/{id}`.
///
/// Every field is defaulted so a partial provider response still deserializes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchJob {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: BatchStatus,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub completion_window: String,
    /// The provider reports progress in `request_counts`.
    #[serde(default, alias = "counts")]
    pub request_counts: BatchCounts,
    #[serde(default)]
    pub usage: Option<BatchUsage>,
    /// Inline results — `null` until the batch completes.
    #[serde(default)]
    pub results: Option<Vec<BatchResult>>,
    /// Top-level failure reason for a `failed` batch.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Build the request headers: `Authorization: Bearer <key>` plus the optional
/// `HTTP-Referer` / `X-Title` extras OpenRouter recognises.
fn build_headers(spec: &ProviderSpec) -> Result<Vec<(String, String)>, LlmError> {
    let key = resolve_api_key(spec)
        .ok_or_else(|| LlmError::Provider(format!("no API key for provider '{}'", spec.name)))?;

    let mut headers = vec![("authorization".to_string(), format!("Bearer {key}"))];

    if let Some(serde_json::Value::String(referer)) = spec.extra.get("http_referer")
        && !referer.trim().is_empty()
    {
        headers.push(("http-referer".to_string(), referer.clone()));
    }
    if let Some(serde_json::Value::String(title)) = spec.extra.get("x_title")
        && !title.trim().is_empty()
    {
        headers.push(("x-title".to_string(), title.clone()));
    }

    Ok(headers)
}

/// Send a request, mapping transport failures to [`LlmError::Request`] and
/// non-2xx responses to [`LlmError::Http`].
async fn send(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> Result<reqwest::Response, LlmError> {
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let resp = req.send().await.map_err(LlmError::Request)?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let retry_after = retry_after_from_headers(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        return Err(LlmError::Http {
            status,
            body,
            retry_after,
        });
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a batch job from inline requests.
///
/// `POST {base}/batches` with
/// `{"endpoint", "model", "completion_window", "requests"}` — in that exact
/// key order, because OpenRouter rejects a body where `requests` comes first.
pub async fn submit_batch(
    spec: &ProviderSpec,
    model: &str,
    requests: Vec<BatchRequest>,
) -> Result<BatchJob, LlmError> {
    let headers = build_headers(spec)?;
    let client = reqwest::Client::new();
    let body = SubmitBatchBody {
        endpoint: BATCH_ENDPOINT,
        model: model.to_string(),
        completion_window: COMPLETION_WINDOW,
        requests,
    };

    let resp = send(
        client
            .post(format!("{}/batches", spec.base_url()))
            .json(&body),
        &headers,
    )
    .await?;

    let job: BatchJob = resp.json().await.map_err(LlmError::Request)?;
    Ok(job)
}

/// Fetch the current state of a batch job — `GET {base}/batches/{id}`.
pub async fn get_batch(spec: &ProviderSpec, batch_id: &str) -> Result<BatchJob, LlmError> {
    let headers = build_headers(spec)?;
    let client = reqwest::Client::new();

    let resp = send(
        client.get(format!("{}/batches/{batch_id}", spec.base_url())),
        &headers,
    )
    .await?;

    let job: BatchJob = resp.json().await.map_err(LlmError::Request)?;
    Ok(job)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_requests() -> Vec<BatchRequest> {
        vec![
            BatchRequest {
                custom_id: "0".into(),
                body: BatchRequestBody {
                    messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                },
            },
            BatchRequest {
                custom_id: "1".into(),
                body: BatchRequestBody {
                    messages: vec![serde_json::json!({"role": "user", "content": "bye"})],
                },
            },
        ]
    }

    #[test]
    fn submit_body_orders_scalars_before_requests() {
        let body = SubmitBatchBody {
            endpoint: BATCH_ENDPOINT,
            model: "openai/gpt-4o".into(),
            completion_window: COMPLETION_WINDOW,
            requests: sample_requests(),
        };
        let text = serde_json::to_string(&body).unwrap();

        let endpoint = text.find("\"endpoint\"").unwrap();
        let model = text.find("\"model\"").unwrap();
        let window = text.find("\"completion_window\"").unwrap();
        let requests = text.find("\"requests\"").unwrap();
        assert!(endpoint < model, "{text}");
        assert!(model < window, "{text}");
        assert!(window < requests, "{text}");

        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["endpoint"], "/v1/chat/completions");
        assert_eq!(value["model"], "openai/gpt-4o");
        assert_eq!(value["completion_window"], "24h");
        assert_eq!(value["requests"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn request_bodies_carry_messages_only() {
        let text = serde_json::to_string(&sample_requests()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(value[0]["custom_id"], "0");
        assert_eq!(value[0]["body"]["messages"][0]["content"], "hi");
        // The model lives at batch level, never inside a request body.
        assert!(value[0]["body"].get("model").is_none());
        assert!(!text.contains("\"method\""));
        assert!(!text.contains("\"url\""));
    }

    #[test]
    fn deserialize_completed_batch_with_results() {
        let json = serde_json::json!({
            "id": "batch_123",
            "object": "batch",
            "endpoint": "/v1/chat/completions",
            "model": "openai/gpt-4o",
            "completion_window": "24h",
            "status": "completed",
            "request_counts": { "total": 2, "completed": 1, "failed": 1 },
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 },
            "results": [
                {
                    "id": "batch_req_1",
                    "custom_id": "0",
                    "response": {
                        "status_code": 200,
                        "request_id": "request_123",
                        "body": {
                            "choices": [{ "message": { "role": "assistant", "content": "hello" } }]
                        }
                    },
                    "error": null
                },
                {
                    "custom_id": "1",
                    "response": null,
                    "error": { "code": "server_error", "message": "boom" }
                }
            ],
            "error": null
        });

        let job: BatchJob = serde_json::from_value(json).unwrap();

        assert_eq!(job.id, "batch_123");
        assert_eq!(job.status, BatchStatus::Completed);
        assert!(job.status.is_terminal());
        assert_eq!(job.endpoint, "/v1/chat/completions");
        assert_eq!(job.model, "openai/gpt-4o");
        assert_eq!(job.completion_window, "24h");
        assert_eq!(job.request_counts.total, 2);
        assert_eq!(job.request_counts.completed, 1);
        assert_eq!(job.request_counts.failed, 1);
        assert_eq!(job.usage.as_ref().unwrap().total_tokens, 15);

        let results = job.results.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].custom_id, "0");
        assert_eq!(results[0].content().as_deref(), Some("hello"));
        assert_eq!(results[0].response.as_ref().unwrap().status_code, 200);
        assert_eq!(results[1].custom_id, "1");
        assert_eq!(results[1].content(), None);
        assert_eq!(results[1].error.as_ref().unwrap()["message"], "boom");
    }

    #[test]
    fn in_progress_batch_has_null_results() {
        let job: BatchJob = serde_json::from_value(serde_json::json!({
            "id": "batch_123",
            "status": "in_progress",
            "request_counts": { "total": 4, "completed": 1, "failed": 0 },
            "results": null,
            "usage": null,
            "error": null
        }))
        .unwrap();

        assert_eq!(job.status, BatchStatus::InProgress);
        assert!(!job.status.is_terminal());
        assert_eq!(job.request_counts.total, 4);
        assert!(job.results.is_none());
        assert!(job.usage.is_none());
    }

    #[test]
    fn legacy_counts_key_still_deserializes() {
        let job: BatchJob =
            serde_json::from_value(serde_json::json!({ "counts": { "total": 3 } })).unwrap();
        assert_eq!(job.request_counts.total, 3);
    }

    #[test]
    fn unknown_status_becomes_other() {
        let json = serde_json::json!({
            "id": "batch_new",
            "status": "awaiting_upload",
            "request_counts": {}
        });

        let job: BatchJob = serde_json::from_value(json).unwrap();

        assert_eq!(
            job.status,
            BatchStatus::Other("awaiting_upload".to_string())
        );
        assert!(!job.status.is_terminal());
        assert_eq!(job.status.to_string(), "awaiting_upload");
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let job: BatchJob = serde_json::from_value(serde_json::json!({})).unwrap();

        assert_eq!(job.id, "");
        assert_eq!(job.endpoint, "");
        assert_eq!(job.model, "");
        assert_eq!(job.completion_window, "");
        assert_eq!(job.request_counts, BatchCounts::default());
        assert!(job.usage.is_none());
        assert!(job.results.is_none());
        assert!(job.error.is_none());
    }

    #[test]
    fn known_statuses_round_trip() {
        let statuses = [
            BatchStatus::Validating,
            BatchStatus::Failed,
            BatchStatus::InProgress,
            BatchStatus::Finalizing,
            BatchStatus::Completed,
            BatchStatus::Expired,
            BatchStatus::Cancelling,
            BatchStatus::Cancelled,
        ];
        for status in statuses {
            let text = serde_json::to_string(&status).unwrap();
            assert_eq!(text, format!("\"{}\"", status.as_str()));
            let back: BatchStatus = serde_json::from_str(&text).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn other_status_round_trips_verbatim() {
        let status = BatchStatus::Other("brand_new".to_string());
        let text = serde_json::to_string(&status).unwrap();
        assert_eq!(text, "\"brand_new\"");
        assert_eq!(serde_json::from_str::<BatchStatus>(&text).unwrap(), status);
    }

    #[test]
    fn batch_counts_defaults_to_zero() {
        let counts: BatchCounts = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(counts.total, 0);
        assert_eq!(counts.completed, 0);
        assert_eq!(counts.failed, 0);
    }

    #[test]
    fn result_content_ignores_non_string_content() {
        let result = BatchResult {
            response: Some(BatchResponse {
                status_code: 200,
                request_id: None,
                body: serde_json::json!({ "choices": [{ "message": { "content": 42 } }] }),
            }),
            ..BatchResult::default()
        };
        assert_eq!(result.content(), None);
    }

    #[test]
    fn non_string_status_is_rejected() {
        // A non-string status is a schema break, not a new value.
        let err = serde_json::from_value::<BatchJob>(serde_json::json!({ "status": 7 }));
        assert!(err.is_err(), "numeric status must not deserialize");
    }

    #[test]
    fn headers_require_a_resolvable_key() {
        let spec = ProviderSpec {
            name: "openrouter".to_string(),
            api_key: Some("test-key".to_string()),
            ..ProviderSpec::default()
        };
        let headers = build_headers(&spec).expect("test key must resolve");
        assert!(headers.iter().any(|(name, _)| name == "authorization"));
    }
}
