//! Bridge impl for `EstimationService`.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{EstimationService, EstimationSummary, RevisionStatus, ServiceError};

pub struct EstimationServiceImpl {
    pool: PgPool,
}

impl EstimationServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EstimationService for EstimationServiceImpl {
    async fn get(&self, inquiry_id: Uuid) -> Result<Option<EstimationSummary>, ServiceError> {
        let row: Option<(
            Uuid,
            String,
            String,
            Option<f64>,
            Option<f64>,
            Option<i64>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT id, method, status,
                   total_volume_m3,
                   confidence_score,
                   COALESCE(jsonb_array_length(result_data->'items'), 0)::bigint AS item_count,
                   created_at
            FROM volume_estimations
            WHERE inquiry_id = $1 AND status = 'completed'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(inquiry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(row.map(
            |(id, method, status, total_volume_m3, confidence_score, item_count, created_at)| {
                EstimationSummary {
                    id,
                    method,
                    status,
                    total_volume_m3,
                    confidence_score,
                    item_count: item_count.unwrap_or(0),
                    created_at,
                }
            },
        ))
    }

    async fn override_volume(
        &self,
        inquiry_id: Uuid,
        volume_m3: f64,
        _notes: Option<&str>,
    ) -> Result<(), ServiceError> {
        sqlx::query(
            "UPDATE inquiries SET estimated_volume_m3 = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(volume_m3)
        .bind(inquiry_id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn request_revision(&self, inquiry_id: Uuid) -> Result<RevisionStatus, ServiceError> {
        // Check whether any vision estimations exist for this inquiry — a prerequisite for
        // a meaningful revision. Without photos the worker has nothing to re-process.
        let has_vision: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM volume_estimations
                WHERE inquiry_id = $1 AND method = 'vision'
            )
            "#,
        )
        .bind(inquiry_id)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        if !has_vision {
            return Ok(RevisionStatus {
                queued: false,
                reason: Some(
                    "Keine Fotos hochgeladen — Vision-Worker hat kein Material zum Neuverarbeiten."
                        .to_string(),
                ),
                request_id: None,
            });
        }

        // Insert the revision request; the vision worker polls this table.
        let request_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO vision_revision_requests (inquiry_id)
            VALUES ($1)
            RETURNING id
            "#,
        )
        .bind(inquiry_id)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(RevisionStatus {
            queued: true,
            reason: None,
            request_id: Some(request_id),
        })
    }
}
