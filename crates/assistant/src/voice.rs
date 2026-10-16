//! Voice transcription adapter.
//!
//! Telegram voice notes (OGG/Opus) are transcribed locally with whisper.cpp
//! (`WhisperCppTranscriber`): no audio leaves the server. The driver never sees
//! audio — the bridge transcribes, echoes the transcript to Alex, and runs a
//! normal text turn on it. `NoopTranscriber` stands in when no model is set up.

use async_trait::async_trait;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Semaphore;
use tracing::{info, warn};

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
    /// - [`AssistantError::Internal`] on actual transcription failures.
    async fn transcribe(&self, ogg_bytes: &[u8]) -> Result<String>;
}

/// Transcriber for deployments without a whisper model configured.
///
/// Always returns [`AssistantError::VoiceUnsupported`].
pub struct NoopTranscriber;

#[async_trait]
impl VoiceTranscriber for NoopTranscriber {
    async fn transcribe(&self, _ogg_bytes: &[u8]) -> Result<String> {
        Err(AssistantError::VoiceUnsupported)
    }
}

/// The transcriber this deployment's `[voice]` config describes: whisper.cpp when
/// a model file is present, otherwise the no-op stand-in.
pub fn from_config(cfg: &aust_core::config::VoiceConfig) -> std::sync::Arc<dyn VoiceTranscriber> {
    let model = PathBuf::from(cfg.model_path.trim());
    if cfg.model_path.trim().is_empty() || !model.is_file() {
        if !cfg.model_path.trim().is_empty() {
            warn!(model = %model.display(), "Voice model not found — voice notes disabled");
        }
        return std::sync::Arc::new(NoopTranscriber);
    }
    info!(model = %model.display(), "Voice notes enabled (whisper.cpp)");
    std::sync::Arc::new(WhisperCppTranscriber::new(WhisperSettings {
        bin: PathBuf::from(&cfg.whisper_bin),
        model,
        threads: cfg.threads,
        prompt: cfg.prompt.clone(),
        timeout: Duration::from_secs(cfg.timeout_secs),
    }))
}

/// Settings for [`WhisperCppTranscriber`].
#[derive(Debug, Clone)]
pub struct WhisperSettings {
    /// `whisper-cli` binary (whisper.cpp).
    pub bin: PathBuf,
    /// GGML model file, e.g. `ggml-small-q5_1.bin`.
    pub model: PathBuf,
    /// CPU threads for one transcription; 0 = all cores.
    pub threads: usize,
    /// Initial prompt biasing the decoder toward the domain vocabulary.
    pub prompt: String,
    /// Upper bound for one transcription before it is abandoned.
    pub timeout: Duration,
}

/// Local speech-to-text via whisper.cpp's `whisper-cli`.
///
/// One transcription at a time (a semaphore): the production VPS has a single
/// core shared with the API, Postgres and the mail poller, so parallel decodes
/// would only slow everything down together. The decode runs under `nice` so the
/// rest of the backend stays responsive while it works. Greedy decoding with
/// flash attention: on the benchmark clip it gave the same text as beam search
/// in two thirds of the time.
pub struct WhisperCppTranscriber {
    settings: WhisperSettings,
    slots: Semaphore,
}

impl WhisperCppTranscriber {
    pub fn new(settings: WhisperSettings) -> Self {
        Self { settings, slots: Semaphore::new(1) }
    }

    fn threads(&self) -> usize {
        match self.settings.threads {
            0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            n => n,
        }
    }
}

#[async_trait]
impl VoiceTranscriber for WhisperCppTranscriber {
    async fn transcribe(&self, ogg_bytes: &[u8]) -> Result<String> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|e| AssistantError::Internal(format!("Transkription nicht verfügbar: {e}")))?;
        let started = std::time::Instant::now();

        let dir = std::env::temp_dir().join(format!("aust-voice-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| AssistantError::Internal(format!("voice tmp dir: {e}")))?;
        let result = self.run(&dir, ogg_bytes).await;
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            warn!("voice tmp cleanup failed: {e}");
        }
        if let Ok(text) = &result {
            info!(ms = started.elapsed().as_millis() as u64, chars = text.len(), "Voice note transcribed");
        }
        result
    }
}

impl WhisperCppTranscriber {
    async fn run(&self, dir: &std::path::Path, ogg_bytes: &[u8]) -> Result<String> {
        let ogg = dir.join("in.ogg");
        let wav = dir.join("in.wav");
        tokio::fs::write(&ogg, ogg_bytes)
            .await
            .map_err(|e| AssistantError::Internal(format!("voice write: {e}")))?;

        // whisper.cpp wants 16 kHz mono PCM.
        let ffmpeg = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&ogg)
            .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| AssistantError::Internal(format!("ffmpeg nicht startbar: {e}")))?;
        if !ffmpeg.status.success() {
            return Err(AssistantError::Internal(format!(
                "ffmpeg: {}",
                String::from_utf8_lossy(&ffmpeg.stderr).trim()
            )));
        }

        let mut cmd = Command::new("nice");
        cmd.args(["-n", "10"])
            .arg(&self.settings.bin)
            .arg("-m")
            .arg(&self.settings.model)
            .args(["-l", "de", "-bs", "1", "-bo", "1", "-fa", "-nt", "-np"])
            .args(["-t", &self.threads().to_string()]);
        // Whisper always encodes a 30 s window; a short note padded to 30 s spends
        // most of the time on silence. Shrinking the window to the note's length
        // gave the same text ~2.4× faster on the 1-vCPU VPS (large-v3-turbo, 11 s
        // clip: 77 s → 32 s).
        if let Ok(meta) = tokio::fs::metadata(&wav).await
            && let Some(ctx) = audio_ctx_for(meta.len())
        {
            cmd.args(["-ac", &ctx.to_string()]);
        }
        if !self.settings.prompt.trim().is_empty() {
            cmd.args(["--prompt", self.settings.prompt.trim()]);
        }
        cmd.arg("-f").arg(&wav).stdin(Stdio::null()).kill_on_drop(true);

        let out = tokio::time::timeout(self.settings.timeout, cmd.output())
            .await
            .map_err(|_| AssistantError::Internal("Transkription hat zu lange gedauert".to_string()))?
            .map_err(|e| AssistantError::Internal(format!("whisper-cli nicht startbar: {e}")))?;
        if !out.status.success() {
            return Err(AssistantError::Internal(format!(
                "whisper-cli: {}",
                String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim()
            )));
        }
        let text = clean_transcript(&String::from_utf8_lossy(&out.stdout));
        if text.is_empty() {
            return Err(AssistantError::Internal("Keine Sprache erkannt".to_string()));
        }
        Ok(text)
    }
}

/// Encoder window (`-ac`) for a 16 kHz mono s16 WAV of `wav_bytes`: whisper's
/// full window is 1500 frames for 30 s (50 per second). Two seconds of margin,
/// never below 512 frames (at 384 turbo looped "Ask… Ask… Ask…" on a 4 s clip), `None`
/// when the note fills the whole window anyway.
pub(crate) fn audio_ctx_for(wav_bytes: u64) -> Option<u32> {
    const BYTES_PER_SEC: u64 = 16_000 * 2;
    let secs = wav_bytes.saturating_sub(44).div_ceil(BYTES_PER_SEC);
    let ctx = ((secs + 2) * 50).max(512);
    (ctx < 1500).then_some(ctx as u32)
}

/// Collapse whisper's line-per-segment output into one paragraph, drop the
/// bracketed non-speech markers it emits on silence ("[Musik]", "(Stille)") and
/// the quotes it sometimes wraps a whole note in.
pub(crate) fn clean_transcript(raw: &str) -> String {
    let text = raw
        .split_whitespace()
        .filter(|w| {
            !((w.starts_with('[') && w.ends_with(']')) || (w.starts_with('(') && w.ends_with(')')))
        })
        .collect::<Vec<_>>()
        .join(" ");
    for (open, close) in [('"', '"'), ('„', '“'), ('»', '«'), ('“', '”')] {
        if let Some(inner) = text.strip_prefix(open).and_then(|t| t.strip_suffix(close))
            && !inner.contains(open)
            && !inner.contains(close)
        {
            return inner.trim().to_string();
        }
    }
    text
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

    #[test]
    fn audio_ctx_follows_the_note_length() {
        let wav = |secs: u64| 44 + secs * 32_000;
        assert_eq!(audio_ctx_for(wav(11)), Some(650));
        assert_eq!(audio_ctx_for(wav(1)), Some(512), "floor against repetition loops");
        assert_eq!(audio_ctx_for(wav(28)), None, "full window");
        assert_eq!(audio_ctx_for(wav(120)), None);
    }

    #[test]
    fn transcript_is_one_clean_paragraph() {
        assert_eq!(
            clean_transcript(" Trag den Umzug von Frau Lange\n am Freitag ein. [Musik]\n"),
            "Trag den Umzug von Frau Lange am Freitag ein."
        );
        assert_eq!(clean_transcript("[BLANK_AUDIO]\n"), "");
        assert_eq!(
            clean_transcript("\"Kannst du kurz herausfinden, wie viele Rechnungen offen sind?\""),
            "Kannst du kurz herausfinden, wie viele Rechnungen offen sind?"
        );
        // A quote inside the note stays.
        assert_eq!(
            clean_transcript("\"Umzug\" ist eingetragen, schreib \"erledigt\""),
            "\"Umzug\" ist eingetragen, schreib \"erledigt\""
        );
    }

    /// Runs only where whisper.cpp and a model are installed:
    /// `AUST_TEST_WHISPER_BIN=… AUST_TEST_WHISPER_MODEL=… AUST_TEST_VOICE_OGG=…`.
    #[tokio::test]
    async fn transcribes_a_real_voice_note_when_whisper_is_installed() {
        let (Ok(bin), Ok(model), Ok(ogg)) = (
            std::env::var("AUST_TEST_WHISPER_BIN"),
            std::env::var("AUST_TEST_WHISPER_MODEL"),
            std::env::var("AUST_TEST_VOICE_OGG"),
        ) else {
            return;
        };
        let t = WhisperCppTranscriber::new(WhisperSettings {
            bin: bin.into(),
            model: model.into(),
            threads: 0,
            prompt: String::new(),
            timeout: Duration::from_secs(300),
        });
        let text = t.transcribe(&std::fs::read(ogg).unwrap()).await.unwrap();
        assert!(text.split_whitespace().count() > 5, "{text}");
    }
}
