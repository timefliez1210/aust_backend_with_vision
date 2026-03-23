//! Email repository — centralised queries for `email_threads` and `email_messages` tables.

use sqlx::PgPool;
use uuid::Uuid;

/// Find the most recent email thread for an inquiry.
///
/// **Caller**: `find_or_create_inquiry_thread`, `find_or_create_offer_thread`
/// **Why**: Multiple flows need to find an existing thread before creating a new one.
pub(crate) async fn find_thread_by_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM email_threads WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Find a thread linked by customer_id through the inquiry's customer.
///
/// **Caller**: `find_or_create_offer_thread`
/// **Why**: Fallback when no thread is directly linked to the inquiry.
pub(crate) async fn find_thread_by_inquiry_customer(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT et.id FROM email_threads et
        JOIN inquiries q ON et.customer_id = q.customer_id
        WHERE q.id = $1
        ORDER BY et.created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Link an existing thread to an inquiry.
///
/// **Caller**: `find_or_create_offer_thread`
/// **Why**: When a customer thread exists but isn't linked to the inquiry yet.
pub(crate) async fn link_thread_to_inquiry(
    pool: &PgPool,
    thread_id: Uuid,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE email_threads SET inquiry_id = $1, updated_at = NOW() WHERE id = $2")
        .bind(inquiry_id)
        .bind(thread_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Create a new email thread.
///
/// **Caller**: `find_or_create_inquiry_thread`, `find_or_create_offer_thread`
/// **Why**: When no thread exists for an inquiry, one must be created.
pub(crate) async fn create_thread(
    pool: &PgPool,
    id: Uuid,
    customer_id: Uuid,
    inquiry_id: Uuid,
    subject: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_threads (id, customer_id, inquiry_id, subject, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, NOW(), NOW())",
    )
    .bind(id)
    .bind(customer_id)
    .bind(inquiry_id)
    .bind(subject)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert an email message (draft or outbound).
///
/// **Caller**: `generate_offer_email_draft`, `handle_offer_approval`
/// **Why**: Centralises email message creation across offer approval and draft generation.
pub(crate) async fn insert_message(
    pool: &PgPool,
    id: Uuid,
    thread_id: Uuid,
    direction: &str,
    from_address: &str,
    to_address: &str,
    subject: &str,
    body_text: &str,
    llm_generated: bool,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO email_messages
            (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
        "#,
    )
    .bind(id)
    .bind(thread_id)
    .bind(direction)
    .bind(from_address)
    .bind(to_address)
    .bind(subject)
    .bind(body_text)
    .bind(llm_generated)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

/// Discard all LLM-generated draft messages in a thread.
///
/// **Caller**: `generate_offer_email_draft`
/// **Why**: When regenerating an offer, stale drafts must be discarded.
pub(crate) async fn discard_llm_drafts(
    pool: &PgPool,
    thread_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE email_messages SET status = 'discarded' \
         WHERE thread_id = $1 AND status = 'draft' AND llm_generated = true",
    )
    .bind(thread_id)
    .execute(pool)
    .await?;
    Ok(())
}
