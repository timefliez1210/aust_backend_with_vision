//! Repository for `review_requests` — Google-review follow-up emails.

use chrono::{NaiveDate, DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Row returned by `fetch_pending_reminders`.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ReviewReminderRow {
    pub inquiry_id: Uuid,
    pub remind_after: NaiveDate,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// Creates or replaces the review request for an inquiry.
///
/// **Caller**: `routes::admin::create_review_request`
/// **Why**: ON CONFLICT UPDATE lets the admin change their mind (e.g. "Nicht" → "Später").
pub(crate) async fn upsert(
    db: &PgPool,
    inquiry_id: Uuid,
    status: &str,
    remind_after: Option<NaiveDate>,
    sent_at: Option<DateTime<Utc>>,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO review_requests (inquiry_id, status, remind_after, sent_at, updated_at)
        VALUES ($1, $2, $3, $4, NOW())
        ON CONFLICT (inquiry_id) DO UPDATE
            SET status       = EXCLUDED.status,
                remind_after = EXCLUDED.remind_after,
                sent_at      = EXCLUDED.sent_at,
                updated_at   = NOW()
        "#,
    )
    .bind(inquiry_id)
    .bind(status)
    .bind(remind_after)
    .bind(sent_at)
    .execute(db)
    .await
    .map_err(ApiError::Database)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Returns all pending review requests whose `remind_after` date is today or earlier.
///
/// **Caller**: `routes::admin::list_review_reminders` (dashboard widget)
/// **Why**: Surfaces inquiries where Alex chose "Später" and the reminder date has arrived.
pub(crate) async fn fetch_pending_reminders(db: &PgPool) -> Result<Vec<ReviewReminderRow>, ApiError> {
    sqlx::query_as::<_, ReviewReminderRow>(
        r#"
        SELECT rr.inquiry_id,
               rr.remind_after,
               c.name     AS customer_name,
               c.email    AS customer_email
        FROM review_requests rr
        JOIN inquiries        i ON i.id = rr.inquiry_id
        LEFT JOIN customers   c ON c.id = i.customer_id
        WHERE rr.status = 'pending'
          AND rr.remind_after IS NOT NULL
          AND rr.remind_after <= CURRENT_DATE
        ORDER BY rr.remind_after
        "#,
    )
    .fetch_all(db)
    .await
    .map_err(ApiError::Database)
}
