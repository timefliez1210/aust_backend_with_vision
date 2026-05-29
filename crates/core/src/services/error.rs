//! Service-layer error type shared between the service traits and their callers.

use thiserror::Error;

/// Errors that can be returned from any service trait implementation.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// The requested entity does not exist.
    #[error("Not found: {0}")]
    NotFound(String),

    /// The request is invalid (e.g. wrong status, missing field).
    #[error("Validation error: {0}")]
    Validation(String),

    /// A conflicting entity already exists.
    #[error("Conflict: {0}")]
    Conflict(String),

    /// An underlying database error occurred.
    #[error("Database error: {0}")]
    Db(#[source] anyhow::Error),

    /// An error from an external service (storage, LLM, ORS, …).
    #[error("External error: {0}")]
    External(#[source] anyhow::Error),
}

impl ServiceError {
    /// Convenience constructor for `NotFound`.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Convenience constructor for `Validation`.
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}

// Note: From<sqlx::Error> is NOT implemented here to keep `aust-core` free of
// database dependencies. Implement the conversion in the api crate's bridge impls.
