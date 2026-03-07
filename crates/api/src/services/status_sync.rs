use sqlx::PgPool;
use uuid::Uuid;

/// Sync linked entities when a quote is accepted.
/// - Offers with status 'draft' or 'sent' → 'accepted'
pub async fn sync_quote_accepted(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE offers SET status = 'accepted' WHERE inquiry_id = $1 AND status IN ('draft', 'sent')",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sync linked entities when a quote is cancelled or rejected.
/// - Offers with status 'draft' or 'sent' → 'rejected'
pub async fn sync_quote_cancelled(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE offers SET status = 'rejected' WHERE inquiry_id = $1 AND status IN ('draft', 'sent')",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sync linked entities when a quote is downgraded (back to pre-acceptance).
/// - Accepted offers → draft
pub async fn sync_quote_downgraded(pool: &PgPool, inquiry_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE offers SET status = 'draft' WHERE inquiry_id = $1 AND status = 'accepted'",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;

    #[tokio::test]
    async fn sync_quote_accepted_updates_offers() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let offer_id = insert_test_offer(&pool, inquiry_id, "sent").await;

        sync_quote_accepted(&pool, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn sync_quote_cancelled_rejects_offers() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let offer_id = insert_test_offer(&pool, inquiry_id, "draft").await;

        sync_quote_cancelled(&pool, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "rejected");
    }

    #[tokio::test]
    async fn sync_quote_downgraded_reverts_offer_to_draft() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "accepted").await;
        let offer_id = insert_test_offer(&pool, inquiry_id, "accepted").await;

        sync_quote_downgraded(&pool, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "draft");
    }
}
