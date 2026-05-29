//! Durable structured memory — append-only facts, preferences, rules, patterns.
//!
//! Facts are never DELETEd. Use `supersede` to replace a fact or `retire` to
//! withdraw it without a replacement. Both the old and new rows remain visible
//! for audit purposes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

/// The category of a durable memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Preference,
    Fact,
    Rule,
    Pattern,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Preference => "preference",
            MemoryKind::Fact => "fact",
            MemoryKind::Rule => "rule",
            MemoryKind::Pattern => "pattern",
        }
    }
}

impl std::fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A single durable memory row.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DurableMemory {
    pub id: Uuid,
    pub kind: String,
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub source: String,
    pub confidence: f64,
    pub superseded_by: Option<Uuid>,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl DurableMemory {
    /// Returns true if this memory is still active (not superseded or retired).
    pub fn is_active(&self) -> bool {
        self.superseded_by.is_none() && self.retired_at.is_none()
    }
}

/// Parameters for storing a new durable memory.
pub struct RememberParams<'a> {
    pub kind: MemoryKind,
    /// Scope string: "global", "customer:<uuid>", "employee:<uuid>", "inquiry:<uuid>".
    pub scope: &'a str,
    pub key: &'a str,
    pub value: Value,
    pub source: &'a str,
    /// Confidence in the range [0.0, 1.0].
    pub confidence: f64,
}

/// Insert a new durable memory row. Returns the new row ID.
pub async fn remember(pool: &PgPool, params: RememberParams<'_>) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO agent_memory (id, kind, scope, key, value, source, confidence)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(params.kind.as_str())
    .bind(params.scope)
    .bind(params.key)
    .bind(&params.value)
    .bind(params.source)
    .bind(params.confidence)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Fetch all active (non-superseded, non-retired) memories matching scope and kind filters.
///
/// Pass `None` to either filter to skip that constraint (i.e. "any scope" or "any kind").
pub async fn recall(
    pool: &PgPool,
    scope_filter: Option<&str>,
    kind_filter: Option<MemoryKind>,
) -> Result<Vec<DurableMemory>> {
    let rows: Vec<DurableMemory> = sqlx::query_as(
        r#"
        SELECT id, kind, scope, key, value, source, confidence,
               superseded_by, retired_at, created_at
        FROM agent_memory
        WHERE superseded_by IS NULL
          AND retired_at IS NULL
          AND ($1::TEXT IS NULL OR scope = $1)
          AND ($2::TEXT IS NULL OR kind = $2)
        ORDER BY created_at DESC
        "#,
    )
    .bind(scope_filter)
    .bind(kind_filter.map(|k| k.as_str().to_string()))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Supersede an existing memory with a new one. Both rows remain in the table.
///
/// 1. Inserts the new row.
/// 2. Sets `superseded_by` on the old row to point to the new one.
///
/// Returns the ID of the new row.
pub async fn supersede(
    pool: &PgPool,
    old_id: Uuid,
    params: RememberParams<'_>,
) -> Result<Uuid> {
    let mut tx = pool.begin().await?;

    let new_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO agent_memory (id, kind, scope, key, value, source, confidence)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(new_id)
    .bind(params.kind.as_str())
    .bind(params.scope)
    .bind(params.key)
    .bind(&params.value)
    .bind(params.source)
    .bind(params.confidence)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE agent_memory SET superseded_by = $1 WHERE id = $2")
        .bind(new_id)
        .bind(old_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(new_id)
}

/// Retire a memory without replacing it (sets `retired_at`).
///
/// Does NOT delete the row — it remains visible for audit purposes.
pub async fn retire(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE agent_memory SET retired_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
