use thiserror::Error;

#[derive(Debug, Error)]
pub enum VolumeError {
    #[error("Vision analysis error: {0}")]
    Vision(String),

    #[error("Inventory processing error: {0}")]
    Inventory(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("External service error: {0}")]
    ExternalService(String),
}
