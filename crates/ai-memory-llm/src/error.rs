//! LLM error type.

use thiserror::Error;

/// Result alias used throughout the LLM crate.
pub type LlmResult<T> = Result<T, LlmError>;

/// Errors raised by LLM providers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LlmError {
    /// Underlying HTTP failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    /// Provider returned a non-2xx status.
    #[error("provider error {status}: {body}")]
    Provider {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },

    /// JSON (de)serialization failure.
    #[error("serde: {0}")]
    Serde(String),

    /// Provider gave a response with unexpected shape (e.g. no tool
    /// use block where structured output was requested).
    #[error("unexpected response shape: {0}")]
    UnexpectedShape(String),

    /// Configured provider lacks the env var we need.
    #[error("provider not configured: {0}")]
    NotConfigured(String),

    /// Provider authentication failed or expired.
    #[error("auth: {0}")]
    Auth(String),

    /// JSON schema for structured output could not be derived.
    #[error("schema: {0}")]
    Schema(String),
}

impl From<serde_json::Error> for LlmError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

impl LlmError {
    /// True for errors where retrying the same request has a real chance of
    /// succeeding — i.e. the upstream was temporarily unavailable, rate-limited,
    /// or returned a corrupted/gateway response. Concretely: transport
    /// failures, 429, 5xx, an HTML/gateway error page served with a 2xx
    /// status, and a JSON parse failure (a truncated/corrupted body from a
    /// gateway that cut the stream under throttling). Permanent 4xx
    /// (401/403/404) and schema/usage errors are not retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            // Transport errors (timeout, connection reset, DNS) are transient.
            Self::Http(_) => true,
            // A truncated/corrupted JSON body (e.g. `expected `,` or `}` at
            // line N`) is a gateway that cut the stream under load, not a
            // permanent problem — retrying has a real chance of a full response.
            Self::Serde(_) => true,
            // 429 rate-limit and 5xx are explicitly retryable. We also treat a
            // `Provider` error whose body looks like an HTML/gateway page as
            // retryable even when it arrived with a 2xx status (gateways that
            // wrap throttling in a 200 HTML response).
            Self::Provider { status, body } => {
                if *status == 429 || (*status >= 500 && *status < 600) {
                    return true;
                }
                let trimmed = body.trim_start();
                trimmed.to_ascii_lowercase().starts_with("<!doctype")
                    || trimmed.starts_with("<html")
                    || trimmed.to_ascii_lowercase().contains("502")
                    || trimmed.to_ascii_lowercase().contains("bad gateway")
                    || trimmed.to_ascii_lowercase().contains("rate limit")
                    || trimmed.to_ascii_lowercase().contains("too many requests")
            }
            _ => false,
        }
    }
}
