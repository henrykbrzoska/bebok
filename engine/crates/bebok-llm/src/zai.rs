use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::anthropic::{anthropic_body, anthropic_stream};
use crate::provider::{ChatRequest, LlmError, Provider, StreamEvent, StreamResult};

/// Z.ai Anthropic-compatible Messages endpoint.
pub const ZAI_ANTHROPIC_URL: &str = "https://api.z.ai/api/anthropic/v1/messages";

/// Z.ai / GLM provider using the Anthropic-compatible SSE endpoint.
pub struct ZaiProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl ZaiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: ZAI_ANTHROPIC_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

/// `zai/glm-4.6` -> `glm-4.6`
fn model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

#[async_trait]
impl Provider for ZaiProvider {
    fn name(&self) -> &str {
        "zai"
    }

    async fn stream(
        &self,
        req: ChatRequest,
    ) -> StreamResult<BoxStream<'static, StreamResult<StreamEvent>>> {
        let model = model_name(&req.model).to_string();
        let body = anthropic_body(&req, &model);

        let resp = self
            .client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after = crate::provider::retry_after_from_headers(resp.headers());
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Http {
                status,
                body: text,
                retry_after,
            });
        }

        Ok(Box::pin(anthropic_stream(resp)))
    }
}
