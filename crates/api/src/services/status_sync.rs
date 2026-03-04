use aust_calendar::CalendarService;
use sqlx::PgPool;
use uuid::Uuid;

/// Sync linked entities when a quote is accepted.
/// - Offers with status 'draft' or 'sent' → 'accepted'
/// - Active booking → confirmed
pub async fn sync_quote_accepted(
    pool: &PgPool,
    calendar: &CalendarService,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE offers SET status = 'accepted' WHERE inquiry_id = $1 AND status IN ('draft', 'sent')",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    let booking_row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_bookings WHERE inquiry_id = $1 AND status != 'cancelled' LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    if let Some((booking_id,)) = booking_row {
        let _ = calendar.confirm_booking(booking_id).await;
    }

    Ok(())
}

/// Sync linked entities when a quote is cancelled or rejected.
/// - Active bookings → cancelled
/// - Offers with status 'draft' or 'sent' → 'rejected'
pub async fn sync_quote_cancelled(
    pool: &PgPool,
    calendar: &CalendarService,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    let booking_row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_bookings WHERE inquiry_id = $1 AND status != 'cancelled' LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    if let Some((booking_id,)) = booking_row {
        let _ = calendar.cancel_booking(booking_id).await;
    }

    sqlx::query(
        "UPDATE offers SET status = 'rejected' WHERE inquiry_id = $1 AND status IN ('draft', 'sent')",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Sync linked entities when a quote is downgraded (back to pre-acceptance).
/// - Active bookings → tentative
/// - Accepted offers → draft
pub async fn sync_quote_downgraded(pool: &PgPool, inquiry_id: Uuid) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE calendar_bookings SET status = 'tentative', updated_at = $1 WHERE inquiry_id = $2 AND status != 'cancelled'",
    )
    .bind(now)
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "UPDATE offers SET status = 'draft' WHERE inquiry_id = $1 AND status = 'accepted'",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Sync linked entities when a booking is confirmed.
/// - Quote → 'accepted' (if currently offer_generated/offer_sent)
/// - Offers with status 'draft' or 'sent' → 'accepted'
pub async fn sync_booking_confirmed(pool: &PgPool, inquiry_id: Uuid) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE inquiries SET status = 'accepted', updated_at = $1 WHERE id = $2 AND status IN ('offer_ready', 'offer_sent')",
    )
    .bind(now)
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "UPDATE offers SET status = 'accepted' WHERE inquiry_id = $1 AND status IN ('draft', 'sent')",
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Sync linked entities when a booking is cancelled.
/// - Quote → 'rejected' (only if no other active bookings remain)
pub async fn sync_booking_cancelled(pool: &PgPool, inquiry_id: Uuid) -> Result<(), sqlx::Error> {
    // Check if other active bookings exist
    let other: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM calendar_bookings WHERE inquiry_id = $1 AND status != 'cancelled'",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    let has_others = other.map(|(c,)| c > 0).unwrap_or(false);

    if !has_others {
        let now = chrono::Utc::now();
        sqlx::query(
            "UPDATE inquiries SET status = 'rejected', updated_at = $1 WHERE id = $2 AND status IN ('offer_ready', 'offer_sent', 'accepted')",
        )
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    }

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

        let calendar = aust_calendar::CalendarService::new(pool.clone(), 1, 3, 14);
        sync_quote_accepted(&pool, &calendar, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn sync_quote_cancelled_rejects_offers() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let offer_id = insert_test_offer(&pool, inquiry_id, "draft").await;

        let calendar = aust_calendar::CalendarService::new(pool.clone(), 1, 3, 14);
        sync_quote_cancelled(&pool, &calendar, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "rejected");
    }

    #[tokio::test]
    async fn sync_quote_cancelled_cancels_bookings() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "confirmed").await;

        let calendar = aust_calendar::CalendarService::new(pool.clone(), 1, 3, 14);
        sync_quote_cancelled(&pool, &calendar, inquiry_id).await.unwrap();

        let status = get_booking_status(&pool, booking_id).await;
        assert_eq!(status, "cancelled");
    }

    #[tokio::test]
    async fn sync_booking_confirmed_updates_quote() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "offer_sent").await;
        insert_test_offer(&pool, inquiry_id, "sent").await;
        insert_test_booking(&pool, inquiry_id, "tentative").await;

        sync_booking_confirmed(&pool, inquiry_id).await.unwrap();

        let status = get_quote_status(&pool, inquiry_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn sync_booking_cancelled_preserves_quote_if_other_bookings() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "accepted").await;
        // One confirmed booking (unique partial index only allows one active per quote)
        insert_test_booking(&pool, inquiry_id, "confirmed").await;

        // sync_booking_cancelled checks if ANY active bookings remain.
        // Since b1 is still confirmed, quote should stay accepted.
        sync_booking_cancelled(&pool, inquiry_id).await.unwrap();

        let status = get_quote_status(&pool, inquiry_id).await;
        assert_eq!(status, "accepted"); // Not downgraded because b1 still exists
    }

    #[tokio::test]
    async fn sync_quote_downgraded_reverts_booking_to_tentative() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "accepted").await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "confirmed").await;

        sync_quote_downgraded(&pool, inquiry_id).await.unwrap();

        let status = get_booking_status(&pool, booking_id).await;
        assert_eq!(status, "tentative");
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

    #[tokio::test]
    async fn sync_booking_cancelled_rejects_quote_when_last_booking() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "offer_sent").await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "confirmed").await;

        // Cancel the booking first so sync_booking_cancelled sees zero active bookings
        let calendar = aust_calendar::CalendarService::new(pool.clone(), 3, 3, 14);
        calendar.cancel_booking(booking_id).await.unwrap();

        sync_booking_cancelled(&pool, inquiry_id).await.unwrap();

        let status = get_quote_status(&pool, inquiry_id).await;
        assert_eq!(status, "rejected");
    }

    #[tokio::test]
    async fn sync_booking_confirmed_updates_offer_status() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "offer_sent").await;
        let offer_id = insert_test_offer(&pool, inquiry_id, "sent").await;
        insert_test_booking(&pool, inquiry_id, "tentative").await;

        sync_booking_confirmed(&pool, inquiry_id).await.unwrap();

        let status = get_offer_status(&pool, offer_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn sync_quote_accepted_with_no_booking_does_not_error() {
        let pool = test_db_pool().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "offer_sent").await;

        let calendar = aust_calendar::CalendarService::new(pool.clone(), 3, 3, 14);
        let result = sync_quote_accepted(&pool, &calendar, inquiry_id).await;
        assert!(result.is_ok());
    }
}
