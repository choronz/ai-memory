//! OpenAI Chat Completions client (with `response_format` JSON schema for
//! structured output).

use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{LlmError, LlmResult};
use crate::provider::LlmProvider;
use crate::types::{ChatRequest, ChatResponse, Role, Usage};

/// Default OpenAI API base.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// OpenAI Chat Completions-backed provider.
pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: SecretString,
    base_url: String,
    model: String,
}

impl OpenAiProvider {
    /// Construct a provider given an API key + model id.
    ///
    /// # Errors
    /// Returns a `reqwest::Error` if the HTTP client cannot be built.
    pub fn new(api_key: SecretString, model: impl Into<String>) -> LlmResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            client,
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
        })
    }

    /// Override the API base URL (tests; or pointing at an
    /// OpenAI-compatible mirror).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

#[derive(Debug, Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMsg<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OpenAiResponseFormat>,
}

#[derive(Debug, Serialize)]
struct OpenAiMsg<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiResponseFormat {
    JsonSchema { json_schema: OpenAiJsonSchema },
}

#[derive(Debug, Serialize)]
struct OpenAiJsonSchema {
    name: String,
    schema: serde_json::Value,
    strict: bool,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    model: String,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessageResponse,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessageResponse {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: ChatRequest) -> LlmResult<ChatResponse> {
        let response = self.post(&self.build_request(&request, None)).await?;
        Ok(self.to_chat_response(response))
    }

    async fn complete_structured_raw(
        &self,
        request: ChatRequest,
        schema: serde_json::Value,
    ) -> LlmResult<serde_json::Value> {
        let response_format = OpenAiResponseFormat::JsonSchema {
            json_schema: OpenAiJsonSchema {
                name: "Result".into(),
                schema,
                strict: true,
            },
        };
        let response = self
            .post(&self.build_request(&request, Some(response_format)))
            .await?;
        let text = response
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("");
        serde_json::from_str::<serde_json::Value>(text).map_err(LlmError::from)
    }
}

impl OpenAiProvider {
    fn build_request<'a>(
        &'a self,
        request: &'a ChatRequest,
        response_format: Option<OpenAiResponseFormat>,
    ) -> OpenAiRequest<'a> {
        let mut messages: Vec<OpenAiMsg<'a>> = Vec::new();
        if let Some(sys) = request.system.as_deref() {
            messages.push(OpenAiMsg {
                role: "system",
                content: sys,
            });
        }
        for m in &request.messages {
            messages.push(OpenAiMsg {
                role: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                content: &m.content,
            });
        }
        OpenAiRequest {
            model: &self.model,
            messages,
            max_tokens: Some(request.max_tokens),
            temperature: request.temperature,
            response_format,
        }
    }

    fn to_chat_response(&self, response: OpenAiResponse) -> ChatResponse {
        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();
        ChatResponse {
            text,
            usage: response.usage.map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            }),
            model: response.model,
        }
    }

    async fn post<B: Serialize>(&self, body: &B) -> LlmResult<OpenAiResponse> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        debug!(url, "POST openai");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(self.api_key.expose_secret())
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                body: truncate(&body, 1024),
            });
        }
        resp.json::<OpenAiResponse>().await.map_err(LlmError::from)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
