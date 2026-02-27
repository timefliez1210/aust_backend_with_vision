/// Pluggable LLM abstraction supporting Claude, OpenAI, and Ollama.
///
/// The primary entry point is [`create_provider`], which reads [`LlmConfig`]
/// and returns an `Arc<dyn LlmProvider>` ready for injection.
///
/// # Provider selection
/// Controlled by `LlmConfig::default_provider`:
/// - `"claude"` → [`ClaudeProvider`] (Anthropic Messages API; best for German)
/// - `"openai"` → [`OpenAiProvider`] (OpenAI Chat Completions; alternative)
/// - `"ollama"` → [`OllamaProvider`] (local self-hosted; privacy-focused)
///
/// # Key types
/// - [`LlmProvider`] — async trait with `complete` and `analyze_image` methods.
/// - [`LlmMessage`] / [`LlmRole`] — message envelope for multi-turn prompts.
/// - [`LlmError`] — all failure modes.
/// - [`MockLlmProvider`] — deterministic test double.
pub mod error;
pub mod mock;
pub mod traits;

mod claude;
mod ollama;
mod openai;

pub use claude::ClaudeProvider;
pub use error::LlmError;
pub use mock::MockLlmProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;
pub use traits::{LlmMessage, LlmProvider, LlmRole};

use aust_core::config::LlmConfig;
use std::sync::Arc;

/// Instantiate the configured LLM provider and return it as a trait object.
///
/// **Caller**: `src/main.rs` at startup; the returned `Arc` is cloned into
/// every service that needs LLM access (email agent, offer generator, volume
/// estimator).
///
/// # Parameters
/// - `config` — The `[llm]` section of the application config.
///
/// # Returns
/// An `Arc<dyn LlmProvider>` wrapping the selected backend, ready to share
/// across async tasks.
///
/// # Errors
/// - `LlmError::Configuration` when `default_provider` names an unknown backend,
///   or when the required sub-config (e.g., `claude.api_key`) is absent.
pub fn create_provider(config: &LlmConfig) -> Result<Arc<dyn LlmProvider>, LlmError> {
    match config.default_provider.as_str() {
        "claude" => {
            let claude_config = config
                .claude
                .as_ref()
                .ok_or_else(|| LlmError::Configuration("Claude config not found".into()))?;
            Ok(Arc::new(ClaudeProvider::new(
                claude_config.api_key.clone(),
                claude_config.model.clone(),
            )))
        }
        "openai" => {
            let openai_config = config
                .openai
                .as_ref()
                .ok_or_else(|| LlmError::Configuration("OpenAI config not found".into()))?;
            Ok(Arc::new(OpenAiProvider::new(
                openai_config.api_key.clone(),
                openai_config.model.clone(),
            )))
        }
        "ollama" => {
            let ollama_config = config
                .ollama
                .as_ref()
                .ok_or_else(|| LlmError::Configuration("Ollama config not found".into()))?;
            let provider = match &ollama_config.api_key {
                Some(key) if !key.is_empty() => OllamaProvider::with_api_key(
                    ollama_config.base_url.clone(),
                    ollama_config.model.clone(),
                    key.clone(),
                ),
                _ => OllamaProvider::new(
                    ollama_config.base_url.clone(),
                    ollama_config.model.clone(),
                ),
            };
            Ok(Arc::new(provider))
        }
        provider => Err(LlmError::Configuration(format!(
            "Unknown provider: {provider}"
        ))),
    }
}
