//! Nightly consolidation job.
//!
//! Scans the last 24 hours of episodes and low-confidence memory proposals,
//! groups them by tag (simple clustering — TODO: replace with embedding-based
//! clustering in a later phase), calls the cheap LLM to produce candidate
//! memories, and surfaces them to Alex via Telegram for approval.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tracing::{debug, info};
use aust_llm_providers::LlmMessage;

use crate::error::Result;
use crate::llm::{AssistantLlmProvider, ModelTier};
use crate::memory::durable::{self, MemoryKind, RememberParams};
use crate::memory::episodic;
use crate::memory::proposals::{self, NewProposal, PendingProposal};

/// A candidate memory produced by the consolidation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMemory {
    pub kind: String,
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub confidence: f64,
    pub evidence_count: u32,
}

/// Result of the nightly consolidation run.
#[derive(Debug, Default)]
pub struct ConsolidationResult {
    pub candidates_produced: usize,
    pub candidates_stored: usize,
    pub candidates_queued: usize,
    pub episodes_processed: usize,
    /// Pending proposals enqueued within the last 24h that should be included
    /// in Alex's morning briefing for approval.
    pub pending_for_briefing: Vec<PendingProposal>,
}

/// Entry point for the nightly consolidation job.
///
/// Call this from a scheduled task (e.g. tokio-cron-scheduler or a cron container).
/// Does not block the main event loop — all work is async.
pub async fn run_consolidation(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
) -> Result<ConsolidationResult> {
    let mut result = ConsolidationResult::default();

    // Fetch episodes from the last 24 hours.
    let recent_episodes = episodic::fetch_recent(pool, 50).await?;
    let last_24h: Vec<_> = recent_episodes
        .iter()
        .filter(|e| {
            let age = Utc::now() - e.created_at;
            age.num_hours() < 24
        })
        .collect();

    result.episodes_processed = last_24h.len();
    info!(
        episode_count = last_24h.len(),
        "Consolidation: processing recent episodes"
    );

    if last_24h.is_empty() {
        return Ok(result);
    }

    // Simple clustering: group by first tag.
    // TODO(phase4): Replace with embedding-based clustering (k-means or hierarchical).
    let mut groups: std::collections::HashMap<String, Vec<&episodic::Episode>> =
        std::collections::HashMap::new();
    for ep in &last_24h {
        let tag = ep.tags.first().cloned().unwrap_or_else(|| "untagged".to_string());
        groups.entry(tag).or_default().push(ep);
    }

    debug!(group_count = groups.len(), "Consolidation: grouped episodes");

    // For each group, call the cheap LLM to produce candidate memories.
    let mut all_candidates: Vec<CandidateMemory> = Vec::new();
    for (tag, episodes) in &groups {
        let summaries: Vec<&str> = episodes.iter().map(|e| e.summary.as_str()).collect();
        let candidates = call_consolidation_llm(llm, tag, &summaries).await;
        all_candidates.extend(candidates);
    }

    result.candidates_produced = all_candidates.len();

    // Auto-store candidates with confidence >= 0.8.
    // Lower-confidence candidates are queued for Alex's approval.
    // TODO(phase3): Surface low-confidence candidates via Telegram keyboard.
    for candidate in &all_candidates {
        if candidate.confidence >= 0.8 {
            let kind = parse_kind(&candidate.kind);
            match durable::remember(
                pool,
                RememberParams {
                    kind,
                    scope: &candidate.scope,
                    key: &candidate.key,
                    value: candidate.value.clone(),
                    source: "consolidation_nightly",
                    confidence: candidate.confidence,
                },
            )
            .await
            {
                Ok(_) => result.candidates_stored += 1,
                Err(e) => tracing::warn!("Failed to store consolidated memory: {e}"),
            }
        } else {
            // Persist low-confidence candidates for batch approval.
            // TODO(phase-3): wire approval keyboard via Telegram.
            let kind = parse_kind(&candidate.kind);
            match proposals::enqueue(
                pool,
                NewProposal {
                    session_id: None,
                    kind,
                    scope: &candidate.scope,
                    key: &candidate.key,
                    value: candidate.value.clone(),
                    confidence: candidate.confidence as f32,
                    source_episodes: vec![],
                    rationale: Some("consolidation_nightly (low confidence)"),
                },
            )
            .await
            {
                Ok(_) => {
                    result.candidates_queued += 1;
                    debug!(
                        key = candidate.key,
                        confidence = candidate.confidence,
                        "Low-confidence candidate queued for approval"
                    );
                }
                Err(e) => tracing::warn!("Failed to enqueue pending proposal: {e}"),
            }
        }
    }

    // Pull recently-enqueued pending proposals for the morning briefing.
    match proposals::list_recent(pool, 24).await {
        Ok(rows) => {
            result.pending_for_briefing = rows
                .into_iter()
                .filter(|r| r.status == "pending")
                .collect();
        }
        Err(e) => tracing::warn!("Failed to list recent pending proposals: {e}"),
    }

    info!(
        produced = result.candidates_produced,
        stored = result.candidates_stored,
        "Consolidation complete"
    );
    Ok(result)
}

async fn call_consolidation_llm(
    llm: &dyn AssistantLlmProvider,
    tag: &str,
    summaries: &[&str],
) -> Vec<CandidateMemory> {
    let summaries_text = summaries.join("\n- ");
    let prompt = format!(
        "Analysiere folgende Ereignisse der letzten 24 Stunden (Gruppe: '{tag}'):\n\
         - {summaries_text}\n\n\
         Gibt es wiederkehrende Muster, die als Erinnerung gespeichert werden sollten?\n\
         Antworte mit JSON-Array: [\
         {{\"kind\": \"...\", \"scope\": \"...\", \"key\": \"...\", \"value\": ..., \
         \"confidence\": 0.0, \"evidence_count\": 0}}, ...\
         ]. Leeres Array wenn keine Muster erkennbar."
    );

    let messages = vec![LlmMessage::user(prompt)];
    let response = match llm.chat(ModelTier::Cheap, &messages).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Consolidation LLM call failed for tag '{tag}': {e}");
            return vec![];
        }
    };

    parse_candidates(&response)
}

fn parse_candidates(response: &str) -> Vec<CandidateMemory> {
    let json_start = match response.find('[') {
        Some(i) => i,
        None => return vec![],
    };
    let json_str = &response[json_start..];
    serde_json::from_str(json_str).unwrap_or_default()
}

fn parse_kind(s: &str) -> MemoryKind {
    match s {
        "preference" => MemoryKind::Preference,
        "fact" => MemoryKind::Fact,
        "rule" => MemoryKind::Rule,
        "pattern" => MemoryKind::Pattern,
        _ => MemoryKind::Fact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockAssistantLlm;

    #[test]
    fn parse_candidates_from_valid_json() {
        let json = r#"[{"kind":"preference","scope":"global","key":"weekend_surcharge","value":1.2,"confidence":0.85,"evidence_count":3}]"#;
        let candidates = parse_candidates(json);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].key, "weekend_surcharge");
    }

    #[test]
    fn parse_candidates_handles_empty_array() {
        let candidates = parse_candidates("[]");
        assert!(candidates.is_empty());
    }

    #[test]
    fn parse_candidates_handles_invalid_json() {
        let candidates = parse_candidates("not json");
        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn consolidation_llm_call_parses_mock_response() {
        let llm = MockAssistantLlm::always(
            r#"[{"kind":"rule","scope":"global","key":"min_notice_days","value":2,"confidence":0.9,"evidence_count":5}]"#,
        );
        let candidates = call_consolidation_llm(&llm, "scheduling", &["Episode 1", "Episode 2"]).await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, "rule");
        assert!(candidates[0].confidence >= 0.8);
    }
}
