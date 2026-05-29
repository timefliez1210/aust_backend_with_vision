//! Bridge impl for `ReviewService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{
    FeedbackRecord, ReviewRecord, ReviewService, ServiceError,
};

pub struct ReviewServiceImpl {
    pool: PgPool,
}

impl ReviewServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ReviewService for ReviewServiceImpl {
    async fn list_reviews(
        &self,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
    ) -> Result<Vec<ReviewRecord>, ServiceError> {
        // The schema tracks "review_requests" — proxy for review records.
        let rows: Vec<(Uuid, Option<Uuid>, String, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            r#"
            SELECT id, inquiry_id, status::text, response_draft, created_at
            FROM review_requests
            WHERE ($1::date IS NULL OR created_at::date >= $1)
              AND ($2::date IS NULL OR created_at::date <= $2)
            ORDER BY created_at DESC
            LIMIT 100
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, inquiry_id, status, response_draft, created_at)| ReviewRecord {
                id,
                inquiry_id,
                rating: None,
                text: Some(status), // store status as text for now
                response_draft,
                created_at,
            })
            .collect())
    }

    async fn list_feedback(
        &self,
        unresolved_only: bool,
    ) -> Result<Vec<FeedbackRecord>, ServiceError> {
        let rows: Vec<(Uuid, String, Option<String>, String, chrono::DateTime<chrono::Utc>)> =
            sqlx::query_as(
                r#"
                SELECT id, title, description, status, created_at
                FROM feedback_reports
                WHERE ($1::bool = false OR status != 'resolved')
                ORDER BY created_at DESC
                LIMIT 100
                "#,
            )
            .bind(unresolved_only)
            .fetch_all(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, title, description, status, created_at)| FeedbackRecord {
                id,
                inquiry_id: None,
                category: Some(title),
                description: description.unwrap_or_default(),
                resolved: status == "resolved",
                notes: None,
                created_at,
            })
            .collect())
    }

    async fn set_review_response_draft(
        &self,
        id: Uuid,
        draft: &str,
    ) -> Result<(), ServiceError> {
        sqlx::query(
            r#"
            UPDATE review_requests
            SET response_draft = $1,
                response_draft_updated_at = NOW(),
                updated_at = NOW()
            WHERE id = $2
            "#,
        )
        .bind(draft)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn mark_feedback_resolved(
        &self,
        id: Uuid,
        _notes: Option<&str>,
    ) -> Result<(), ServiceError> {
        sqlx::query(
            "UPDATE feedback_reports SET status = 'resolved', updated_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(())
    }
}
