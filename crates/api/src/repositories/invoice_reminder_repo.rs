//! Repository for `invoice_reminders` — dashboard-driven dunning flow.

use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Full row returned by `fetch_due`.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InvoiceReminderRow {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub inquiry_id: Uuid,
    pub invoice_number: String,
    pub level: i32,
    pub remind_after: NaiveDate,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// Inserts the initial reminder (level 1) when an invoice is sent.
///
/// **Caller**: `routes::invoices::send_invoice` — called immediately after marking sent.
/// **Why**: Ensures every sent invoice automatically gets a 7-day follow-up in the
/// admin dashboard without any manual setup.
pub(crate) async fn create(
    db: &PgPool,
    invoice_id: Uuid,
    remind_after: NaiveDate,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO invoice_reminders (invoice_id, level, status, remind_after)
        VALUES ($1, 1, 'pending', $2)
        ON CONFLICT (invoice_id) DO NOTHING
        "#,
    )
    .bind(invoice_id)
    .bind(remind_after)
    .execute(db)
    .await
    .map_err(ApiError::Database)?;
    Ok(())
}

/// Advances a reminder to the next level (or closes it at level 3).
///
/// **Caller**: `routes::admin::invoice_reminder_action` (action = "send")
/// **Why**: After a dunning email is sent the next level becomes active
/// 7 days later, or the reminder is closed after the 2. Mahnung.
pub(crate) async fn advance(db: &PgPool, id: Uuid, next_remind_after: NaiveDate) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE invoice_reminders
        SET level        = CASE WHEN level >= 3 THEN level ELSE level + 1 END,
            status       = CASE WHEN level >= 3 THEN 'closed' ELSE 'pending' END,
            remind_after = CASE WHEN level >= 3 THEN remind_after ELSE $2 END,
            updated_at   = NOW()
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(next_remind_after)
    .execute(db)
    .await
    .map_err(ApiError::Database)?;
    Ok(())
}

/// Snoozes a reminder by updating its `remind_after` date.
///
/// **Caller**: `routes::admin::invoice_reminder_action` (action = "later")
pub(crate) async fn snooze(db: &PgPool, id: Uuid, remind_after: NaiveDate) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE invoice_reminders SET remind_after = $2, updated_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .bind(remind_after)
    .execute(db)
    .await
    .map_err(ApiError::Database)?;
    Ok(())
}

/// Closes a reminder (admin marked the invoice as paid or dismissed).
///
/// **Caller**: `routes::admin::invoice_reminder_action` (action = "paid")
pub(crate) async fn close(db: &PgPool, id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE invoice_reminders SET status = 'closed', updated_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .execute(db)
    .await
    .map_err(ApiError::Database)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

/// Returns all due reminders (remind_after ≤ today, status = pending).
///
/// **Caller**: `routes::admin::list_invoice_reminders` + dashboard count
pub(crate) async fn fetch_due(db: &PgPool) -> Result<Vec<InvoiceReminderRow>, ApiError> {
    sqlx::query_as::<_, InvoiceReminderRow>(
        r#"
        SELECT
            ir.id,
            ir.invoice_id,
            i.inquiry_id,
            i.invoice_number,
            ir.level,
            ir.remind_after,
            c.name      AS customer_name,
            c.email     AS customer_email
        FROM invoice_reminders ir
        JOIN invoices   i   ON i.id  = ir.invoice_id
        JOIN inquiries  inq ON inq.id = i.inquiry_id
        LEFT JOIN customers c ON c.id = inq.customer_id
        WHERE ir.status = 'pending'
          AND ir.remind_after <= CURRENT_DATE
          AND i.status = 'sent'
        ORDER BY ir.remind_after, ir.level
        "#,
    )
    .fetch_all(db)
    .await
    .map_err(ApiError::Database)
}

/// Fetch a single reminder row (for action handler).
pub(crate) async fn fetch_one(db: &PgPool, id: Uuid) -> Result<Option<InvoiceReminderRow>, ApiError> {
    sqlx::query_as::<_, InvoiceReminderRow>(
        r#"
        SELECT
            ir.id,
            ir.invoice_id,
            i.inquiry_id,
            i.invoice_number,
            ir.level,
            ir.remind_after,
            c.name      AS customer_name,
            c.email     AS customer_email
        FROM invoice_reminders ir
        JOIN invoices   i   ON i.id  = ir.invoice_id
        JOIN inquiries  inq ON inq.id = i.inquiry_id
        LEFT JOIN customers c ON c.id = inq.customer_id
        WHERE ir.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(ApiError::Database)
}
