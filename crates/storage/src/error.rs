use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("Not found: {0}")]
    NotFound(String),
}
