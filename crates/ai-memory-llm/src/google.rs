//! Google Gemini Embeddings API (`embedContent`).
//!
//! See <https://ai.google.dev/gemini-api/docs/embeddings>.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::embedding::{Embedder, normalise};
use crate::error::{LlmError, LlmResult};
use crate::response::{provider_error_body, response_json_limited};

/// Default Gemini API host.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Default text embedding model (Matryoshka-friendly 768-dim truncation).
pub const DEFAULT_MODEL: &str = "gemini-embedding-001";

/// Gemini / Google Generative Language embeddings.
pub struct GoogleEmbedder {
    client: reqwest::Client,
    api_keys: Vec<SecretString>,
    /// Shared cursor so concurrent requests spread their starting key
    /// instead of stampeding a single key (cross-request round-robin).
    next_key: AtomicUsize,
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
        Self::new_with_keys(vec![api_key], model, dim)
    }

    /// Construct a Google embedder with multiple API keys for rotation.
    ///
    /// # Errors
    /// Propagates HTTP client construction errors.
    pub fn new_with_keys(
        api_keys: Vec<SecretString>,
        model: impl Into<String>,
        dim: u32,
    ) -> LlmResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let model = normalize_model_id(model.into());
        let embedding_v2 = model.contains("embedding-2");
        Ok(Self {
            client,
            api_keys,
            next_key: AtomicUsize::new(0),
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
        let len = self.api_keys.len();
        let max_attempts = std::cmp::max(5, len) as u32;
        let mut attempt = 0u32;
        // Spread the starting key across concurrent requests so the shared
        // embedder does not stampede a single key (cross-request round-robin).
        let start = if len == 0 {
            0
        } else {
            self.next_key.fetch_add(1, Ordering::Relaxed) % len
        };
        let mut key_idx = start;

        loop {
            let api_key = self.api_keys.get(key_idx).cloned();
            let Some(api_key) = api_key else {
                return Err(LlmError::Provider {
                    status: 500,
                    body: "no api keys configured".into(),
                });
            };

            let send_result = self
                .client
                .post(&url)
                .header("x-goog-api-key", api_key.expose_secret())
                .json(&body)
                .send()
                .await;

            let resp = match send_result {
                Ok(r) => r,
                Err(e) => {
                    // Transport-level failure (timeout, connection reset, DNS):
                    // fail over to the next key instead of giving up. No
                    // rate-limit backoff here — we just switch keys.
                    if attempt < max_attempts.saturating_sub(1) {
                        attempt += 1;
                        key_idx = (key_idx + 1) % len;
                        debug!(
                            attempt,
                            key_index = key_idx,
                            ?e,
                            "google transport error, failing over to next key"
                        );
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    }
                    return Err(LlmError::from(e));
                }
            };

            let status = resp.status();
            let is_retryable =
                status.as_u16() == 429 || (status.as_u16() >= 500 && status.as_u16() < 600);
            if is_retryable && attempt < max_attempts.saturating_sub(1) {
                attempt += 1;
                key_idx = (key_idx + 1) % len;
                let delay = Self::retry_delay(attempt, start);
                debug!(
                    attempt,
                    key_index = key_idx,
                    ?delay,
                    status = status.as_u16(),
                    "google embedContent key failed, rotating to next key"
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            if !status.is_success() {
                let body = provider_error_body(resp).await;
                return Err(LlmError::Provider {
                    status: status.as_u16(),
                    body,
                });
            }
            let parsed: GeminiEmbedResponse = response_json_limited(resp).await?;
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

    /// Exponential backoff (capped) with a small per-request jitter derived
    /// from the starting key index, so that concurrent requests desynchronise
    /// their retries (thundering-herd avoidance) without an RNG dependency.
    fn retry_delay(attempt: u32, start_key: usize) -> Duration {
        let base = 2u64.saturating_pow(attempt.min(4));
        let jitter_ms = (((start_key as u64).wrapping_add(attempt as u64)) * 7919) % 250 + 1;
        Duration::from_millis(base * 1000 + jitter_ms)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    #[derive(Clone)]
    struct AssertApiKeyHeader;

    impl Respond for AssertApiKeyHeader {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let api_key = req
                .headers
                .get("x-goog-api-key")
                .and_then(|value| value.to_str().ok());
            if api_key != Some("test-key") {
                return ResponseTemplate::new(500).set_body_string("missing x-goog-api-key header");
            }
            if req.headers.get("authorization").is_some() {
                return ResponseTemplate::new(500)
                    .set_body_string("unexpected authorization header");
            }
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embedding": { "values": [1.0, 0.0, 0.0] }
            }))
        }
    }

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

    #[tokio::test]
    async fn embed_content_uses_api_key_header_not_bearer_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(AssertApiKeyHeader)
            .mount(&server)
            .await;

        let embedder =
            GoogleEmbedder::new(SecretString::from("test-key"), "gemini-embedding-001", 3)
                .expect("google embedder builds")
                .with_base_url(server.uri());

        let embedding = embedder
            .embed_document("hello")
            .await
            .expect("embedContent request succeeds with API-key auth");

        assert_eq!(embedding, vec![1.0, 0.0, 0.0]);
    }

    /// Records the `x-goog-api-key` used per request and returns `fail_status`
    /// for the first `fail_for` requests, then a 200 with `success_body`.
    #[derive(Clone)]
    struct RecordingResponder {
        seen_keys: Arc<Mutex<Vec<String>>>,
        request_count: Arc<AtomicUsize>,
        fail_status: u16,
        fail_for: usize,
        success_body: String,
    }

    impl Respond for RecordingResponder {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let n = self.request_count.fetch_add(1, Ordering::SeqCst) + 1;
            let api_key = req
                .headers
                .get("x-goog-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            self.seen_keys.lock().unwrap().push(api_key);
            if n <= self.fail_for {
                return ResponseTemplate::new(self.fail_status);
            }
            ResponseTemplate::new(200).set_body_string(self.success_body.clone())
        }
    }

    fn embed_success_body() -> String {
        serde_json::json!({ "embedding": { "values": [1.0, 0.0, 0.0] } }).to_string()
    }

    #[tokio::test]
    async fn embed_rotates_to_next_key_on_429() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: seen.clone(),
                request_count: count.clone(),
                fail_status: 429,
                fail_for: 1,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0"), SecretString::from("k1")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let embedding = embedder
            .embed_document("hello")
            .await
            .expect("succeeds after rotating to the second key");

        assert_eq!(embedding, vec![1.0, 0.0, 0.0]);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["k0".to_string(), "k1".to_string()]
        );
    }

    #[tokio::test]
    async fn embed_retries_same_key_on_429_single_key() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: seen.clone(),
                request_count: count.clone(),
                fail_status: 429,
                fail_for: 1,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let embedding = embedder
            .embed_document("hello")
            .await
            .expect("succeeds after one retry on the same key");

        assert_eq!(embedding, vec![1.0, 0.0, 0.0]);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["k0".to_string(), "k0".to_string()]
        );
    }

    #[tokio::test]
    async fn embed_rotates_through_multiple_keys() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: seen.clone(),
                request_count: count.clone(),
                fail_status: 429,
                fail_for: 2,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![
                SecretString::from("k0"),
                SecretString::from("k1"),
                SecretString::from("k2"),
            ],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let embedding = embedder
            .embed_document("hello")
            .await
            .expect("succeeds on the third key");

        assert_eq!(embedding, vec![1.0, 0.0, 0.0]);
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["k0".to_string(), "k1".to_string(), "k2".to_string()]
        );
    }

    #[tokio::test]
    async fn embed_does_not_retry_on_non_retryable_status() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: Arc::new(Mutex::new(Vec::new())),
                request_count: count.clone(),
                fail_status: 400,
                fail_for: 999,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let err = embedder
            .embed_document("hello")
            .await
            .expect_err("400 is not retryable");
        assert!(matches!(err, LlmError::Provider { status: 400, .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn embed_no_keys_configured_errors() {
        let embedder = GoogleEmbedder::new_with_keys(vec![], "gemini-embedding-001", 3)
            .expect("google embedder builds");

        let err = embedder
            .embed_document("hello")
            .await
            .expect_err("empty key list must error before any request");
        match err {
            LlmError::Provider { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("no api keys"));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn embed_rotates_on_5xx() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: seen.clone(),
                request_count: count.clone(),
                fail_status: 503,
                fail_for: 1,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0"), SecretString::from("k1")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let embedding = embedder
            .embed_document("hello")
            .await
            .expect("succeeds after rotating past a 503");

        assert_eq!(embedding, vec![1.0, 0.0, 0.0]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["k0".to_string(), "k1".to_string()]
        );
    }

    #[tokio::test]
    async fn embed_all_keys_exhausted_returns_last_error() {
        let server = MockServer::start().await;
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: Arc::new(Mutex::new(Vec::new())),
                request_count: count.clone(),
                fail_status: 429,
                fail_for: 999,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0"), SecretString::from("k1")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        let err = embedder
            .embed_document("hello")
            .await
            .expect_err("all keys 429 must surface an error");
        assert!(matches!(err, LlmError::Provider { status: 429, .. }));
        // max_attempts = max(5, len) = 5 for 2 keys.
        assert_eq!(count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn embed_fails_over_on_transport_error() {
        // Both keys resolve to a dead port, so every send is a transport-level
        // connection error. The loop must retry across keys up to the cap and
        // surface the error as `LlmError::Http` (not succeed, not a `Provider`).
        // Bind a real listener we keep alive for the test so the port can't be
        // reclaimed by a concurrent test's ephemeral MockServer. An accept loop
        // that immediately drops each connection turns every embed request into
        // a deterministic transport-level failure (ECONNRESET), exercising the
        // key-rotation path without depending on OS socket-reuse timing.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let dead_uri = format!("http://{}", dead.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((stream, _)) = dead.accept().await {
                drop(stream);
            }
        });

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0"), SecretString::from("k1")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(dead_uri);

        let err = embedder
            .embed_document("hello")
            .await
            .expect_err("all keys hit a dead server");
        assert!(
            matches!(err, LlmError::Http(_)),
            "transport failure should surface as Http after exhausting keys, got {err:?}"
        );
    }

    #[tokio::test]
    async fn embed_spreads_starting_key_across_requests() {
        let server = MockServer::start().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-embedding-001:embedContent"))
            .respond_with(RecordingResponder {
                seen_keys: seen.clone(),
                request_count: count.clone(),
                fail_status: 429,
                fail_for: 0,
                success_body: embed_success_body(),
            })
            .mount(&server)
            .await;

        let embedder = GoogleEmbedder::new_with_keys(
            vec![SecretString::from("k0"), SecretString::from("k1")],
            "gemini-embedding-001",
            3,
        )
        .expect("google embedder builds")
        .with_base_url(server.uri());

        embedder
            .embed_document("one")
            .await
            .expect("first request succeeds");
        embedder
            .embed_document("two")
            .await
            .expect("second request succeeds");

        assert_eq!(
            *seen.lock().unwrap(),
            vec!["k0".to_string(), "k1".to_string()],
            "consecutive requests must start on rotating keys"
        );
    }
}
