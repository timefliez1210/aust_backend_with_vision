pub mod error;
pub mod traits;

mod claude;
mod ollama;
mod openai;

pub use claude::ClaudeProvider;
pub use error::LlmError;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;
pub use traits::{LlmMessage, LlmProvider, LlmRole};

use aust_core::config::LlmConfig;
use std::sync::Arc;

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
            Ok(Arc::new(OllamaProvider::new(
                ollama_config.base_url.clone(),
                ollama_config.model.clone(),
            )))
        }
        provider => Err(LlmError::Configuration(format!(
            "Unknown provider: {provider}"
        ))),
    }
}
