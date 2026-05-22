//! Provider factory.
//!
//! Maps the user-visible `ProviderChoice` + env config into a
//! concrete `Arc<dyn LlmProvider>`.

use std::sync::Arc;

use secrecy::SecretString;

use crate::AnthropicProvider;
use crate::OpenAiCompatProvider;
use crate::OpenAiProvider;
use crate::error::{LlmError, LlmResult};
use crate::provider::LlmProvider;

/// Three providers ship in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions.
    OpenAi,
    /// OpenAI-compatible (Ollama / vLLM / LM Studio).
    OpenAiCompat,
}

/// All settings needed to construct one of the three providers.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider selection.
    pub provider: ProviderChoice,
    /// Model id (`claude-opus-4-7`, `gpt-4o-mini`, `llama3.1:8b`, …).
    pub model: String,
    /// API key. Required for Anthropic + OpenAI; optional for compat.
    pub api_key: Option<SecretString>,
    /// Base URL override (required for OpenAI-compat).
    pub base_url: Option<String>,
}

/// Construct an `Arc<dyn LlmProvider>` matching the config.
///
/// # Errors
/// Returns [`LlmError::NotConfigured`] if a required env value (API
/// key, base URL) is missing.
pub fn build_provider(config: ProviderConfig) -> LlmResult<Arc<dyn LlmProvider>> {
    match config.provider {
        ProviderChoice::Anthropic => {
            let key = config
                .api_key
                .ok_or_else(|| LlmError::NotConfigured("ANTHROPIC_API_KEY".into()))?;
            Ok(Arc::new(AnthropicProvider::new(key, config.model)?))
        }
        ProviderChoice::OpenAi => {
            let key = config
                .api_key
                .ok_or_else(|| LlmError::NotConfigured("OPENAI_API_KEY".into()))?;
            Ok(Arc::new(OpenAiProvider::new(key, config.model)?))
        }
        ProviderChoice::OpenAiCompat => {
            let base = config
                .base_url
                .ok_or_else(|| LlmError::NotConfigured("LLM_BASE_URL".into()))?;
            Ok(Arc::new(OpenAiCompatProvider::new(
                base,
                config.api_key,
                config.model,
            )?))
        }
    }
}
