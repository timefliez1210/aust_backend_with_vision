//! Error types for the assistant crate.

use thiserror::Error;

/// All failure modes the assistant subsystem can produce.
#[derive(Debug, Error)]
pub enum AssistantError {
    /// Database query or connection failure.
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// LLM provider returned an error.
    #[error("LLM error: {0}")]
    Llm(#[from] aust_llm_providers::LlmError),

    /// The Telegram chat is not bound to any user.
    #[error("Unbound chat: {0}")]
    UnboundChat(i64),

    /// The user's role does not permit the requested operation.
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// A required resource was not found.
    #[error("Not found: {0}")]
    NotFound(String),

    /// Tool argument validation failed against the JSON schema.
    #[error("Argument validation failed for tool '{tool}': {message}")]
    ArgValidation { tool: String, message: String },

    /// The SOUL.md file is missing a required section.
    #[error("SOUL.md missing required section: '{0}'")]
    SoulMissingSection(String),

    /// SOUL.md file could not be loaded from disk.
    #[error("Failed to load SOUL.md: {0}")]
    SoulLoad(String),

    /// A pending action was not found or already resolved.
    #[error("Pending action not found or already resolved: {0}")]
    PendingActionNotFound(uuid::Uuid),

    /// Input validation error (e.g. chat mismatch, invalid state transition).
    #[error("Validation error: {0}")]
    Validation(String),

    /// Voice transcription is not yet supported (Phase 6).
    #[error("Voice transcription not yet supported (Phase 6)")]
    VoiceUnsupported,

    /// A capability is not yet wired into the assistant — used by Confirm
    /// tools whose backing service method requires plumbing (SMTP, S3) that
    /// the service trait does not yet expose. Tells Alex explicitly that the
    /// action did NOT happen, rather than the silent no-op the marker pattern
    /// previously caused.
    #[error("Aktion noch nicht final verdrahtet: {0}. Bitte vorerst über das Admin-Panel ausführen.")]
    NotWired(String),

    /// Generic internal error.
    #[error("Internal error: {0}")]
    Internal(String),

    /// Serialisation / deserialisation failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<aust_core::services::ServiceError> for AssistantError {
    fn from(e: aust_core::services::ServiceError) -> Self {
        use aust_core::services::ServiceError as S;
        match e {
            S::NotFound(msg) => AssistantError::NotFound(msg),
            S::Validation(msg) => AssistantError::ArgValidation {
                tool: "<service>".to_string(),
                message: msg,
            },
            S::Conflict(msg) => AssistantError::Internal(msg),
            S::Db(err) => AssistantError::Internal(format!("DB: {err}")),
            S::External(err) => AssistantError::Internal(format!("External: {err}")),
        }
    }
}

pub type Result<T> = std::result::Result<T, AssistantError>;
