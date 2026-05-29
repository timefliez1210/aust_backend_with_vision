//! Bridge impl for `MetricsService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;

use aust_core::services::{MetricsService, PipelineMetrics, ServiceError};

pub struct MetricsServiceImpl {
    pool: PgPool,
}

impl MetricsServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MetricsService for MetricsServiceImpl {
    async fn pipeline(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<PipelineMetrics, ServiceError> {
        let inquiries_total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inquiries WHERE created_at::date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let offers_sent: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM offers
            WHERE created_at::date BETWEEN $1 AND $2
              AND status NOT IN ('draft', 'rejected', 'cancelled')
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let scheduled: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM inquiries
            WHERE updated_at::date BETWEEN $1 AND $2
              AND status::text IN ('scheduled', 'completed', 'invoiced', 'paid')
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let invoiced: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invoices WHERE created_at::date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let paid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invoices WHERE status = 'paid' AND created_at::date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let revenue_brutto_cents: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(o.price_cents), 0)::bigint
            FROM offers o
            JOIN inquiries i ON o.inquiry_id = i.id
            WHERE o.status = 'accepted'
              AND i.updated_at::date BETWEEN $1 AND $2
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let revenue_netto_cents = (revenue_brutto_cents as f64 / 1.19).round() as i64;

        Ok(PipelineMetrics {
            period_from: from,
            period_to: to,
            inquiries_total,
            offers_sent,
            scheduled,
            invoiced,
            paid,
            revenue_netto_cents,
        })
    }
}
