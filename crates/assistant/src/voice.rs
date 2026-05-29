//! Voice transcription adapter.
//!
//! # Phase 6 plan
//! In Phase 6 a real `WhisperTranscriber` will be wired here, calling either
//! a local Whisper endpoint or the OpenAI Whisper API. The `VoiceTranscriber`
//! trait is defined now so the session input normalization path can accept
//! audio without breaking the API. Until Phase 6 the only implementation is
//! `NoopTranscriber` which returns `Err(VoiceUnsupported)`.
//!
//! Integration point: the driver loop calls `transcriber.transcribe(ogg_bytes)`
//! when it receives a Telegram voice message update and feeds the result into
//! the normal text input path.

use async_trait::async_trait;

use crate::error::{AssistantError, Result};

/// Converts raw audio bytes into a transcribed text string.
///
/// Implementations are expected to be `Send + Sync` so they can be stored in
/// shared application state alongside the LLM and DB pool.
#[async_trait]
pub trait VoiceTranscriber: Send + Sync {
    /// Transcribe an OGG/Opus audio buffer (typical Telegram voice message format).
    ///
    /// # Errors
    /// - [`AssistantError::VoiceUnsupported`] when the implementation is a no-op stub.
    /// - [`AssistantError::Internal`] on actual transcription failures (Phase 6).
    async fn transcribe(&self, ogg_bytes: &[u8]) -> Result<String>;
}

/// No-op transcriber used until Phase 6 wires a real speech-to-text backend.
///
/// Always returns [`AssistantError::VoiceUnsupported`].
pub struct NoopTranscriber;

#[async_trait]
impl VoiceTranscriber for NoopTranscriber {
    async fn transcribe(&self, _ogg_bytes: &[u8]) -> Result<String> {
        Err(AssistantError::VoiceUnsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_returns_unsupported() {
        let t = NoopTranscriber;
        let err = t.transcribe(b"fake audio").await.unwrap_err();
        assert!(matches!(err, AssistantError::VoiceUnsupported));
    }
}
