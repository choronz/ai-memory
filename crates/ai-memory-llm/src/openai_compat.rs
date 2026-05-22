//! OpenAI-compatible client (Ollama, vLLM, LM Studio, llama.cpp).
//!
//! Uses the same wire format as [`crate::openai::OpenAiProvider`] but
//! with a configurable base URL (and no key required for most local
//! deployments). Structured output falls back to "parse first JSON
//! object out of the text" because most local engines lack reliable
//! `response_format` honour.

use async_trait::async_trait;
use secrecy::SecretString;
use tracing::debug;

use crate::error::{LlmError, LlmResult};
use crate::openai::OpenAiProvider;
use crate::provider::LlmProvider;
use crate::types::{ChatRequest, ChatResponse};

/// OpenAI-compatible provider, parameterised by base URL.
pub struct OpenAiCompatProvider {
    inner: OpenAiProvider,
    name_tag: &'static str,
}

impl OpenAiCompatProvider {
    /// Construct a provider pointed at `base_url` (`LLM_BASE_URL` or
    /// `OLLAMA_HOST`). API key is optional; many local engines accept
    /// any non-empty string.
    ///
    /// # Errors
    /// Returns a `reqwest::Error` if the HTTP client cannot be built.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<SecretString>,
        model: impl Into<String>,
    ) -> LlmResult<Self> {
        let key = api_key.unwrap_or_else(|| SecretString::from("dummy"));
        let inner = OpenAiProvider::new(key, model)?.with_base_url(base_url);
        Ok(Self {
            inner,
            name_tag: "openai-compat",
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    fn name(&self) -> &'static str {
        self.name_tag
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn complete(&self, request: ChatRequest) -> LlmResult<ChatResponse> {
        self.inner.complete(request).await
    }

    async fn complete_structured_raw(
        &self,
        request: ChatRequest,
        _schema: serde_json::Value,
    ) -> LlmResult<serde_json::Value> {
        // Most local engines don't honour `response_format`. We
        // ask the model to emit a JSON object and fall back to
        // extracting the first balanced `{…}` from the text.
        let res = self.inner.complete(request).await?;
        match serde_json::from_str::<serde_json::Value>(&res.text) {
            Ok(v) if v.is_object() => Ok(v),
            _ => {
                let Some(slice) = first_json_object(&res.text) else {
                    debug!(text = %truncate(&res.text, 200), "no balanced JSON object found");
                    return Err(LlmError::UnexpectedShape(
                        "openai-compat response did not contain a JSON object".into(),
                    ));
                };
                serde_json::from_str::<serde_json::Value>(slice).map_err(LlmError::from)
            }
        }
    }
}

fn first_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0_i32;
    let bytes = s.as_bytes();
    for (i, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_json_object_finds_balanced_object() {
        assert_eq!(first_json_object("noise {\"k\":1} more"), Some("{\"k\":1}"));
        assert_eq!(
            first_json_object("text {\"a\":{\"b\":2}} trailing"),
            Some("{\"a\":{\"b\":2}}"),
        );
        assert_eq!(first_json_object("no json here"), None);
    }
}
