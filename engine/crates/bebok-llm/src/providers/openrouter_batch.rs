//! OpenRouter Batch API client.
//!
//! This module is *not* a [`crate::Provider`] implementation — the `Provider`
//! trait exposes a single `stream` method that does not fit the batch REST
//! lifecycle (submit → poll → download). Instead it provides standalone async
//! functions driving the batch job through its three phases, using the same
//! authentication and base-URL resolution as the streaming provider.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};

use crate::ProviderSpec;
use crate::provider::{LlmError, retry_after_from_headers};
use crate::spec::resolve_api_key;

/// The endpoint a batch job runs against.
const BATCH_ENDPOINT: &str = "/v1/chat/completions";
/// How long the provider keeps accepting a batch's work.
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

/// Per-request counts tracked by the batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchCounts {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub failed: u64,
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
    pub input_file_id: Option<String>,
    #[serde(default)]
    pub output_file_id: Option<String>,
    #[serde(default)]
    pub error_file_id: Option<String>,
    #[serde(default)]
    pub counts: BatchCounts,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl Default for BatchStatus {
    fn default() -> Self {
        Self::Other(String::new())
    }
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

/// Read a `{ "id": "..." }` style object and return the id.
fn expect_id(value: &serde_json::Value, what: &str) -> Result<String, LlmError> {
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            LlmError::Parse(format!(
                "{what} response is missing a string 'id' field: {value}"
            ))
        })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Upload a JSONL payload as a batch input file and return its file id.
///
/// `POST {base}/files` with `{"purpose": "batch", "content": <jsonl>}`.
pub async fn upload_batch_file(spec: &ProviderSpec, jsonl: &str) -> Result<String, LlmError> {
    let headers = build_headers(spec)?;
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "purpose": "batch", "content": jsonl });

    let resp = send(
        client
            .post(format!("{}/files", spec.base_url()))
            .json(&body),
        &headers,
    )
    .await?;

    let value: serde_json::Value = resp.json().await.map_err(LlmError::Request)?;
    expect_id(&value, "file upload")
}

/// Create a batch job for an already-uploaded input file.
///
/// `POST {base}/batches` with
/// `{"input_file_id", "endpoint", "completion_window", "metadata"}`.
pub async fn submit_batch(
    spec: &ProviderSpec,
    input_file_id: &str,
    metadata: serde_json::Value,
) -> Result<BatchJob, LlmError> {
    let headers = build_headers(spec)?;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "input_file_id": input_file_id,
        "endpoint": BATCH_ENDPOINT,
        "completion_window": COMPLETION_WINDOW,
        "metadata": metadata,
    });

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

/// Download a file's content as text — `GET {base}/files/{file_id}/content`.
///
/// Used to fetch a completed batch's `output_file_id` (or `error_file_id`).
pub async fn download_output(spec: &ProviderSpec, file_id: &str) -> Result<String, LlmError> {
    let headers = build_headers(spec)?;
    let client = reqwest::Client::new();

    let resp = send(
        client.get(format!("{}/files/{file_id}/content", spec.base_url())),
        &headers,
    )
    .await?;

    resp.text().await.map_err(LlmError::Request)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_completed_batch_with_counts() {
        let json = serde_json::json!({
            "id": "batch_abc123",
            "status": "completed",
            "endpoint": "/v1/chat/completions",
            "input_file_id": "file_in_001",
            "output_file_id": "file_out_001",
            "error_file_id": null,
            "counts": { "total": 10, "completed": 10, "failed": 0 },
            "metadata": { "project": "test" }
        });

        let job: BatchJob = serde_json::from_value(json).unwrap();

        assert_eq!(job.id, "batch_abc123");
        assert_eq!(job.status, BatchStatus::Completed);
        assert!(job.status.is_terminal());
        assert_eq!(job.endpoint, "/v1/chat/completions");
        assert_eq!(job.input_file_id.as_deref(), Some("file_in_001"));
        assert_eq!(job.output_file_id.as_deref(), Some("file_out_001"));
        assert!(job.error_file_id.is_none());
        assert_eq!(job.counts.total, 10);
        assert_eq!(job.counts.completed, 10);
        assert_eq!(job.counts.failed, 0);
        assert_eq!(job.metadata["project"], "test");
    }

    #[test]
    fn unknown_status_becomes_other() {
        let json = serde_json::json!({
            "id": "batch_new",
            "status": "awaiting_upload",
            "counts": {}
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
        assert!(job.input_file_id.is_none());
        assert!(job.output_file_id.is_none());
        assert!(job.error_file_id.is_none());
        assert_eq!(job.counts, BatchCounts::default());
        assert!(job.metadata.is_null());
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
    fn expect_id_reads_the_id_field() {
        let value = serde_json::json!({ "id": "file_1", "bytes": 12 });
        assert_eq!(expect_id(&value, "file upload").unwrap(), "file_1");

        let missing = serde_json::json!({ "bytes": 12 });
        assert!(expect_id(&missing, "file upload").is_err());
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
            api_key: None,
            ..ProviderSpec::default()
        };
        // Either the env var is set (headers built) or the call fails cleanly.
        match build_headers(&spec) {
            Ok(headers) => {
                assert!(headers.iter().any(|(name, _)| name == "authorization"));
            }
            Err(err) => assert!(
                matches!(err, LlmError::Provider(_)),
                "expected a clean provider error, got {err:?}"
            ),
        }
    }
}
