//! Shared SQL query helpers extracted from repeated patterns across route handlers.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Update a quote's estimated volume and status after estimation completes.
pub async fn update_quote_volume(
    pool: &PgPool,
    quote_id: Uuid,
    volume: f64,
    status: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(volume)
    .bind(status)
    .bind(now)
    .bind(quote_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Check if a quote has any non-cancelled calendar booking.
pub async fn has_active_booking(pool: &PgPool, quote_id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_bookings WHERE quote_id = $1 AND status != 'cancelled' LIMIT 1",
    )
    .bind(quote_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Return the booking ID for the first non-cancelled booking of a quote, if any.
pub async fn find_active_booking_id(
    pool: &PgPool,
    quote_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_bookings WHERE quote_id = $1 AND status != 'cancelled' LIMIT 1",
    )
    .bind(quote_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Row returned by insert_estimation.
#[derive(Debug, FromRow)]
pub struct EstimationRow {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub method: String,
    pub status: String,
    pub source_data: serde_json::Value,
    pub result_data: Option<serde_json::Value>,
    pub total_volume_m3: Option<f64>,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Insert a volume estimation record and return the full row.
pub async fn insert_estimation(
    pool: &PgPool,
    id: Uuid,
    quote_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
    now: DateTime<Utc>,
) -> Result<EstimationRow, sqlx::Error> {
    let row: EstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations
            (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#,
    )
    .bind(id)
    .bind(quote_id)
    .bind(method)
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Insert a volume estimation record without returning (fire-and-forget style).
pub async fn insert_estimation_no_return(
    pool: &PgPool,
    id: Uuid,
    quote_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO volume_estimations
            (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(id)
    .bind(quote_id)
    .bind(method)
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;

    #[tokio::test]
    async fn test_has_active_booking_ignores_cancelled() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        insert_test_booking(&pool, quote_id, "cancelled").await;
        let result = has_active_booking(&pool, quote_id).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_has_active_booking_finds_confirmed() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        insert_test_booking(&pool, quote_id, "confirmed").await;
        let result = has_active_booking(&pool, quote_id).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_has_active_booking_finds_tentative() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        insert_test_booking(&pool, quote_id, "tentative").await;
        let result = has_active_booking(&pool, quote_id).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn test_has_active_booking_no_booking() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        let result = has_active_booking(&pool, quote_id).await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_find_active_booking_id_returns_id() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, quote_id, "confirmed").await;
        let result = find_active_booking_id(&pool, quote_id).await.unwrap();
        assert_eq!(result, Some(booking_id));
    }

    #[tokio::test]
    async fn test_find_active_booking_id_returns_none_for_cancelled() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        insert_test_booking(&pool, quote_id, "cancelled").await;
        let result = find_active_booking_id(&pool, quote_id).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_update_quote_volume_sets_status() {
        let pool = test_db_pool().await;

        let quote_id = insert_test_quote(&pool).await;
        let now = Utc::now();
        update_quote_volume(&pool, quote_id, 15.5, "volume_estimated", now)
            .await
            .unwrap();
        let status = get_quote_status(&pool, quote_id).await;
        assert_eq!(status, "volume_estimated");
    }
}
