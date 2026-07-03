//! Retention / GC sweepers for assistant-owned tables.
//!
//! Each sweeper is independent and returns `(deleted, summarized)` counts so
//! the caller can log a tidy report.  Call [`run_retention_pass`] from a
//! periodic background task (default interval: 6 hours).
//!
//! # Policy summary
//! | Table              | Policy                                                        |
//! |--------------------|---------------------------------------------------------------|
//! | agent_actions      | DELETE rows > 4 days old; preserve errors & confirmed actions |
//! | pending_actions    | DELETE resolved/expired rows > 4 days old                    |
//! | agent_sessions     | NULL out `turns` for sessions inactive > 30 days             |
//! | agent_episodes     | NULL out `embedding` for rows > 180 days without keep tag    |
//! | domain_events      | Archive assistant-consumed events > 30 days old              |
//! | agent_memory       | Never touched (kept forever)                                  |

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A single row from `agent_actions` sufficient for summarisation.
#[derive(Debug, sqlx::FromRow)]
struct ActionRow {
    id: Uuid,
    session_id: Uuid,
    tool_name: String,
    error_message: Option<String>,
    confirmed_action_id: Option<Uuid>,
}

/// A pending_action row fetched just for its German summary text.
#[derive(Debug, sqlx::FromRow)]
struct PendingActionSummary {
    tool_name: String,
    status: String,
    /// Not a real DB column — derived in query via coalesce on proposed_args/final_args.
    proposed_args: Value,
}

/// Aggregated counts from one full retention pass.
#[derive(Debug, Default)]
pub struct RetentionReport {
    pub actions_deleted: u64,
    pub actions_summarized: u64,
    pub pending_deleted: u64,
    pub sessions_cleared: u64,
    pub episodes_embedding_dropped: u64,
    pub domain_events_archived: u64,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run all five sweepers and return aggregated counts.
///
/// Failures in individual sweepers are logged and counted as zero so that one
/// table's problem does not block the others.
pub async fn run_retention_pass(pool: &PgPool) -> RetentionReport {
    let now = Utc::now();
    let mut report = RetentionReport::default();

    match sweep_agent_actions(pool, now).await {
        Ok((deleted, summarized)) => {
            report.actions_deleted = deleted;
            report.actions_summarized = summarized;
        }
        Err(e) => tracing::error!("sweep_agent_actions failed: {e}"),
    }

    match sweep_pending_actions(pool, now).await {
        Ok(deleted) => report.pending_deleted = deleted,
        Err(e) => tracing::error!("sweep_pending_actions failed: {e}"),
    }

    match sweep_agent_sessions(pool, now).await {
        Ok(cleared) => report.sessions_cleared = cleared,
        Err(e) => tracing::error!("sweep_agent_sessions failed: {e}"),
    }

    match sweep_agent_episodes(pool, now).await {
        Ok(dropped) => report.episodes_embedding_dropped = dropped,
        Err(e) => tracing::error!("sweep_agent_episodes failed: {e}"),
    }

    match sweep_domain_events(pool, now).await {
        Ok(archived) => report.domain_events_archived = archived,
        Err(e) => tracing::error!("sweep_domain_events failed: {e}"),
    }

    tracing::info!(
        actions_deleted = report.actions_deleted,
        actions_summarized = report.actions_summarized,
        pending_deleted = report.pending_deleted,
        sessions_cleared = report.sessions_cleared,
        episodes_embedding_dropped = report.episodes_embedding_dropped,
        domain_events_archived = report.domain_events_archived,
        "Retention pass complete"
    );

    report
}

// ── agent_actions ─────────────────────────────────────────────────────────────

/// Delete `agent_actions` rows older than 4 days.
///
/// Before deletion, any row with a non-null `error_message` or a non-null
/// `confirmed_action_id` is preserved as a `retention_summary` episode — unless
/// it is a plain Read-tool success with no error and no confirmation (noise).
///
/// Returns `(deleted_count, summarized_count)`.
pub async fn sweep_agent_actions(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<(u64, u64)> {
    let cutoff = now - chrono::Duration::days(4);

    // Fetch rows that need summarisation before deletion.
    let important: Vec<ActionRow> = sqlx::query_as(
        r#"
        SELECT id, session_id, tool_name, error_message, confirmed_action_id
        FROM agent_actions
        WHERE created_at < $1
          AND (error_message IS NOT NULL OR confirmed_action_id IS NOT NULL)
        "#,
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    let mut summarized: u64 = 0;
    for row in &important {
        match summarize_important_action(pool, row).await {
            Ok(()) => summarized += 1,
            Err(e) => tracing::warn!(
                action_id = %row.id,
                tool_name = %row.tool_name,
                "Failed to write retention_summary episode: {e}"
            ),
        }
    }

    // Now delete all rows older than the cutoff.
    let result = sqlx::query(
        "DELETE FROM agent_actions WHERE created_at < $1",
    )
    .bind(cutoff)
    .execute(pool)
    .await?;

    Ok((result.rows_affected(), summarized))
}

/// Write a one-line German `retention_summary` episode for an important action.
///
/// The summary is templated — no LLM call.
async fn summarize_important_action(pool: &PgPool, action: &ActionRow) -> Result<()> {
    let summary = if let Some(ref msg) = action.error_message {
        // Tool failed.
        let short = msg.chars().take(120).collect::<String>();
        format!("❌ Tool {}: {}", action.tool_name, short)
    } else if let Some(confirmed_id) = action.confirmed_action_id {
        // Alex-confirmed action — try to fetch the pending_action's tool_name and status.
        let pa: Option<PendingActionSummary> = sqlx::query_as(
            r#"
            SELECT tool_name, status, proposed_args
            FROM pending_actions
            WHERE id = $1
            "#,
        )
        .bind(confirmed_id)
        .fetch_optional(pool)
        .await?;

        match pa {
            Some(pa) => {
                // Extract a short description from the pending action.
                let args_hint = args_short_hint(&pa.proposed_args);
                format!(
                    "✅ {} ({}): {}",
                    pa.tool_name,
                    pa.status,
                    args_hint
                )
            }
            None => {
                // pending_action already deleted — use tool_name from the action row.
                format!("✅ {} (bestätigt)", action.tool_name)
            }
        }
    } else {
        // Should not happen given the WHERE clause; skip gracefully.
        tracing::warn!(action_id = %action.id, "summarize_important_action: no error and no confirmed_action_id — skipping");
        return Ok(());
    };

    let episode_id = Uuid::now_v7();
    let tags: Vec<String> = vec![
        action.tool_name.clone(),
        "retention_summary".to_string(),
        "preserved".to_string(),
    ];
    let refs = serde_json::json!({
        "session_id": action.session_id,
        "action_id": action.id,
    });

    sqlx::query(
        r#"
        INSERT INTO agent_episodes (id, summary, embedding, tags, refs)
        VALUES ($1, $2, NULL, $3, $4)
        "#,
    )
    .bind(episode_id)
    .bind(&summary)
    .bind(&tags)
    .bind(&refs)
    .execute(pool)
    .await?;

    Ok(())
}

/// Extract a brief (~80 char) human-readable hint from a tool's proposed_args JSON.
fn args_short_hint(args: &Value) -> String {
    // Try to pull common meaningful fields in priority order.
    let candidates = ["inquiry_id", "customer_name", "email", "summary", "text", "tool_name"];
    for key in &candidates {
        if let Some(v) = args.get(key)
            && let Some(s) = v.as_str()
            && !s.is_empty()
        {
            let short = s.chars().take(80).collect::<String>();
            return format!("{key}={short}");
        }
    }
    // Fallback: first 80 chars of raw JSON.
    let raw = args.to_string();
    raw.chars().take(80).collect()
}

// ── pending_actions ───────────────────────────────────────────────────────────

/// Delete resolved/canceled/expired `pending_actions` rows whose `resolved_at` is
/// older than 4 days.  Rows with `status = 'pending'` or `status = 'edited'` are
/// never touched (they are still active).
///
/// Returns the number of rows deleted.
pub async fn sweep_pending_actions(pool: &PgPool, now: DateTime<Utc>) -> Result<u64> {
    let cutoff = now - chrono::Duration::days(4);
    let result = sqlx::query(
        r#"
        DELETE FROM pending_actions
        WHERE status IN ('confirmed', 'canceled', 'expired')
          AND resolved_at IS NOT NULL
          AND resolved_at < $1
        "#,
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ── agent_sessions ────────────────────────────────────────────────────────────

/// For sessions inactive for > 30 days, NULL out the `turns` column (raw turn
/// history) while keeping `last_summary` and `id`.
///
/// Sessions are never hard-deleted because they are FK targets for episodes and
/// memory proposals.
///
/// Returns the number of rows updated.
pub async fn sweep_agent_sessions(pool: &PgPool, now: DateTime<Utc>) -> Result<u64> {
    let cutoff = now - chrono::Duration::days(30);
    let result = sqlx::query(
        r#"
        UPDATE agent_sessions
           SET turns = '[]'::jsonb
         WHERE updated_at < $1
           AND turns != '[]'::jsonb
        "#,
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ── agent_episodes ────────────────────────────────────────────────────────────

/// For episodes older than 180 days that do NOT have a `keep` tag, set
/// `embedding` to NULL to free pgvector storage.  The row, summary, and tags are
/// kept — the episode remains queryable by text and tags.
///
/// Returns the number of rows updated.
pub async fn sweep_agent_episodes(pool: &PgPool, now: DateTime<Utc>) -> Result<u64> {
    let cutoff = now - chrono::Duration::days(180);
    let result = sqlx::query(
        r#"
        UPDATE agent_episodes
           SET embedding = NULL
         WHERE created_at < $1
           AND embedding IS NOT NULL
           AND NOT ('keep' = ANY(tags))
        "#,
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ── domain_events ─────────────────────────────────────────────────────────────

/// Move `domain_events` rows that have been consumed by `'assistant'` and are
/// older than 30 days to `domain_events_archive`, then delete from the primary
/// table.
///
/// Returns the number of rows archived.
pub async fn sweep_domain_events(pool: &PgPool, now: DateTime<Utc>) -> Result<u64> {
    let cutoff = now - chrono::Duration::days(30);

    // Copy-then-delete inside a transaction so we never lose events.
    let mut tx = pool.begin().await?;

    let result = sqlx::query(
        r#"
        INSERT INTO domain_events_archive (id, kind, aggregate, payload, created_at, consumed_by)
        SELECT id, kind, aggregate, payload, created_at, consumed_by
        FROM domain_events
        WHERE consumed_by ? 'assistant'
          AND created_at < $1
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await?;

    let archived = result.rows_affected();

    if archived > 0 {
        sqlx::query(
            r#"
            DELETE FROM domain_events
            WHERE consumed_by ? 'assistant'
              AND created_at < $1
            "#,
        )
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(archived)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sqlx::PgPool;
    use uuid::Uuid;

    // Helper: insert an agent_session and return its id.
    async fn insert_session(pool: &PgPool, updated_at: DateTime<Utc>) -> Uuid {
        let id = Uuid::now_v7();
        let chat_id: i64 = rand_chat_id();
        sqlx::query(
            r#"
            INSERT INTO agent_sessions (id, chat_id, turns, turn_count, created_at, updated_at)
            VALUES ($1, $2, '[{"role":"user","content":"hello"}]'::jsonb, 1, $3, $4)
            "#,
        )
        .bind(id)
        .bind(chat_id)
        .bind(updated_at)
        .bind(updated_at)
        .execute(pool)
        .await
        .expect("insert session");
        id
    }

    fn rand_chat_id() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        -(nanos as i64) - 1
    }

    // Helper: insert an agent_action row.
    async fn insert_action(
        pool: &PgPool,
        session_id: Uuid,
        tool_name: &str,
        error_message: Option<&str>,
        confirmed_action_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO agent_actions
                (id, session_id, tool_name, args, result, error_message, confirmed_action_id, created_at)
            VALUES ($1, $2, $3, '{}'::jsonb, NULL, $4, $5, $6)
            "#,
        )
        .bind(id)
        .bind(session_id)
        .bind(tool_name)
        .bind(error_message)
        .bind(confirmed_action_id)
        .bind(created_at)
        .execute(pool)
        .await
        .expect("insert action");
        id
    }

    // Helper: insert a pending_action row.
    async fn insert_pending(
        pool: &PgPool,
        session_id: Uuid,
        tool_name: &str,
        status: &str,
        resolved_at: Option<DateTime<Utc>>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO pending_actions (id, session_id, tool_name, proposed_args, status, resolved_at)
            VALUES ($1, $2, $3, '{}'::jsonb, $4, $5)
            "#,
        )
        .bind(id)
        .bind(session_id)
        .bind(tool_name)
        .bind(status)
        .bind(resolved_at)
        .execute(pool)
        .await
        .expect("insert pending");
        id
    }

    // Helper: insert an agent_episode with an embedding.
    async fn insert_episode_with_embedding(
        pool: &PgPool,
        tags: &[&str],
        created_at: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let tags_arr: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        // Build a 768-dim zero vector literal.
        let zeros = vec!["0"; 768].join(",");
        let vec_literal = format!("[{zeros}]");
        sqlx::query(
            r#"
            INSERT INTO agent_episodes (id, summary, embedding, tags, refs, created_at)
            VALUES ($1, 'test episode', $2::vector, $3, '{}'::jsonb, $4)
            "#,
        )
        .bind(id)
        .bind(&vec_literal)
        .bind(&tags_arr)
        .bind(created_at)
        .execute(pool)
        .await
        .expect("insert episode");
        id
    }

    // Helper: count agent_memory rows.
    async fn count_agent_memory(pool: &PgPool) -> i64 {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_memory")
                .fetch_one(pool)
                .await
                .expect("count agent_memory");
        n
    }

    async fn count_retention_summary_episodes(pool: &PgPool) -> i64 {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM agent_episodes WHERE 'retention_summary' = ANY(tags)",
        )
        .fetch_one(pool)
        .await
        .expect("count retention_summary episodes");
        n
    }

    // ── sweep_agent_actions ──────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_actions_deletes_old_rows(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        // Old plain read-tool success — should be deleted, no episode written.
        insert_action(&pool, session_id, "read_inquiry", None, None, now - Duration::days(5)).await;

        let (deleted, summarized) = sweep_agent_actions(&pool, now).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(summarized, 0, "plain read success should not be summarized");

        let (cnt,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agent_actions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cnt, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_actions_keeps_recent_rows(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        // Recent row — should NOT be deleted.
        insert_action(&pool, session_id, "read_inquiry", None, None, now - Duration::days(2)).await;

        let (deleted, _) = sweep_agent_actions(&pool, now).await.unwrap();
        assert_eq!(deleted, 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_actions_summarizes_error(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        insert_action(
            &pool,
            session_id,
            "send_email",
            Some("SMTP connection refused"),
            None,
            now - Duration::days(5),
        )
        .await;

        let before = count_retention_summary_episodes(&pool).await;
        let (deleted, summarized) = sweep_agent_actions(&pool, now).await.unwrap();
        let after = count_retention_summary_episodes(&pool).await;

        assert_eq!(deleted, 1);
        assert_eq!(summarized, 1);
        assert_eq!(after - before, 1, "one retention_summary episode should be written");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_actions_summarizes_confirmed_action(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        let pending_id = insert_pending(
            &pool,
            session_id,
            "send_offer",
            "confirmed",
            Some(now - Duration::days(5)),
        )
        .await;

        insert_action(
            &pool,
            session_id,
            "send_offer",
            None,
            Some(pending_id),
            now - Duration::days(5),
        )
        .await;

        let before = count_retention_summary_episodes(&pool).await;
        let (deleted, summarized) = sweep_agent_actions(&pool, now).await.unwrap();
        let after = count_retention_summary_episodes(&pool).await;

        assert_eq!(deleted, 1);
        assert_eq!(summarized, 1);
        assert_eq!(after - before, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_actions_read_success_not_summarized(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        // Plain read-tool success (no error, no confirmed_action_id) — noise, skip.
        insert_action(&pool, session_id, "get_inquiry", None, None, now - Duration::days(5)).await;

        let before = count_retention_summary_episodes(&pool).await;
        let (_, summarized) = sweep_agent_actions(&pool, now).await.unwrap();
        let after = count_retention_summary_episodes(&pool).await;

        assert_eq!(summarized, 0);
        assert_eq!(after, before, "no episode should be written for plain read success");
    }

    // ── sweep_pending_actions ────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_pending_actions_deletes_resolved(pool: PgPool) {
        let now = Utc::now();
        let session_id = insert_session(&pool, now).await;

        insert_pending(
            &pool,
            session_id,
            "send_offer",
            "confirmed",
            Some(now - Duration::days(5)),
        )
        .await;
        insert_pending(
            &pool,
            session_id,
            "send_offer",
            "canceled",
            Some(now - Duration::days(5)),
        )
        .await;
        insert_pending(
            &pool,
            session_id,
            "send_offer",
            "expired",
            Some(now - Duration::days(5)),
        )
        .await;
        // Pending — must NOT be deleted.
        insert_pending(&pool, session_id, "send_offer", "pending", None).await;

        let deleted = sweep_pending_actions(&pool, now).await.unwrap();
        assert_eq!(deleted, 3, "confirmed+canceled+expired rows should be deleted");

        let (cnt,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_actions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cnt, 1, "pending row must survive");
    }

    // ── sweep_agent_sessions ─────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_sessions_clears_old_turns(pool: PgPool) {
        let now = Utc::now();
        let old_session = insert_session(&pool, now - Duration::days(40)).await;
        let new_session = insert_session(&pool, now - Duration::days(5)).await;

        let cleared = sweep_agent_sessions(&pool, now).await.unwrap();
        assert_eq!(cleared, 1);

        let (old_turns,): (Value,) =
            sqlx::query_as("SELECT turns FROM agent_sessions WHERE id = $1")
                .bind(old_session)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(old_turns, serde_json::json!([]), "old session turns should be cleared");

        let (new_turns,): (Value,) =
            sqlx::query_as("SELECT turns FROM agent_sessions WHERE id = $1")
                .bind(new_session)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(new_turns, serde_json::json!([]), "recent session turns should be untouched");
    }

    // ── sweep_agent_episodes ─────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_agent_episodes_drops_old_embedding(pool: PgPool) {
        let now = Utc::now();

        let old_id = insert_episode_with_embedding(
            &pool,
            &["tool_call"],
            now - Duration::days(200),
        )
        .await;
        let kept_id = insert_episode_with_embedding(
            &pool,
            &["tool_call", "keep"],
            now - Duration::days(200),
        )
        .await;
        let recent_id =
            insert_episode_with_embedding(&pool, &["tool_call"], now - Duration::days(10)).await;

        let dropped = sweep_agent_episodes(&pool, now).await.unwrap();
        assert_eq!(dropped, 1, "only the old non-keep row should lose embedding");

        // Old without keep — embedding should be NULL.
        let (emb,): (Option<String>,) =
            sqlx::query_as("SELECT embedding::text FROM agent_episodes WHERE id = $1")
                .bind(old_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(emb.is_none(), "old episode embedding should be NULL");

        // Old with keep tag — embedding must remain.
        let (emb,): (Option<String>,) =
            sqlx::query_as("SELECT embedding::text FROM agent_episodes WHERE id = $1")
                .bind(kept_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(emb.is_some(), "kept episode should retain embedding");

        // Recent — embedding must remain.
        let (emb,): (Option<String>,) =
            sqlx::query_as("SELECT embedding::text FROM agent_episodes WHERE id = $1")
                .bind(recent_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(emb.is_some(), "recent episode should retain embedding");
    }

    // ── domain_events ────────────────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_sweep_domain_events_archives_consumed(pool: PgPool) {
        let now = Utc::now();

        // Old, consumed by assistant — should be archived.
        let old_consumed_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO domain_events (id, kind, aggregate, payload, created_at, consumed_by)
            VALUES ($1, 'inquiry.created', 'inquiry:abc', '{"x":1}'::jsonb, $2,
                    '{"assistant": "2026-01-01T00:00:00Z"}'::jsonb)
            "#,
        )
        .bind(old_consumed_id)
        .bind(now - Duration::days(35))
        .execute(&pool)
        .await
        .unwrap();

        // Old, NOT consumed — should stay in domain_events.
        let old_unconsumed_id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO domain_events (id, kind, aggregate, payload, created_at, consumed_by)
            VALUES ($1, 'inquiry.created', 'inquiry:xyz', '{"x":2}'::jsonb, $2, '{}'::jsonb)
            "#,
        )
        .bind(old_unconsumed_id)
        .bind(now - Duration::days(35))
        .execute(&pool)
        .await
        .unwrap();

        let archived = sweep_domain_events(&pool, now).await.unwrap();
        assert_eq!(archived, 1);

        let (in_primary,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM domain_events WHERE id = $1")
                .bind(old_consumed_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(in_primary, 0, "consumed+old event should be removed from domain_events");

        let (in_archive,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM domain_events_archive WHERE id = $1")
                .bind(old_consumed_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(in_archive, 1, "consumed+old event should appear in archive");

        let (unconsumed_in_primary,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM domain_events WHERE id = $1")
                .bind(old_unconsumed_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unconsumed_in_primary, 1, "unconsumed event must stay in primary table");
    }

    // ── agent_memory untouched ───────────────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_agent_memory_never_touched(pool: PgPool) {
        let before = count_agent_memory(&pool).await;
        // Just verify the sweeper doesn't interact with agent_memory.
        let _now = Utc::now();
        let _ = run_retention_pass(&pool).await;
        let after = count_agent_memory(&pool).await;
        assert_eq!(before, after, "agent_memory must not be touched by any sweeper");
    }
}
