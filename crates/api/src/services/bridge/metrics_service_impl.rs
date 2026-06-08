//! Bridge impl for `MetricsService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;

use aust_core::services::{DailyMetrics, MetricsService, PipelineMetrics, ServiceError};

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

    async fn daily(&self, date: NaiveDate) -> Result<DailyMetrics, ServiceError> {
        // Inquiries created on the day.
        let inquiries_created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inquiries WHERE created_at::date = $1",
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Offers sent on the day (timestamped on the inquiry).
        let offers_sent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inquiries WHERE offer_sent_at::date = $1",
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Offers accepted on the day (timestamped on the inquiry).
        let offers_accepted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inquiries WHERE accepted_at::date = $1",
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Jobs whose move is scheduled FOR this day (overlap for multi-day).
        let jobs_scheduled: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM inquiries
            WHERE scheduled_date IS NOT NULL
              AND scheduled_date <= $1
              AND COALESCE(end_date, scheduled_date) >= $1
              AND status NOT IN ('cancelled', 'rejected', 'expired')
            "#,
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let invoices_created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invoices WHERE created_at::date = $1",
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Revenue from offers accepted on the day.
        let revenue_brutto_cents: i64 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(o.price_cents), 0)::bigint
            FROM offers o
            JOIN inquiries i ON o.inquiry_id = i.id
            WHERE o.status = 'accepted'
              AND i.accepted_at::date = $1
            "#,
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let revenue_accepted_netto_cents = (revenue_brutto_cents as f64 / 1.19).round() as i64;

        Ok(DailyMetrics {
            date,
            inquiries_created,
            offers_sent,
            offers_accepted,
            jobs_scheduled,
            invoices_created,
            revenue_accepted_netto_cents,
        })
    }
}
