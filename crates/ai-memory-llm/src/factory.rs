//! Provider factory.
//!
//! Maps the user-visible `ProviderChoice` + env config into a
//! concrete `Arc<dyn LlmProvider>`.

use std::sync::Arc;

use secrecy::SecretString;

use crate::AnthropicProvider;
use crate::CopilotProvider;
use crate::GeminiProvider;
use crate::OpenAiCompatProvider;
use crate::OpenAiOAuthProvider;
use crate::OpenAiProvider;
use crate::OpenCodeProvider;

/// Resolve the per-provider concurrency cap (max in-flight requests) for the
/// OpenAI-family providers. Precedence: explicit `config` override (TOML
/// `llm_max_concurrency`) > `AI_MEMORY_LLM_MAX_CONCURRENCY` env > provider
/// default (3). A value of `0` disables the limiter. The cap prevents a
/// burst of consolidation / embedding calls from tripping gateway throttling
/// ("too many calls").
fn resolve_max_concurrency(config: Option<usize>) -> usize {
    if let Some(n) = config {
        return n;
    }
    match std::env::var("AI_MEMORY_LLM_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        Some(0) => 0,
        Some(n) => n,
        None => crate::openai::DEFAULT_MAX_CONCURRENCY,
    }
}
use crate::auth::{AuthRequirement, ProviderAuth};
use crate::embedding::{Embedder, OpenAiEmbedder, VoyageEmbedder};
use crate::error::{LlmError, LlmResult};
use crate::google::GoogleEmbedder;
use crate::provider::LlmProvider;

/// LLM providers available to ai-memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChoice {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions.
    OpenAi,
    /// Google Gemini (Generative Language API).
    Gemini,
    /// OpenAI-compatible (Ollama / vLLM / LM Studio).
    OpenAiCompat,
    /// OpenAI ChatGPT/Codex OAuth backend.
    OpenAiOAuth,
    /// GitHub Copilot Chat backend.
    Copilot,
    /// Anthropic Messages API via a Claude-subscription OAuth token.
    AnthropicOAuth,
    /// OpenCode Zen/Go cloud API (OpenAI-compatible endpoint).
    OpenCode,
}

impl ProviderChoice {
    /// Wire-format provider name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::OpenAiCompat => "openai-compat",
            Self::OpenAiOAuth => "openai-oauth",
            Self::Copilot => "copilot",
            Self::AnthropicOAuth => "anthropic-oauth",
            Self::OpenCode => "opencode",
        }
    }

    /// Auth requirement for this provider.
    #[must_use]
    pub const fn auth_requirement(self) -> AuthRequirement {
        match self {
            Self::Anthropic => AuthRequirement::RequiredApiKey {
                env_var: "ANTHROPIC_API_KEY",
            },
            Self::OpenAi => AuthRequirement::RequiredApiKey {
                env_var: "OPENAI_API_KEY",
            },
            Self::Gemini => AuthRequirement::RequiredApiKey {
                env_var: "GEMINI_API_KEY",
            },
            Self::OpenAiCompat => AuthRequirement::OptionalApiKey {
                env_var: "LLM_API_KEY",
            },
            Self::OpenAiOAuth => AuthRequirement::OpenAiOAuthToken,
            Self::Copilot => AuthRequirement::CopilotToken,
            Self::AnthropicOAuth => AuthRequirement::AnthropicOAuthToken,
            Self::OpenCode => AuthRequirement::RequiredApiKey {
                env_var: "OPENCODE_API_KEY",
            },
        }
    }
}

/// All settings needed to construct one LLM provider instance.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider selection.
    pub provider: ProviderChoice,
    /// Model id (`claude-opus-4-7`, `gpt-4o-mini`, `llama3.1:8b`, …).
    pub model: String,
    /// Resolved provider authentication material.
    pub auth: ProviderAuth,
    /// Base URL override (required for OpenAI-compat).
    pub base_url: Option<String>,
    /// Opt-in strict mode for the `openai-compat` provider: send
    /// `response_format=json_schema` instead of the tolerant prose-JSON
    /// parser. Ignored by every other provider. Sourced once from
    /// `AI_MEMORY_LLM_COMPAT_STRICT` by `Config::load`.
    pub compat_strict: bool,
    /// Optional additional API keys for rotation (OpenAI / OpenCode).
    /// Mirrors the Gemini/`api_keys` path: when set, the provider rotates
    /// across keys on 429/5xx with a per-key cooldown. Empty for single-key
    /// or key-less providers.
    pub api_keys: Vec<SecretString>,
    /// Optional cap on concurrent in-flight requests (0 disables the limiter).
    /// `None` falls back to the provider default / `AI_MEMORY_LLM_MAX_CONCURRENCY`.
    pub max_concurrency: Option<usize>,
}

/// Embedding providers available to ai-memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderChoice {
    /// OpenAI Embeddings API.
    OpenAi,
    /// Voyage Embeddings API.
    Voyage,
    /// Google Gemini Embeddings API (`embedContent`).
    Google,
}

impl EmbedderChoice {
    /// Wire-format provider name; matches what the `Embedder::provider`
    /// implementations return so the refuse-on-mismatch query lines up.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Voyage => "voyage",
            Self::Google => "google",
        }
    }
}

/// Settings to build an embedder.
#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    /// Provider selection.
    pub provider: EmbedderChoice,
    /// Model id (e.g. `text-embedding-3-small`).
    pub model: String,
    /// Vector dimensionality. Refused on mismatch with the stored
    /// pages' dim.
    pub dim: u32,
    /// API key (single).
    pub api_key: SecretString,
    /// Optional base URL override.
    pub base_url: Option<String>,
    /// Optional additional API keys for rotation (Google/Gemini).
    pub api_keys: Vec<SecretString>,
}

/// Construct an `Arc<dyn Embedder>` from the config.
///
/// # Errors
/// Propagates HTTP-client construction errors.
pub fn build_embedder(config: EmbedderConfig) -> LlmResult<Arc<dyn Embedder>> {
    let arc: Arc<dyn Embedder> = match config.provider {
        EmbedderChoice::OpenAi => {
            let mut e = OpenAiEmbedder::new(config.api_key, config.model, config.dim)?;
            if let Some(url) = config.base_url {
                e = e.with_base_url(url);
            }
            Arc::new(e)
        }
        EmbedderChoice::Voyage => {
            let mut e = VoyageEmbedder::new(config.api_key, config.model, config.dim)?;
            if let Some(url) = config.base_url {
                e = e.with_base_url(url);
            }
            Arc::new(e)
        }
        EmbedderChoice::Google => {
            let keys = if config.api_keys.is_empty() {
                vec![config.api_key]
            } else {
                let mut merged = config.api_keys;
                // Ensure at least one key
                if merged.is_empty() {
                    merged.push(config.api_key);
                }
                merged
            };
            let mut e = GoogleEmbedder::new_with_keys(keys, config.model, config.dim)?;
            if let Some(url) = config.base_url {
                e = e.with_base_url(url);
            }
            Arc::new(e)
        }
    };
    Ok(arc)
}

/// Default dim for known embedding models. Used when the operator
/// omits `AI_MEMORY_EMBEDDING_DIM`. Falls back to a model-family
/// default; unknown models still require an explicit dim.
#[must_use]
pub fn default_embedding_dim(provider: EmbedderChoice, model: &str) -> u32 {
    match (provider, model) {
        (EmbedderChoice::OpenAi, "text-embedding-3-small") => 1536,
        (EmbedderChoice::OpenAi, "text-embedding-3-large") => 3072,
        (EmbedderChoice::OpenAi, _) => 1536,
        (EmbedderChoice::Voyage, "voyage-3-large") => 1024,
        (EmbedderChoice::Voyage, _) => 1024,
        (EmbedderChoice::Google, "gemini-embedding-2") => 768,
        (EmbedderChoice::Google, "gemini-embedding-001") => 768,
        (EmbedderChoice::Google, _) => 768,
    }
}

/// Construct an `Arc<dyn LlmProvider>` matching the config.
///
/// # Errors
/// Returns [`LlmError::NotConfigured`] if a required env value (API
/// key, base URL) is missing.
pub fn build_provider(config: ProviderConfig) -> LlmResult<Arc<dyn LlmProvider>> {
    match config.provider {
        ProviderChoice::Anthropic => {
            let key = config.auth.require_api_key()?;
            Ok(Arc::new(AnthropicProvider::new(key, config.model)?))
        }
        ProviderChoice::OpenAi => {
            let keys = config.auth.api_keys();
            let keys = if keys.is_empty() {
                vec![config.auth.require_api_key()?]
            } else {
                keys
            };
            Ok(Arc::new(
                OpenAiProvider::new_with_keys(keys, config.model)?
                    .with_concurrency(resolve_max_concurrency(config.max_concurrency)),
            ))
        }
        ProviderChoice::Gemini => {
            let keys = config.auth.api_keys();
            let keys = if keys.is_empty() {
                vec![config.auth.require_api_key()?]
            } else {
                keys
            };
            Ok(Arc::new(GeminiProvider::new_with_keys(keys, config.model)?))
        }
        ProviderChoice::OpenAiCompat => {
            let base = config
                .base_url
                .ok_or_else(|| LlmError::NotConfigured("LLM_BASE_URL".into()))?;
            Ok(Arc::new(
                OpenAiCompatProvider::new(base, config.auth.api_keys(), config.model)?
                    .with_strict(config.compat_strict)
                    .with_concurrency(resolve_max_concurrency(config.max_concurrency)),
            ))
        }
        ProviderChoice::OpenAiOAuth => {
            let path = config.auth.require_openai_oauth_token_file()?.to_path_buf();
            Ok(Arc::new(OpenAiOAuthProvider::new(path, config.model)?))
        }
        ProviderChoice::Copilot => {
            let auth = config.auth.require_copilot_auth()?;
            Ok(Arc::new(CopilotProvider::new(auth, config.model)?))
        }
        ProviderChoice::AnthropicOAuth => {
            let token = config.auth.require_anthropic_oauth_token()?;
            let mut provider = AnthropicProvider::new_oauth(token, config.model)?;
            if let Some(url) = config.base_url {
                provider = provider.with_base_url(url);
            }
            Ok(Arc::new(provider))
        }
        ProviderChoice::OpenCode => {
            let keys = config.auth.api_keys();
            let keys = if keys.is_empty() {
                vec![config.auth.require_api_key()?]
            } else {
                keys
            };
            Ok(Arc::new(
                OpenCodeProvider::new(keys, config.model)?
                    .with_concurrency(resolve_max_concurrency(config.max_concurrency)),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_choices_declare_current_auth_requirements() {
        assert_eq!(
            ProviderChoice::Anthropic.auth_requirement(),
            AuthRequirement::RequiredApiKey {
                env_var: "ANTHROPIC_API_KEY"
            }
        );
        assert_eq!(
            ProviderChoice::OpenAi.auth_requirement(),
            AuthRequirement::RequiredApiKey {
                env_var: "OPENAI_API_KEY"
            }
        );
        assert_eq!(
            ProviderChoice::Gemini.auth_requirement(),
            AuthRequirement::RequiredApiKey {
                env_var: "GEMINI_API_KEY"
            }
        );
        assert_eq!(
            ProviderChoice::OpenAiCompat.auth_requirement(),
            AuthRequirement::OptionalApiKey {
                env_var: "LLM_API_KEY"
            }
        );
        assert_eq!(
            ProviderChoice::OpenAiOAuth.auth_requirement(),
            AuthRequirement::OpenAiOAuthToken
        );
        assert_eq!(
            ProviderChoice::Copilot.auth_requirement(),
            AuthRequirement::CopilotToken
        );
        assert_eq!(
            ProviderChoice::AnthropicOAuth.auth_requirement(),
            AuthRequirement::AnthropicOAuthToken
        );
    }

    #[test]
    fn missing_required_provider_auth_preserves_error_shape() {
        let cfg = ProviderConfig {
            provider: ProviderChoice::OpenAi,
            model: "gpt-4o-mini".into(),
            auth: ProviderAuth::required_api_key_from_env("OPENAI_API_KEY", None),
            base_url: None,
            compat_strict: false,
            api_keys: Vec::new(),
            max_concurrency: None,
        };

        let err = match build_provider(cfg) {
            Ok(_) => panic!("provider should fail without OPENAI_API_KEY"),
            Err(err) => err,
        };
        assert!(matches!(err, LlmError::NotConfigured(msg) if msg == "OPENAI_API_KEY"));
    }
}
