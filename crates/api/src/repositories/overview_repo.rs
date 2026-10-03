//! Overview repository — the few aggregates the "Heute" screen needs that no other
//! page already computes. Everything that *is* computed elsewhere (register amounts,
//! KVA-Buch state, calendar schedule) is reused from its own repo so the numbers on
//! the overview can never disagree with the page they link to.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgPool};

/// Oldest callback request still waiting — how long someone has been waiting.
///
/// **Caller**: `routes::overview::overview`
pub(crate) async fn oldest_open_flash_contact(pool: &PgPool) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let (oldest,): (Option<DateTime<Utc>>,) = sqlx::query_as(
        "SELECT MIN(created_at) FROM flash_contacts WHERE handled_at IS NULL AND dismissed_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    Ok(oldest)
}

/// Unread inbound mail: when the oldest arrived and who the first few are from.
#[derive(Debug, FromRow)]
pub(crate) struct UnreadMailSummary {
    pub oldest: Option<DateTime<Utc>>,
    pub senders: Vec<String>,
}

/// **Caller**: `routes::overview::overview`
/// **Why**: the count alone says "4 mails"; the oldest age and the names say whether
/// it can wait. Uses the same `read_at IS NULL` condition as the mailbox badge.
pub(crate) async fn unread_mail_summary(pool: &PgPool) -> Result<UnreadMailSummary, sqlx::Error> {
    sqlx::query_as(
        r#"
        WITH unread AS (
            SELECT m.created_at,
                   COALESCE(NULLIF(TRIM(COALESCE(c.first_name, '') || ' ' || COALESCE(c.last_name, '')), ''),
                            c.name, m.from_address) AS sender
            FROM email_messages m
            JOIN email_threads t ON t.id = m.thread_id
            LEFT JOIN customers c ON c.id = t.customer_id
            WHERE m.direction = 'inbound' AND m.read_at IS NULL
        ),
        names AS (
            SELECT sender, MIN(created_at) AS first_at FROM unread GROUP BY sender ORDER BY first_at LIMIT 3
        )
        SELECT (SELECT MIN(created_at) FROM unread) AS oldest,
               COALESCE((SELECT ARRAY_AGG(sender ORDER BY first_at) FROM names), '{}') AS senders
        "#,
    )
    .fetch_one(pool)
    .await
}

/// One inquiry's progress through the pipeline, for the funnel.
#[derive(Debug, FromRow)]
pub(crate) struct FunnelRow {
    pub status: String,
    pub has_volume: bool,
    pub has_offer: bool,
    pub has_issued_invoice: bool,
}

/// Every inquiry created since `from`, with how far it got.
///
/// **Caller**: `routes::overview::overview`
/// **Why**: "how far it got" cannot be read from `status` alone — a rejected
/// inquiry still had an offer. So the stages are evidence-based: a volume was
/// estimated, an offer exists (superseded ones don't count twice, they're the same
/// KVA), an invoice was actually issued.
pub(crate) async fn funnel_since(pool: &PgPool, from: NaiveDate) -> Result<Vec<FunnelRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT i.status,
               (i.estimated_volume_m3 IS NOT NULL AND i.estimated_volume_m3 > 0) AS has_volume,
               EXISTS (SELECT 1 FROM offers o WHERE o.inquiry_id = i.id AND o.status <> 'superseded') AS has_offer,
               EXISTS (SELECT 1 FROM invoices v
                       WHERE v.inquiry_id = i.id
                         AND (v.sent_at IS NOT NULL OR v.paid_at IS NOT NULL)
                         AND v.status NOT IN ('void', 'written_off')) AS has_issued_invoice
        FROM inquiries i
        WHERE (i.created_at AT TIME ZONE 'Europe/Berlin')::date >= $1
        "#,
    )
    .bind(from)
    .fetch_all(pool)
    .await
}
