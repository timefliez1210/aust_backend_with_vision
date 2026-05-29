//! Memory bundle assembler.
//!
//! `assemble_bundle` pulls from all three memory layers — global durable memories,
//! scope-specific durable memories, and semantically similar episodes — and caps the
//! result at a token budget so the combined context stays within the LLM's window.

use sqlx::PgPool;
use tracing::debug;

use super::durable::{self, DurableMemory};
use super::episodic::{self, Episode};
use crate::error::Result;
use crate::llm::AssistantLlmProvider;

/// Token-budget constants.  Rough approximation: 1 token ≈ 4 characters.
const GLOBAL_MEMORY_BUDGET: usize = 2_000;  // chars
const SCOPED_MEMORY_BUDGET: usize = 3_000;
const EPISODE_BUDGET: usize = 2_000;
const MAX_EPISODES: i64 = 10;

/// The assembled memory context passed to prompt assembly.
#[derive(Debug, Default)]
pub struct MemoryBundle {
    /// Active global durable memories (preferences, rules).
    pub global_memories: Vec<DurableMemory>,
    /// Active scope-specific durable memories.
    pub scoped_memories: Vec<DurableMemory>,
    /// Relevant recent episodes.
    pub episodes: Vec<Episode>,
    /// List of what was loaded (for tracing / debug).
    pub load_log: Vec<String>,
}

impl MemoryBundle {
    /// Render the bundle as a compact text block for prompt injection.
    pub fn as_context_text(&self) -> String {
        let mut parts = Vec::new();

        if !self.global_memories.is_empty() {
            let items: Vec<String> = self
                .global_memories
                .iter()
                .map(|m| format!("[{}] {}: {}", m.kind, m.key, m.value))
                .collect();
            parts.push(format!("## Globale Erinnerungen\n{}", items.join("\n")));
        }

        if !self.scoped_memories.is_empty() {
            let items: Vec<String> = self
                .scoped_memories
                .iter()
                .map(|m| format!("[{}] {} / {}: {}", m.kind, m.scope, m.key, m.value))
                .collect();
            parts.push(format!("## Kontext-Erinnerungen\n{}", items.join("\n")));
        }

        if !self.episodes.is_empty() {
            let items: Vec<String> = self
                .episodes
                .iter()
                .map(|e| format!("- {} ({})", e.summary, e.created_at.format("%d.%m.%Y %H:%M")))
                .collect();
            parts.push(format!("## Ähnliche frühere Ereignisse\n{}", items.join("\n")));
        }

        parts.join("\n\n")
    }
}

/// Assemble a memory bundle for a given session context.
///
/// # Parameters
/// - `pool`           — DB connection pool.
/// - `llm`            — LLM provider for generating the query embedding.
/// - `query_text`     — The current user message, used for episode similarity search.
/// - `active_scopes`  — Scope strings that should be included (e.g. `["customer:abc"]`).
///   Global scope is always included.
pub async fn assemble_bundle(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
    query_text: &str,
    active_scopes: &[&str],
) -> Result<MemoryBundle> {
    let mut bundle = MemoryBundle::default();

    // 1. Global durable memories (always included, budget-capped).
    let globals = durable::recall(pool, Some("global"), None).await?;
    let mut char_count = 0usize;
    for mem in globals {
        let len = mem.key.len() + mem.value.to_string().len();
        if char_count + len > GLOBAL_MEMORY_BUDGET {
            break;
        }
        char_count += len;
        bundle.global_memories.push(mem);
    }
    bundle.load_log.push(format!(
        "global_memories: {} loaded",
        bundle.global_memories.len()
    ));
    debug!(global_count = bundle.global_memories.len(), "Loaded global memories");

    // 2. Scope-specific memories.
    let mut scoped_char_count = 0usize;
    for scope in active_scopes {
        if *scope == "global" {
            continue; // Already loaded.
        }
        let scoped = durable::recall(pool, Some(scope), None).await?;
        for mem in scoped {
            let len = mem.key.len() + mem.value.to_string().len();
            if scoped_char_count + len > SCOPED_MEMORY_BUDGET {
                break;
            }
            scoped_char_count += len;
            bundle.scoped_memories.push(mem);
        }
    }
    bundle.load_log.push(format!(
        "scoped_memories: {} loaded for {} scopes",
        bundle.scoped_memories.len(),
        active_scopes.len()
    ));
    debug!(
        scoped_count = bundle.scoped_memories.len(),
        "Loaded scoped memories"
    );

    // 3. Episodic memories — similar to the current query.
    let episodes = episodic::retrieve_similar(pool, llm, query_text, MAX_EPISODES)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Episode retrieval failed, falling back to recent: {e}");
            vec![]
        });

    let mut ep_char_count = 0usize;
    for ep in episodes {
        let len = ep.summary.len();
        if ep_char_count + len > EPISODE_BUDGET {
            break;
        }
        ep_char_count += len;
        bundle.episodes.push(ep);
    }
    bundle
        .load_log
        .push(format!("episodes: {} loaded", bundle.episodes.len()));
    debug!(episode_count = bundle.episodes.len(), "Loaded episodes");

    Ok(bundle)
}
