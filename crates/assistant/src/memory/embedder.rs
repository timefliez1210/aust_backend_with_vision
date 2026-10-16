//! In-process sentence embeddings for episodic memory.
//!
//! Ollama Cloud has no embedding models, so episodes were never embedded (nor,
//! as it turned out, written). This runs a small multilingual model locally
//! (`paraphrase-multilingual-MiniLM-L12-v2`, quantized, 384 dims) through
//! fastembed/ONNX. The model costs ~0.8 GB RAM while loaded, on a 3.8 GB server,
//! so it loads on first use and is dropped again after `idle_unload_secs`.
//! Everything degrades to `None` — episodic recall then runs on full-text
//! search alone.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tracing::{info, warn};

/// Vector width of the local model (matches `agent_episodes.embedding_local`).
pub const DIM: usize = 384;

static GLOBAL: OnceLock<Option<Arc<LocalEmbedder>>> = OnceLock::new();

/// Set up the process-wide embedder from `[memory]`. Call once at startup,
/// inside the Tokio runtime (it starts the idle-unload task).
pub fn init(cfg: &aust_core::config::MemoryConfig) {
    let embedder = match cfg.embeddings.trim() {
        "local" => {
            let e = Arc::new(LocalEmbedder {
                cache_dir: PathBuf::from(&cfg.model_cache_dir),
                loaded: Mutex::new(None),
                idle: Duration::from_secs(cfg.idle_unload_secs.max(60)),
            });
            let reaper = e.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    reaper.unload_if_idle();
                }
            });
            info!(cache = %cfg.model_cache_dir, "Episodic memory: local embeddings enabled");
            Some(e)
        }
        _ => None,
    };
    let _ = GLOBAL.set(embedder);
}

/// Embed one text, or `None` when embeddings are off or the model failed.
pub async fn embed(text: &str) -> Option<Vec<f32>> {
    let embedder = GLOBAL.get()?.as_ref()?.clone();
    let text = text.to_string();
    match tokio::task::spawn_blocking(move || embedder.embed_blocking(&text)).await {
        Ok(Ok(v)) if v.len() == DIM => Some(v),
        Ok(Ok(v)) => {
            warn!(dim = v.len(), "Unexpected embedding width");
            None
        }
        Ok(Err(e)) => {
            warn!("Embedding failed: {e}");
            None
        }
        Err(e) => {
            warn!("Embedding task failed: {e}");
            None
        }
    }
}

/// A pgvector literal (`[0.1,0.2,…]`) for binding as `$n::vector`.
pub fn to_pgvector(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|f| f.to_string()).collect();
    format!("[{}]", parts.join(","))
}

struct Loaded {
    model: TextEmbedding,
    last_used: Instant,
}

pub struct LocalEmbedder {
    cache_dir: PathBuf,
    loaded: Mutex<Option<Loaded>>,
    idle: Duration,
}

impl LocalEmbedder {
    fn embed_blocking(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut guard = self.loaded.lock().map_err(|_| "embedder lock poisoned".to_string())?;
        if guard.is_none() {
            let started = Instant::now();
            let model = TextEmbedding::try_new(
                TextInitOptions::new(EmbeddingModel::ParaphraseMLMiniLML12V2Q)
                    .with_cache_dir(self.cache_dir.clone())
                    .with_show_download_progress(false),
            )
            .map_err(|e| format!("model load: {e}"))?;
            info!(ms = started.elapsed().as_millis() as u64, "Embedding model loaded");
            *guard = Some(Loaded { model, last_used: Instant::now() });
        }
        let loaded = guard.as_mut().ok_or_else(|| "embedder not loaded".to_string())?;
        loaded.last_used = Instant::now();
        let mut out = loaded
            .model
            .embed(vec![text], None)
            .map_err(|e| format!("embed: {e}"))?;
        out.pop().ok_or_else(|| "empty embedding batch".to_string())
    }

    fn unload_if_idle(&self) {
        if let Ok(mut guard) = self.loaded.lock()
            && guard.as_ref().is_some_and(|l| l.last_used.elapsed() > self.idle)
        {
            *guard = None;
            info!("Embedding model unloaded after idle period");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgvector_literal_shape() {
        assert_eq!(to_pgvector(&[0.5, -1.0]), "[0.5,-1]");
    }

    /// Runs the real model where it is cached: `AUST_TEST_EMBED_CACHE=<dir>`, plus
    /// `ORT_DYLIB_PATH=<libonnxruntime.so>` (ONNX Runtime >= 1.24, loaded at runtime).
    #[test]
    fn local_model_ranks_the_related_episode_first() {
        let Ok(dir) = std::env::var("AUST_TEST_EMBED_CACHE") else { return };
        let e = LocalEmbedder { cache_dir: dir.into(), loaded: Mutex::new(None), idle: Duration::from_secs(60) };
        let q = e.embed_blocking("Wann ist der Umzug von Frau Lange?").unwrap();
        let related = e.embed_blocking("Umzug Lange auf Freitag verschoben, Crew Tom und Lea").unwrap();
        let unrelated = e.embed_blocking("Rechnung 2026-41 an Müller bezahlt").unwrap();
        assert_eq!(q.len(), DIM);
        let cos = |a: &[f32], b: &[f32]| {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            dot / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
        };
        assert!(cos(&q, &related) > cos(&q, &unrelated) + 0.1);
        e.unload_if_idle();
        assert!(e.loaded.lock().unwrap().is_some(), "not idle yet → stays loaded");
    }

    #[tokio::test]
    async fn embed_is_none_when_not_initialised() {
        // GLOBAL may be unset in unit tests: embeddings simply absent.
        if GLOBAL.get().is_none() {
            assert!(embed("Umzug Lange").await.is_none());
        }
    }
}
