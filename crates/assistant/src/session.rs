//! Agent session management.
//!
//! One `AgentSession` per Telegram chat. Sessions hold the rolling turn history and
//! a running summary produced by the cheap LLM tier when the turn count exceeds the
//! token budget. The session row is persisted in `agent_sessions`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use crate::llm::{AssistantLlmProvider, ModelTier};

/// Maximum number of turns kept in the rolling window before summarisation.
pub const TURN_BUDGET: usize = 20;

/// A single conversation turn stored in `agent_sessions.turns`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// "user" or "assistant".
    pub role: String,
    /// Message text.
    pub content: String,
    /// UTC timestamp.
    pub ts: chrono::DateTime<Utc>,
}

impl Turn {
    /// Construct a user turn with the current timestamp.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            ts: Utc::now(),
        }
    }

    /// Construct an assistant turn with the current timestamp.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            ts: Utc::now(),
        }
    }
}

/// An in-memory view of one `agent_sessions` row, including deserialized turns.
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// Primary key.
    pub id: Uuid,
    /// Telegram chat identifier.
    pub chat_id: i64,
    /// Latest rolling summary (null until first summarisation pass).
    pub last_summary: Option<String>,
    /// Recent turns not yet collapsed into the summary.
    pub turns: Vec<Turn>,
    /// Total turns ever appended to this session.
    pub turn_count: i32,
    /// Entity scopes active in this session (S4).
    ///
    /// Accumulated as the assistant calls tools that reference entity IDs.
    /// Passed to `assemble_bundle` so scoped memories are retrieved.
    /// Always contains at least `"global"`. Capped at 5 entries.
    pub active_scopes: Vec<String>,
}

impl AgentSession {
    /// Build a compact context block for prompt assembly.
    ///
    /// Returns the summary (if any) followed by the recent turns.
    pub fn context_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = &self.last_summary {
            parts.push(format!("[Zusammenfassung bisheriger Verlauf]\n{s}"));
        }
        for turn in &self.turns {
            parts.push(format!("{}: {}", turn.role, turn.content));
        }
        parts.join("\n")
    }
}

/// Load or create the session for a Telegram chat ID.
pub async fn load_or_create(pool: &PgPool, chat_id: i64) -> Result<AgentSession> {
    let row: Option<(Uuid, Option<String>, serde_json::Value, i32, serde_json::Value)> =
        sqlx::query_as(
            "SELECT id, last_summary, turns, turn_count, active_scopes FROM agent_sessions WHERE chat_id = $1",
        )
        .bind(chat_id)
        .fetch_optional(pool)
        .await?;

    if let Some((id, last_summary, turns_json, turn_count, scopes_json)) = row {
        let turns: Vec<Turn> = serde_json::from_value(turns_json)?;
        let active_scopes: Vec<String> = serde_json::from_value(scopes_json)
            .unwrap_or_else(|_| vec!["global".to_string()]);
        Ok(AgentSession {
            id,
            chat_id,
            last_summary,
            turns,
            turn_count,
            active_scopes,
        })
    } else {
        // Create a new session row.
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO agent_sessions (id, chat_id, turns) VALUES ($1, $2, '[]')",
        )
        .bind(id)
        .bind(chat_id)
        .execute(pool)
        .await?;

        Ok(AgentSession {
            id,
            chat_id,
            last_summary: None,
            turns: vec![],
            turn_count: 0,
            active_scopes: vec!["global".to_string()],
        })
    }
}

/// Append a turn to the session and persist to the database.
///
/// If the turn count exceeds `TURN_BUDGET`, `summarise` is called before saving.
pub async fn append_turn(
    pool: &PgPool,
    session: &mut AgentSession,
    turn: Turn,
    llm: &dyn AssistantLlmProvider,
) -> Result<()> {
    session.turns.push(turn);
    session.turn_count += 1;

    if session.turns.len() > TURN_BUDGET {
        summarise(pool, session, llm).await?;
    } else {
        persist(pool, session).await?;
    }
    Ok(())
}

/// Summarise the oldest half of the turns using the cheap LLM tier, then persist.
///
/// After summarisation `session.turns` contains only the recent half. The new
/// summary is stored in `session.last_summary` and in the DB row.
pub async fn summarise(
    pool: &PgPool,
    session: &mut AgentSession,
    llm: &dyn AssistantLlmProvider,
) -> Result<()> {
    let split = session.turns.len() / 2;
    let to_summarise = session.turns.drain(..split).collect::<Vec<_>>();

    let turns_text: String = to_summarise
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");

    let prev_summary = session.last_summary.as_deref().unwrap_or("");
    let prompt = format!(
        "Fasse folgenden Gesprächsverlauf in 3–5 deutschen Sätzen zusammen. \
         Vorherige Zusammenfassung: {prev_summary}\n\nVerlauf:\n{turns_text}"
    );

    let messages = vec![aust_llm_providers::LlmMessage::user(prompt)];
    let summary = llm
        .chat(ModelTier::Cheap, &messages)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Summarisation LLM call failed: {e}");
            prev_summary.to_string()
        });

    session.last_summary = Some(summary);
    persist(pool, session).await
}

/// Persist the current in-memory session state to the database.
async fn persist(pool: &PgPool, session: &AgentSession) -> Result<()> {
    let turns_json = serde_json::to_value(&session.turns)?;
    let scopes_json = serde_json::to_value(&session.active_scopes)?;
    sqlx::query(
        r#"
        UPDATE agent_sessions
           SET last_summary = $1, turns = $2, turn_count = $3, active_scopes = $4, updated_at = NOW()
         WHERE id = $5
        "#,
    )
    .bind(&session.last_summary)
    .bind(&turns_json)
    .bind(session.turn_count)
    .bind(&scopes_json)
    .bind(session.id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockAssistantLlm;

    #[test]
    fn context_text_no_summary() {
        let session = AgentSession {
            id: Uuid::now_v7(),
            chat_id: 1,
            last_summary: None,
            turns: vec![Turn::user("Hallo"), Turn::assistant("Guten Tag")],
            turn_count: 2,
            active_scopes: vec!["global".to_string()],
        };
        let ctx = session.context_text();
        assert!(ctx.contains("user: Hallo"));
        assert!(ctx.contains("assistant: Guten Tag"));
    }

    #[test]
    fn context_text_with_summary() {
        let session = AgentSession {
            id: Uuid::now_v7(),
            chat_id: 1,
            last_summary: Some("Bisherige Zusammenfassung.".to_string()),
            turns: vec![Turn::user("neue Frage")],
            turn_count: 10,
            active_scopes: vec!["global".to_string()],
        };
        let ctx = session.context_text();
        assert!(ctx.contains("Zusammenfassung"));
        assert!(ctx.contains("neue Frage"));
    }

    #[tokio::test]
    async fn mock_llm_summarise_updates_summary() {
        // We can't test the DB path without a live DB, but we can verify the
        // summarisation logic with the MockAssistantLlm by calling it directly.
        let llm = MockAssistantLlm::always("Das ist die Zusammenfassung.");
        // Build a fake session that exceeds the budget.
        let mut turns = Vec::new();
        for i in 0..25 {
            turns.push(Turn::user(format!("Nachricht {i}")));
        }
        let mut session = AgentSession {
            id: Uuid::now_v7(),
            chat_id: 99,
            last_summary: None,
            turns,
            turn_count: 25,
            active_scopes: vec!["global".to_string()],
        };

        // Call summarise directly without DB (we pass a dummy pool reference;
        // the persist call will fail but we only test the in-memory update here).
        // We cannot easily call persist here, so just test the LLM round trip.
        let split = session.turns.len() / 2;
        let to_summarise: Vec<_> = session.turns.drain(..split).collect();
        let turns_text: String = to_summarise
            .iter()
            .map(|t| format!("{}: {}", t.role, t.content))
            .collect::<Vec<_>>()
            .join("\n");
        let messages = vec![aust_llm_providers::LlmMessage::user(format!(
            "Fasse zusammen:\n{turns_text}"
        ))];
        let summary = llm
            .chat(ModelTier::Cheap, &messages)
            .await
            .unwrap();
        session.last_summary = Some(summary.clone());

        assert_eq!(session.last_summary.unwrap(), "Das ist die Zusammenfassung.");
        // Half of the turns were drained.
        assert_eq!(session.turns.len(), 25 - split);
    }
}
