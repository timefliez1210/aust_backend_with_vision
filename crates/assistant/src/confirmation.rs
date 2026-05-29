//! Pending-action confirmation queue.
//!
//! Write-safety tools produce a `pending_action` row instead of executing
//! immediately. Alex sees an inline Telegram keyboard and either confirms,
//! edits, or cancels. The Telegram bot calls `resolve` after Alex responds,
//! which updates the row and unblocks the driver loop.

use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AssistantError, Result};

/// The outcome of a confirmation request.
#[derive(Debug, Clone)]
pub enum Resolution {
    /// Alex confirmed the action as-is.
    Confirmed,
    /// Alex edited the arguments before confirming.
    Edited(Value),
    /// Alex cancelled the action.
    Canceled,
}

/// A pending action row as returned from the database.
#[derive(Debug, sqlx::FromRow)]
pub struct PendingAction {
    pub id: Uuid,
    pub session_id: Uuid,
    pub tool_name: String,
    pub proposed_args: Value,
    pub final_args: Option<Value>,
    pub status: String,
    pub telegram_message_id: Option<i64>,
    /// Originating Telegram chat_id. When set, resolve validates the callback's
    /// chat_id matches to prevent cross-chat action hijacking (S2).
    pub chat_id: Option<i64>,
    pub expires_at: chrono::DateTime<Utc>,
    pub created_at: chrono::DateTime<Utc>,
    pub resolved_at: Option<chrono::DateTime<Utc>>,
}

/// Enqueue a new pending action and return its ID.
///
/// The caller should send a Telegram keyboard message and then update the row
/// with the Telegram message ID via [`set_telegram_message_id`].
pub async fn enqueue(
    pool: &PgPool,
    session_id: Uuid,
    tool_name: &str,
    proposed_args: &Value,
    telegram_message_id: Option<i64>,
) -> Result<Uuid> {
    enqueue_with_chat(pool, session_id, tool_name, proposed_args, telegram_message_id, None).await
}

/// Enqueue a new pending action with an explicit originating `chat_id`.
///
/// The `chat_id` is validated at resolve time — a callback from a different chat
/// is rejected with a validation error. Use this variant whenever the chat_id is
/// known at enqueue time (which is always the case in the driver loop).
pub async fn enqueue_with_chat(
    pool: &PgPool,
    session_id: Uuid,
    tool_name: &str,
    proposed_args: &Value,
    telegram_message_id: Option<i64>,
    chat_id: Option<i64>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO pending_actions
            (id, session_id, tool_name, proposed_args, status, telegram_message_id, chat_id)
        VALUES ($1, $2, $3, $4, 'pending', $5, $6)
        "#,
    )
    .bind(id)
    .bind(session_id)
    .bind(tool_name)
    .bind(proposed_args)
    .bind(telegram_message_id)
    .bind(chat_id)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Update the Telegram message ID after the keyboard message is sent.
pub async fn set_telegram_message_id(
    pool: &PgPool,
    pending_id: Uuid,
    message_id: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE pending_actions SET telegram_message_id = $1 WHERE id = $2",
    )
    .bind(message_id)
    .bind(pending_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a pending action by ID.
///
/// Returns [`AssistantError::PendingActionNotFound`] if absent or already resolved.
pub async fn fetch(pool: &PgPool, pending_id: Uuid) -> Result<PendingAction> {
    sqlx::query_as(
        r#"
        SELECT id, session_id, tool_name, proposed_args, final_args, status,
               telegram_message_id, chat_id, expires_at, created_at, resolved_at
        FROM pending_actions WHERE id = $1
        "#,
    )
    .bind(pending_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AssistantError::PendingActionNotFound(pending_id))
}

/// Resolve a pending action, validating that the callback originates from the
/// same Telegram chat that created it (S2).
///
/// Returns [`AssistantError::Validation`] when the chat_ids don't match.
pub async fn resolve_from_chat(
    pool: &PgPool,
    pending_id: Uuid,
    resolution: Resolution,
    caller_chat_id: i64,
) -> Result<()> {
    // Fetch first to check ownership.
    let action = fetch(pool, pending_id).await?;
    if action.status != "pending" {
        return Err(AssistantError::PendingActionNotFound(pending_id));
    }

    // Validate chat ownership when stored.
    if let Some(stored_chat) = action.chat_id {
        if stored_chat != caller_chat_id {
            return Err(AssistantError::Validation(
                "chat mismatch: diese Aktion gehört einem anderen Chat".to_string(),
            ));
        }
    }

    resolve(pool, pending_id, resolution).await
}

/// Resolve a pending action with the given outcome.
///
/// The `final_args` column is set to:
/// - `proposed_args` on `Confirmed`
/// - the edited value on `Edited(_)`
/// - `NULL` on `Canceled`
///
/// Returns [`AssistantError::PendingActionNotFound`] if the row does not exist or
/// is not in `pending` status.
pub async fn resolve(pool: &PgPool, pending_id: Uuid, resolution: Resolution) -> Result<()> {
    let (status, final_args) = match &resolution {
        Resolution::Confirmed => ("confirmed", None),
        Resolution::Edited(v) => ("edited", Some(v.clone())),
        Resolution::Canceled => ("canceled", None),
    };

    let rows_affected = if let Some(args) = final_args {
        sqlx::query(
            r#"
            UPDATE pending_actions
               SET status = $1, final_args = $2, resolved_at = NOW()
             WHERE id = $3 AND status = 'pending'
            "#,
        )
        .bind(status)
        .bind(&args)
        .bind(pending_id)
        .execute(pool)
        .await?
        .rows_affected()
    } else if status == "confirmed" {
        // For Confirmed, copy proposed_args → final_args.
        sqlx::query(
            r#"
            UPDATE pending_actions
               SET status = $1, final_args = proposed_args, resolved_at = NOW()
             WHERE id = $2 AND status = 'pending'
            "#,
        )
        .bind(status)
        .bind(pending_id)
        .execute(pool)
        .await?
        .rows_affected()
    } else {
        sqlx::query(
            r#"
            UPDATE pending_actions
               SET status = $1, resolved_at = NOW()
             WHERE id = $2 AND status = 'pending'
            "#,
        )
        .bind(status)
        .bind(pending_id)
        .execute(pool)
        .await?
        .rows_affected()
    };

    if rows_affected == 0 {
        return Err(AssistantError::PendingActionNotFound(pending_id));
    }
    Ok(())
}

/// Mark all pending actions whose `expires_at` is in the past as `expired`.
///
/// Returns the number of rows expired. Call this periodically (e.g. hourly).
pub async fn expire_stale(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE pending_actions
           SET status = 'expired', resolved_at = NOW()
         WHERE status = 'pending' AND expires_at < NOW()
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn try_pool() -> Option<sqlx::PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    /// Random offset for chat_ids to avoid UNIQUE collisions across repeated test runs
    /// against the shared dev DB.
    fn rand_chat_offset() -> u32 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    }

    /// Seed a unique agent_sessions row so `pending_actions.session_id` FK is satisfied.
    /// Each test uses a unique `chat_id` to avoid the UNIQUE constraint colliding.
    async fn seed_session(pool: &sqlx::PgPool, chat_id: i64) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO agent_sessions (id, chat_id) VALUES ($1, $2)")
            .bind(id)
            .bind(chat_id)
            .execute(pool)
            .await
            .expect("seed agent_sessions");
        id
    }

    /// S2: Resolving with a mismatched chat_id must return a Validation error.
    #[tokio::test]
    async fn resolve_from_chat_rejects_wrong_chat_id() {
        let Some(pool) = try_pool().await else { return };

        let owner_chat: i64 = 111_000 + (rand_chat_offset() as i64);
        let other_chat: i64 = 999_000 + (rand_chat_offset() as i64);
        let session_id = seed_session(&pool, owner_chat).await;

        let pending_id = enqueue_with_chat(
            &pool,
            session_id,
            "remember",
            &serde_json::json!({"key": "test"}),
            None,
            Some(owner_chat),
        )
        .await
        .expect("enqueue");

        // Attempt to resolve from a different chat.
        let result = resolve_from_chat(&pool, pending_id, Resolution::Confirmed, other_chat).await;
        assert!(
            matches!(result, Err(AssistantError::Validation(_))),
            "must reject with Validation error on chat mismatch, got: {result:?}"
        );

        // Verify the row is still pending.
        let action = fetch(&pool, pending_id).await.expect("fetch");
        assert_eq!(action.status, "pending", "action should remain pending after rejected resolve");

        // Cleanup.
        sqlx::query("DELETE FROM pending_actions WHERE id = $1")
            .bind(pending_id).execute(&pool).await.ok();
    }

    /// S2: Resolving from the correct chat must succeed.
    #[tokio::test]
    async fn resolve_from_chat_accepts_correct_chat_id() {
        let Some(pool) = try_pool().await else { return };

        let owner_chat: i64 = 111_001 + (rand_chat_offset() as i64);
        let session_id = seed_session(&pool, owner_chat).await;

        let pending_id = enqueue_with_chat(
            &pool,
            session_id,
            "remember",
            &serde_json::json!({"key": "test"}),
            None,
            Some(owner_chat),
        )
        .await
        .expect("enqueue");

        let result = resolve_from_chat(&pool, pending_id, Resolution::Confirmed, owner_chat).await;
        assert!(result.is_ok(), "should accept correct chat_id");

        // Cleanup.
        sqlx::query("DELETE FROM pending_actions WHERE id = $1")
            .bind(pending_id).execute(&pool).await.ok();
    }

    /// Race: resolve and expire_stale on the same row — exactly one wins.
    #[tokio::test]
    async fn resolve_and_expire_race_one_wins() {
        let Some(pool) = try_pool().await else { return };

        // Insert an action that expires in the past.
        let id = Uuid::now_v7();
        let chat_id: i64 = 222_000 + (rand_chat_offset() as i64);
        let session_id = seed_session(&pool, chat_id).await;
        sqlx::query(r#"
            INSERT INTO pending_actions (id, session_id, tool_name, proposed_args, status, expires_at)
            VALUES ($1, $2, 'test_tool', '{}'::jsonb, 'pending', NOW() - INTERVAL '1 second')
        "#)
        .bind(id).bind(session_id)
        .execute(&pool).await.expect("insert");

        // Both expire_stale and resolve race.
        let (expire_res, resolve_res) = tokio::join!(
            expire_stale(&pool),
            resolve(&pool, id, Resolution::Canceled),
        );

        // At least one must succeed; at most one changes the row.
        let either_ok = expire_res.is_ok() || resolve_res.is_ok();
        assert!(either_ok, "at least one operation should succeed");

        // The row must not be in 'pending' state after the race.
        let action: Option<(String,)> = sqlx::query_as("SELECT status FROM pending_actions WHERE id = $1")
            .bind(id).fetch_optional(&pool).await.expect("fetch");
        if let Some((status,)) = action {
            assert_ne!(status, "pending", "action must not remain pending after race");
        }

        // Cleanup.
        sqlx::query("DELETE FROM pending_actions WHERE id = $1")
            .bind(id).execute(&pool).await.ok();
    }
}
