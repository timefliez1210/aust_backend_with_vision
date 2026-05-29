//! Audit log writer.
//!
//! Every tool call — whether it succeeded or failed — must be recorded in
//! `agent_actions` before the turn is complete. This module provides a single
//! `record` function used by the driver loop and the tool executor.

use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

/// Parameters for one audit log entry.
pub struct AuditEntry<'a> {
    /// The session this action belongs to.
    pub session_id: Uuid,
    /// The name of the tool that was called.
    pub tool_name: &'a str,
    /// JSON-serialised arguments exactly as produced by the LLM.
    pub args: &'a Value,
    /// JSON-serialised result, if the tool succeeded.
    pub result: Option<&'a Value>,
    /// Error message string, if the tool failed.
    pub error_message: Option<&'a str>,
    /// Wall-clock milliseconds from dispatch to return.
    pub duration_ms: Option<i32>,
    /// Link to the `pending_actions` row when this required confirmation.
    pub confirmed_action_id: Option<Uuid>,
}

/// Insert one audit row for a tool call.
///
/// This is deliberately fire-and-forget with respect to the calling turn — a
/// failure to write the audit log is logged but does not abort the session.
pub async fn record(pool: &PgPool, entry: AuditEntry<'_>) -> Result<()> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO agent_actions
            (id, session_id, tool_name, args, result, error_message,
             duration_ms, confirmed_action_id, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(id)
    .bind(entry.session_id)
    .bind(entry.tool_name)
    .bind(entry.args)
    .bind(entry.result)
    .bind(entry.error_message)
    .bind(entry.duration_ms)
    .bind(entry.confirmed_action_id)
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(())
}
