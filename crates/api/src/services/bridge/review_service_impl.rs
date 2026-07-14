//! Bridge impl for `ReviewService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{
    DueReviewRequest, FeedbackRecord, ReviewRecord, ReviewService, ServiceError,
};

use crate::services::billing_reminder_service;

pub struct ReviewServiceImpl {
    pool: PgPool,
    /// Needed to send the Google-review mail on Alex's say-so.
    config: std::sync::Arc<aust_core::Config>,
}

impl ReviewServiceImpl {
    pub fn new(pool: PgPool, config: std::sync::Arc<aust_core::Config>) -> Self {
        Self { pool, config }
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

    async fn create_feedback(
        &self,
        report_type: &str,
        priority: &str,
        title: &str,
        description: Option<&str>,
        location: Option<&str>,
    ) -> Result<FeedbackRecord, ServiceError> {
        // Validate against the table CHECK constraints up front so the agent gets a
        // clear message instead of an opaque DB error.
        if !matches!(report_type, "bug" | "feature") {
            return Err(ServiceError::Validation(format!(
                "report_type muss 'bug' oder 'feature' sein, nicht '{report_type}'."
            )));
        }
        if !matches!(priority, "low" | "medium" | "high" | "critical") {
            return Err(ServiceError::Validation(format!(
                "priority muss low/medium/high/critical sein, nicht '{priority}'."
            )));
        }
        let title = title.trim();
        if title.is_empty() {
            return Err(ServiceError::Validation(
                "title darf nicht leer sein.".to_string(),
            ));
        }

        let row: (Uuid, String, Option<String>, String, chrono::DateTime<chrono::Utc>) =
            sqlx::query_as(
                r#"
                INSERT INTO feedback_reports (report_type, priority, title, description, location)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id, title, description, status, created_at
                "#,
            )
            .bind(report_type)
            .bind(priority)
            .bind(title)
            .bind(description)
            .bind(location)
            .fetch_one(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

        let (id, title, description, status, created_at) = row;
        Ok(FeedbackRecord {
            id,
            inquiry_id: None,
            category: Some(format!("{report_type}/{priority}")),
            description: description.unwrap_or_else(|| title.clone()),
            resolved: status == "resolved",
            notes: None,
            created_at,
        })
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

    async fn list_due_review_requests(&self) -> Result<Vec<DueReviewRequest>, ServiceError> {
        let rows = billing_reminder_service::list_due_review_requests(&self.pool)
            .await
            .map_err(super::map_api)?;
        Ok(rows
            .into_iter()
            .map(|r| DueReviewRequest {
                inquiry_id: r.inquiry_id,
                remind_after: r.remind_after,
                days_overdue: r.days_overdue,
                customer_name: r.customer_name,
                customer_email: r.customer_email,
            })
            .collect())
    }

    async fn decide_review_request(
        &self,
        inquiry_id: Uuid,
        action: &str,
        remind_after_days: Option<u32>,
    ) -> Result<String, ServiceError> {
        let outcome = billing_reminder_service::decide_review_request(
            &self.pool,
            &self.config.email,
            inquiry_id,
            action,
            remind_after_days,
        )
        .await
        .map_err(super::map_api)?;
        Ok(outcome.status.to_string())
    }
}
