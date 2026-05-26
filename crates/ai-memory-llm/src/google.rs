//! Google Gemini Embeddings API (`embedContent`).
//!
//! See <https://ai.google.dev/gemini-api/docs/embeddings>.

use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::embedding::{Embedder, normalise};
use crate::error::{LlmError, LlmResult};
use crate::text::truncate_with_ellipsis;

/// Default Gemini API host.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Default text embedding model (Matryoshka-friendly 768-dim truncation).
pub const DEFAULT_MODEL: &str = "gemini-embedding-001";

/// Gemini / Google Generative Language embeddings.
pub struct GoogleEmbedder {
    client: reqwest::Client,
    api_key: SecretString,
    base_url: String,
    /// Wire model id, e.g. `models/gemini-embedding-001`.
    model: String,
    dim: u32,
    /// True when the model id contains `embedding-2` (task prefixes in text).
    embedding_v2: bool,
}

impl GoogleEmbedder {
    /// Construct a Google embedder.
    ///
    /// # Errors
    /// Propagates HTTP client construction errors.
    pub fn new(api_key: SecretString, model: impl Into<String>, dim: u32) -> LlmResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let model = normalize_model_id(model.into());
        let embedding_v2 = model.contains("embedding-2");
        Ok(Self {
            client,
            api_key,
            base_url: DEFAULT_BASE_URL.into(),
            model,
            dim,
            embedding_v2,
        })
    }

    /// Override API host (tests).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    async fn embed_with_task(
        &self,
        text: &str,
        task_type: Option<&'static str>,
    ) -> LlmResult<Vec<f32>> {
        let prepared = if self.embedding_v2 {
            match task_type {
                Some("RETRIEVAL_DOCUMENT") => format_document_v2(text),
                Some("RETRIEVAL_QUERY") => format_query_v2(text),
                _ => text.to_string(),
            }
        } else {
            text.to_string()
        };

        let url = embed_url(&self.base_url, &self.model);
        let body = GeminiEmbedRequest {
            content: GeminiContent {
                parts: vec![GeminiPart { text: &prepared }],
            },
            task_type: if self.embedding_v2 { None } else { task_type },
            output_dimensionality: Some(self.dim),
        };

        debug!(url, model = %self.model, ?task_type, "POST google/embedContent");
        let mut attempt = 0u32;
        loop {
            let resp = self
                .client
                .post(&url)
                .header("x-goog-api-key", self.api_key.expose_secret())
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 429 && attempt < 5 {
                attempt += 1;
                let delay = Duration::from_secs(2u64.saturating_pow(attempt));
                debug!(
                    attempt,
                    ?delay,
                    "google embedContent rate-limited; retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(LlmError::Provider {
                    status: status.as_u16(),
                    body: truncate_with_ellipsis(&body_text, 1024),
                });
            }
            let parsed: GeminiEmbedResponse = resp.json().await?;
            let values = parsed.embedding.values;
            if values.len() as u32 != self.dim {
                return Err(LlmError::UnexpectedShape(format!(
                    "expected dim {}, got {}",
                    self.dim,
                    values.len()
                )));
            }
            return Ok(normalise(values));
        }
    }
}

#[derive(Debug, Serialize)]
struct GeminiEmbedRequest<'a> {
    content: GeminiContent<'a>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "taskType")]
    task_type: Option<&'a str>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "outputDimensionality"
    )]
    output_dimensionality: Option<u32>,
}

#[derive(Debug, Serialize)]
struct GeminiContent<'a> {
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Debug, Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedResponse {
    embedding: GeminiEmbeddingValues,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbeddingValues {
    values: Vec<f32>,
}

#[async_trait]
impl Embedder for GoogleEmbedder {
    fn provider(&self) -> &'static str {
        "google"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    async fn embed(&self, text: &str) -> LlmResult<Vec<f32>> {
        self.embed_document(text).await
    }

    async fn embed_document(&self, text: &str) -> LlmResult<Vec<f32>> {
        self.embed_with_task(text, Some("RETRIEVAL_DOCUMENT")).await
    }

    async fn embed_query(&self, text: &str) -> LlmResult<Vec<f32>> {
        self.embed_with_task(text, Some("RETRIEVAL_QUERY")).await
    }
}

/// Prefix model id with `models/` when omitted.
#[must_use]
pub fn normalize_model_id(model: String) -> String {
    let trimmed = model.trim();
    if trimmed.starts_with("models/") {
        trimmed.to_string()
    } else {
        format!("models/{trimmed}")
    }
}

fn embed_url(base: &str, model: &str) -> String {
    format!(
        "{}/v1beta/{}:embedContent",
        base.trim_end_matches('/'),
        model
    )
}

/// Asymmetric document format for `gemini-embedding-2` (see Google docs).
#[must_use]
pub fn format_document_v2(text: &str) -> String {
    format!("title: none | text: {text}")
}

/// Asymmetric query format for `gemini-embedding-2`.
#[must_use]
pub fn format_query_v2(text: &str) -> String {
    format!("task: search result | query: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_model_adds_prefix() {
        assert_eq!(
            normalize_model_id("gemini-embedding-001".into()),
            "models/gemini-embedding-001"
        );
    }

    #[test]
    fn v2_document_and_query_prefixes() {
        assert!(format_document_v2("hello").contains("text: hello"));
        assert!(format_query_v2("find auth").contains("query: find auth"));
    }
}
