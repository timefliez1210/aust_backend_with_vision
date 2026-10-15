//! Conversion repository — the raw facts behind the Conversion tab.
//!
//! Three read-only bulk reads, all tenant-scoped by RLS:
//! - every inquiry with its KVA milestones ([`fetch_inquiries`]); send and Zusage
//!   dates fall back to the `status.changed` event log for older inquiries,
//! - every website form mail the mailbox stored ([`fetch_form_submissions`]),
//! - every Rückruf request from the flash-contact form ([`fetch_flash_contacts`]).
//!
//! Attribution and the funnel live in [`crate::services::conversion_service`]; this
//! file only shapes rows. A few hundred rows per tenant, so no pagination.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// One inquiry with what happened to it.
#[derive(Debug, Clone, FromRow)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub(crate) struct InquiryFactRow {
    pub id: Uuid,
    /// Customer id after following `merged_into`, so a merged duplicate still matches.
    pub customer_key: Option<Uuid>,
    pub source: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub offer_sent_at: Option<DateTime<Utc>>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub customer_phone: Option<String>,
    /// First KVA ever drafted for the inquiry (superseded ones excluded).
    pub first_offer_at: Option<DateTime<Utc>>,
    /// Latest send timestamp on any active KVA.
    pub kva_sent_at: Option<DateTime<Utc>>,
    /// Netto price of the current KVA.
    pub price_cents: Option<i64>,
}

/// One form mail from the website (`send-mail.php` → angebot@ inbox).
#[derive(Debug, Clone, FromRow)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub(crate) struct FormSubmissionRow {
    pub id: Uuid,
    pub subject: String,
    pub created_at: DateTime<Utc>,
    pub customer_key: Option<Uuid>,
    /// Set when the mail pipeline created the inquiry itself (Kostenloses Angebot).
    pub inquiry_id: Option<Uuid>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
}

/// One Rückruf request.
#[derive(Debug, Clone, FromRow)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub(crate) struct FlashContactRow {
    pub id: Uuid,
    pub name: String,
    pub phone: String,
    pub created_at: DateTime<Utc>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

/// Every inquiry with its KVA milestones.
pub(crate) async fn fetch_inquiries(db: &PgPool) -> Result<Vec<InquiryFactRow>, ApiError> {
    let rows = sqlx::query_as::<_, InquiryFactRow>(
        r#"
        SELECT q.id,
               COALESCE(c.merged_into, c.id) AS customer_key,
               q.source::text AS source,
               q.status::text AS status,
               q.created_at,
               COALESCE(q.offer_sent_at, ev.sent_at) AS offer_sent_at,
               COALESCE(q.accepted_at, ev.accepted_at) AS accepted_at,
               COALESCE(NULLIF(c.company_name, ''),
                        NULLIF(TRIM(CONCAT_WS(' ', c.first_name, c.last_name)), ''),
                        NULLIF(c.name, ''),
                        c.email) AS customer_name,
               c.email AS customer_email,
               c.phone AS customer_phone,
               o.first_offer_at,
               o.kva_sent_at,
               o.price_cents
        FROM inquiries q
        LEFT JOIN customers c ON c.id = q.customer_id
        LEFT JOIN LATERAL (
            SELECT MIN(o.created_at) AS first_offer_at,
                   MAX(o.sent_at) AS kva_sent_at,
                   (ARRAY_AGG(o.price_cents ORDER BY o.created_at DESC))[1] AS price_cents
            FROM offers o
            WHERE o.inquiry_id = q.id AND o.status <> 'superseded'
        ) o ON TRUE
        -- Status log, for inquiries moved along before the timestamps were stamped
        -- (migration 20261013120000).
        LEFT JOIN LATERAL (
            SELECT MIN(e.created_at) FILTER (WHERE e.payload->>'new_status' = 'offer_sent') AS sent_at,
                   MIN(e.created_at) FILTER (WHERE e.payload->>'new_status' IN ('accepted', 'scheduled')) AS accepted_at
            FROM (
                SELECT payload, created_at FROM domain_events
                WHERE kind = 'status.changed' AND payload->>'inquiry_id' = q.id::text
                UNION ALL
                SELECT payload, created_at FROM domain_events_archive
                WHERE kind = 'status.changed' AND payload->>'inquiry_id' = q.id::text
            ) e
        ) ev ON TRUE
        "#,
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Website form mails: Kontaktformular and the Kostenloses-Angebot variants.
///
/// The subjects are fixed by `frontend/static/send-mail.php`; the mailbox links every
/// one of them to a customer, and to the inquiry when the pipeline created it.
pub(crate) async fn fetch_form_submissions(
    db: &PgPool,
) -> Result<Vec<FormSubmissionRow>, ApiError> {
    let rows = sqlx::query_as::<_, FormSubmissionRow>(
        r#"
        SELECT m.id,
               m.subject,
               m.created_at,
               COALESCE(c.merged_into, c.id) AS customer_key,
               t.inquiry_id,
               COALESCE(NULLIF(c.company_name, ''),
                        NULLIF(TRIM(CONCAT_WS(' ', c.first_name, c.last_name)), ''),
                        NULLIF(c.name, ''),
                        m.from_address) AS name,
               COALESCE(c.email, m.from_address) AS email,
               c.phone
        FROM email_messages m
        JOIN email_threads t ON t.id = m.thread_id
        LEFT JOIN customers c ON c.id = t.customer_id
        WHERE m.direction = 'inbound'
          AND m.subject ~ '^(Neue Kontaktanfrage|Neues Angebotsanfrage|Neue Angebotsanfrage|Terminanfrage Kostenvoranschlag|Manuelle Angebotsanfrage) von'
        "#,
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Every Rückruf request, dismissed ones included (they are drop-offs too).
pub(crate) async fn fetch_flash_contacts(db: &PgPool) -> Result<Vec<FlashContactRow>, ApiError> {
    let rows = sqlx::query_as::<_, FlashContactRow>(
        "SELECT id, name, phone, created_at, dismissed_at FROM flash_contacts",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}
