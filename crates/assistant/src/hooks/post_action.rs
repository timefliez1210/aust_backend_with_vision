//! Post-action reflection hook.
//!
//! After every tool call that modifies state (Write / Confirm safety), this hook
//! calls the cheap LLM tier with the reflection prompt and parses a `MemoryProposal`.
//! If confidence >= 0.7 the memory is stored immediately; otherwise it is queued
//! for nightly consolidation review.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
#[allow(unused_imports)]
use tracing::{debug, warn};

use crate::error::Result;
use crate::llm::{AssistantLlmProvider, ModelTier};
use crate::memory::durable::MemoryKind;
use crate::memory::proposals::{self, NewProposal};
use aust_llm_providers::LlmMessage;

/// A memory proposal returned by the reflection LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryProposal {
    pub kind: String,
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub confidence: f64,
}

/// Context for the post-action reflection call.
pub struct PostActionCtx<'a> {
    /// Arguments as the LLM proposed them.
    pub proposed_args: &'a Value,
    /// Arguments that were actually executed (may differ if Alex edited).
    pub final_args: &'a Value,
    /// A short description of recent session context.
    pub recent_context: &'a str,
    /// Name of the tool that was executed.
    pub tool_name: &'a str,
    /// Session this reflection belongs to (for queueing proposals).
    pub session_id: Option<uuid::Uuid>,
}

/// Run the post-action reflection and store any high-confidence memory proposal.
///
/// Returns the proposal (if any was parsed) for the caller to log or display.
pub async fn reflect(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
    ctx: PostActionCtx<'_>,
) -> Result<Option<MemoryProposal>> {
    let session_id = ctx.session_id;
    let prompt = build_reflection_prompt(ctx);
    let messages = vec![LlmMessage::user(prompt)];

    let response = match llm.chat(ModelTier::Cheap, &messages).await {
        Ok(r) => r,
        Err(e) => {
            warn!("Reflection LLM call failed: {e}");
            return Ok(None);
        }
    };

    let proposal = parse_proposal(&response);

    if let Some(ref p) = proposal {
        debug!(
            confidence = p.confidence,
            key = p.key,
            "Reflection produced memory proposal"
        );

        // H4: route ALL reflections through `pending_memory_proposals` for Alex
        // to approve, regardless of confidence. Previously high-confidence
        // proposals were auto-stored via `durable::remember`, which bypassed
        // the Safety::Confirm gate that B6 put on the `remember` tool to block
        // prompt-injection planted durable rules. The LLM's self-reported
        // confidence is derived from attacker-influenceable content, so it is
        // not a safe authority for skipping confirmation.
        let kind = parse_kind(&p.kind);
        let rationale = if p.confidence >= 0.7 {
            "post_action_hook (high confidence — awaiting approval)"
        } else {
            "post_action_hook (low confidence)"
        };
        match proposals::enqueue(
            pool,
            NewProposal {
                session_id,
                kind,
                scope: &p.scope,
                key: &p.key,
                value: p.value.clone(),
                confidence: p.confidence as f32,
                source_episodes: vec![],
                rationale: Some(rationale),
            },
        )
        .await
        {
            Ok(_) => debug!(
                confidence = p.confidence,
                key = p.key,
                "Memory proposal queued for review (H4: no auto-store)"
            ),
            Err(e) => warn!("Failed to enqueue pending proposal: {e}"),
        }
    }

    Ok(proposal)
}

fn build_reflection_prompt(ctx: PostActionCtx<'_>) -> String {
    let edited = if ctx.proposed_args != ctx.final_args {
        format!(
            "\nVorgeschlagene Args: {}\nTatsächliche Args: {}",
            ctx.proposed_args, ctx.final_args
        )
    } else {
        String::new()
    };

    format!(
        "Du hast das Tool '{}' ausgeführt.{}\n\nKontext: {}\n\n\
         Gibt es ein Muster oder eine Präferenz, die Alex damit signalisiert hat?\n\
         Antworte mit JSON: {{\"proposal\": {{\"kind\": \"...\", \"scope\": \"...\", \
         \"key\": \"...\", \"value\": ..., \"confidence\": 0.0}} | null, \"reasoning\": \"...\"}}",
        ctx.tool_name, edited, ctx.recent_context
    )
}

/// Parse a `MemoryProposal` from the LLM response JSON.
///
/// Accepts responses where the proposal is nested under `"proposal"` key or
/// at the top level. Returns `None` if parsing fails or proposal is null.
fn parse_proposal(response: &str) -> Option<MemoryProposal> {
    // Try to find a JSON object in the response.
    let json_start = response.find('{')?;
    let json_str = &response[json_start..];
    let json: Value = serde_json::from_str(json_str).ok()?;

    // Try `{"proposal": {...}}` wrapper.
    let proposal_value = json.get("proposal").unwrap_or(&json);

    if proposal_value.is_null() {
        return None;
    }

    serde_json::from_value(proposal_value.clone()).ok()
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
    use serde_json::json;

    #[test]
    fn parses_proposal_from_wrapped_json() {
        let response = r#"{"proposal": {"kind": "preference", "scope": "global", "key": "weekend_surcharge", "value": 1.2, "confidence": 0.85}, "reasoning": "Alex always adds surcharge"}"#;
        let p = parse_proposal(response).unwrap();
        assert_eq!(p.kind, "preference");
        assert_eq!(p.key, "weekend_surcharge");
        assert!((p.confidence - 0.85).abs() < 0.001);
    }

    #[test]
    fn returns_none_for_null_proposal() {
        let response = r#"{"proposal": null, "reasoning": "nothing notable"}"#;
        assert!(parse_proposal(response).is_none());
    }

    #[test]
    fn returns_none_for_invalid_json() {
        assert!(parse_proposal("no json here").is_none());
    }

    #[tokio::test]
    async fn high_confidence_triggers_store_via_mock_llm() {
        use crate::llm::MockAssistantLlm;

        // Mock LLM returns a high-confidence proposal.
        let llm = MockAssistantLlm::always(
            r#"{"proposal": {"kind": "preference", "scope": "global", "key": "test_key", "value": "test_value", "confidence": 0.9}, "reasoning": "test"}"#,
        );

        // We can't test the DB side here without a live pool, but we can verify
        // that the LLM call + parsing path works end-to-end.
        let ctx_data = PostActionCtx {
            proposed_args: &json!({"x": 1}),
            final_args: &json!({"x": 1}),
            recent_context: "test context",
            tool_name: "test_tool",
            session_id: None,
        };

        let proposal_result = {
            let prompt = build_reflection_prompt(ctx_data);
            let messages = vec![LlmMessage::user(prompt)];
            let response = llm
                .chat(ModelTier::Cheap, &messages)
                .await
                .unwrap();
            parse_proposal(&response)
        };

        let proposal = proposal_result.unwrap();
        assert_eq!(proposal.confidence, 0.9);
        assert!(proposal.confidence >= 0.7, "should auto-store");
    }

    #[tokio::test]
    async fn low_confidence_does_not_panic() {
        use crate::llm::MockAssistantLlm;

        let llm = MockAssistantLlm::always(
            r#"{"proposal": {"kind": "fact", "scope": "global", "key": "low_conf", "value": "maybe", "confidence": 0.4}, "reasoning": "uncertain"}"#,
        );
        let ctx_data = PostActionCtx {
            proposed_args: &json!({}),
            final_args: &json!({"edited": true}),
            recent_context: "ctx",
            tool_name: "something",
            session_id: None,
        };
        let prompt = build_reflection_prompt(ctx_data);
        let messages = vec![LlmMessage::user(prompt)];
        let response = llm.chat(ModelTier::Cheap, &messages).await.unwrap();
        let proposal = parse_proposal(&response).unwrap();
        assert!(proposal.confidence < 0.7, "should queue, not auto-store");
    }
}
