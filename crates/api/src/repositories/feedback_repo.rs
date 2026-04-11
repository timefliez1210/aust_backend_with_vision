//! Repository functions for feedback reports (bugs and feature requests).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// A row from the `feedback_reports` table.
#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub(crate) struct FeedbackReport {
    pub id: Uuid,
    pub report_type: String,
    pub priority: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub attachment_keys: Vec<String>,
    pub status: String,
    /// Written by Claude/agents: fix summary, commit reference, or clarification question.
    pub agent_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a new feedback report and return the created row.
///
/// **Caller**: `routes/admin.rs` — `create_feedback` handler.
/// **Why**: Persists a bug report or feature request submitted from the admin dashboard.
///
/// # Parameters
/// - `report_type`     — "bug" or "feature"
/// - `priority`        — "low" / "medium" / "high" / "critical"
/// - `title`           — short summary
/// - `description`     — full description (optional)
/// - `location`        — page or area in the app (optional)
/// - `attachment_keys` — S3 keys of uploaded screenshots
pub(crate) async fn create_report(
    pool: &PgPool,
    report_type: &str,
    priority: &str,
    title: &str,
    description: Option<&str>,
    location: Option<&str>,
    attachment_keys: &[String],
) -> Result<FeedbackReport, sqlx::Error> {
    sqlx::query_as::<_, FeedbackReport>(
        r#"INSERT INTO feedback_reports
             (report_type, priority, title, description, location, attachment_keys)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING *"#,
    )
    .bind(report_type)
    .bind(priority)
    .bind(title)
    .bind(description)
    .bind(location)
    .bind(attachment_keys)
    .fetch_one(pool)
    .await
}

/// Fetch all feedback reports ordered newest-first, with optional status and type filters.
///
/// **Caller**: `routes/admin.rs` — `list_feedback` handler.
/// **Why**: Provides the admin with a filterable list of all submitted reports.
///
/// # Parameters
/// - `status_filter` — optional status to filter by ("open", "in_progress", "resolved")
/// - `type_filter`   — optional type to filter by ("bug", "feature")
pub(crate) async fn list_reports(
    pool: &PgPool,
    status_filter: Option<&str>,
    type_filter: Option<&str>,
) -> Result<Vec<FeedbackReport>, sqlx::Error> {
    sqlx::query_as::<_, FeedbackReport>(
        r#"SELECT * FROM feedback_reports
           WHERE ($1::text IS NULL OR status = $1)
             AND ($2::text IS NULL OR report_type = $2)
           ORDER BY created_at DESC"#,
    )
    .bind(status_filter)
    .bind(type_filter)
    .fetch_all(pool)
    .await
}

/// Fetch a single feedback report by ID.
///
/// **Caller**: `routes/admin.rs` — `get_feedback` handler.
/// **Why**: Returns full detail for the report detail/download view.
///
/// # Parameters
/// - `id` — UUID of the report
///
/// # Returns
/// `Some(FeedbackReport)` if found, `None` if not.
pub(crate) async fn get_report(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<FeedbackReport>, sqlx::Error> {
    sqlx::query_as::<_, FeedbackReport>(
        "SELECT * FROM feedback_reports WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Update status and/or agent_notes on a feedback report.
///
/// **Caller**: `routes/admin.rs` — `patch_feedback` handler (human admin and agents).
/// **Why**: Agents write `agent_notes` to document what was fixed or what clarification is
/// needed. Human admins use `status` to track progress. Both can be set independently.
///
/// # Parameters
/// - `id`          — UUID of the report
/// - `status`      — new status (None = leave unchanged)
/// - `agent_notes` — agent-written notes (None = leave unchanged)
pub(crate) async fn update_report(
    pool: &PgPool,
    id: Uuid,
    status: Option<&str>,
    agent_notes: Option<&str>,
) -> Result<FeedbackReport, sqlx::Error> {
    sqlx::query_as::<_, FeedbackReport>(
        r#"UPDATE feedback_reports
           SET status      = COALESCE($2, status),
               agent_notes = COALESCE($3, agent_notes),
               updated_at  = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(status)
    .bind(agent_notes)
    .fetch_one(pool)
    .await
}
