//! `pending_memory_proposals` repository.
//!
//! Low-confidence memory proposals — from the post-action reflection hook
//! (confidence < 0.7) and the nightly consolidation pass (confidence < 0.8) —
//! are persisted here for batch approval by Alex via Telegram.
//!
//! Append-only: rows are never DELETEd. `approve` transactionally inserts a
//! row into `agent_memory` and flips status to `approved`. `reject` is a pure
//! status update.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use crate::memory::durable::MemoryKind;

/// Data required to enqueue a new pending memory proposal.
#[derive(Debug, Clone)]
pub struct NewProposal<'a> {
    pub session_id: Option<Uuid>,
    pub kind: MemoryKind,
    pub scope: &'a str,
    pub key: &'a str,
    pub value: Value,
    pub confidence: f32,
    pub source_episodes: Vec<Uuid>,
    pub rationale: Option<&'a str>,
}

/// A pending memory proposal row.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PendingProposal {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub proposed_at: DateTime<Utc>,
    pub kind: String,
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub confidence: f32,
    pub source_episodes: Vec<Uuid>,
    pub rationale: Option<String>,
    pub status: String,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<Uuid>,
    pub resolution_note: Option<String>,
}

/// Insert a new pending memory proposal. Returns the new row ID.
pub async fn enqueue(pool: &PgPool, proposal: NewProposal<'_>) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO pending_memory_proposals
            (id, session_id, kind, scope, key, value, confidence, source_episodes, rationale)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(id)
    .bind(proposal.session_id)
    .bind(proposal.kind.as_str())
    .bind(proposal.scope)
    .bind(proposal.key)
    .bind(&proposal.value)
    .bind(proposal.confidence)
    .bind(&proposal.source_episodes)
    .bind(proposal.rationale)
    .execute(pool)
    .await?;
    Ok(id)
}

/// List the most recent pending proposals (status = 'pending'), newest first.
pub async fn list_pending(pool: &PgPool, limit: u32) -> Result<Vec<PendingProposal>> {
    let limit_i = limit.min(500) as i64;
    let rows: Vec<PendingProposal> = sqlx::query_as(
        r#"
        SELECT id, session_id, proposed_at, kind, scope, key, value, confidence,
               source_episodes, rationale, status, resolved_at, resolved_by, resolution_note
        FROM pending_memory_proposals
        WHERE status = 'pending'
        ORDER BY proposed_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit_i)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// List proposals enqueued within the last `hours` hours, regardless of status.
pub async fn list_recent(pool: &PgPool, hours: i64) -> Result<Vec<PendingProposal>> {
    let rows: Vec<PendingProposal> = sqlx::query_as(
        r#"
        SELECT id, session_id, proposed_at, kind, scope, key, value, confidence,
               source_episodes, rationale, status, resolved_at, resolved_by, resolution_note
        FROM pending_memory_proposals
        WHERE proposed_at >= NOW() - ($1::TEXT || ' hours')::INTERVAL
        ORDER BY proposed_at DESC
        "#,
    )
    .bind(hours.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Approve a pending proposal: in one transaction, insert a row into
/// `agent_memory` and update the proposal status to `approved`.
///
/// If the proposal is not in `pending` status, returns `AssistantError::NotFound`
/// (idempotency: callers should not double-approve).
pub async fn approve(pool: &PgPool, id: Uuid, resolved_by: Uuid) -> Result<()> {
    let mut tx = pool.begin().await?;

    // Lock-and-fetch the pending row.
    let row: Option<(String, String, String, Value, f32)> = sqlx::query_as(
        r#"
        SELECT kind, scope, key, value, confidence
        FROM pending_memory_proposals
        WHERE id = $1 AND status = 'pending'
        FOR UPDATE
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let (kind, scope, key, value, confidence) = row.ok_or_else(|| {
        crate::error::AssistantError::NotFound(format!("pending proposal {id}"))
    })?;

    // Insert into agent_memory.
    let mem_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO agent_memory (id, kind, scope, key, value, source, confidence)
        VALUES ($1, $2, $3, $4, $5, 'proposal_approved', $6)
        "#,
    )
    .bind(mem_id)
    .bind(&kind)
    .bind(&scope)
    .bind(&key)
    .bind(&value)
    .bind(confidence as f64)
    .execute(&mut *tx)
    .await?;

    // Flip status.
    sqlx::query(
        r#"
        UPDATE pending_memory_proposals
        SET status = 'approved', resolved_at = NOW(), resolved_by = $2
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(resolved_by)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Reject a pending proposal. Status is set to `rejected` with optional note.
pub async fn reject(
    pool: &PgPool,
    id: Uuid,
    resolved_by: Uuid,
    note: Option<&str>,
) -> Result<()> {
    let affected = sqlx::query(
        r#"
        UPDATE pending_memory_proposals
        SET status = 'rejected', resolved_at = NOW(), resolved_by = $2, resolution_note = $3
        WHERE id = $1 AND status = 'pending'
        "#,
    )
    .bind(id)
    .bind(resolved_by)
    .bind(note)
    .execute(pool)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(crate::error::AssistantError::NotFound(format!(
            "pending proposal {id}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_proposal_builds_with_all_fields() {
        let p = NewProposal {
            session_id: Some(Uuid::nil()),
            kind: MemoryKind::Preference,
            scope: "global",
            key: "weekend_surcharge",
            value: json!(1.2),
            confidence: 0.55,
            source_episodes: vec![Uuid::nil()],
            rationale: Some("low confidence pattern"),
        };
        assert_eq!(p.scope, "global");
        assert!((p.confidence - 0.55).abs() < f32::EPSILON);
        assert_eq!(p.source_episodes.len(), 1);
    }

    #[test]
    fn pending_proposal_serde_roundtrip() {
        let p = PendingProposal {
            id: Uuid::nil(),
            session_id: None,
            proposed_at: Utc::now(),
            kind: "fact".to_string(),
            scope: "global".to_string(),
            key: "k".to_string(),
            value: json!("v"),
            confidence: 0.4,
            source_episodes: vec![],
            rationale: None,
            status: "pending".to_string(),
            resolved_at: None,
            resolved_by: None,
            resolution_note: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: PendingProposal = serde_json::from_str(&s).unwrap();
        assert_eq!(back.status, "pending");
        assert_eq!(back.kind, "fact");
    }
}

