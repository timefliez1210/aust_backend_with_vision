use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("API error: {0}")]
    Api(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}
