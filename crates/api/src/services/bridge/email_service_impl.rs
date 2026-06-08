//! Bridge impl for `EmailService`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{EmailDetail, EmailService, EmailSummary, ServiceError};

pub struct EmailServiceImpl {
    pool: PgPool,
    config: Arc<aust_core::Config>,
}

impl EmailServiceImpl {
    pub fn new(pool: PgPool, config: Arc<aust_core::Config>) -> Self {
        Self { pool, config }
    }
}

#[async_trait]
impl EmailService for EmailServiceImpl {
    async fn list_inbox(&self, limit: u32) -> Result<Vec<EmailSummary>, ServiceError> {
        let limit_i = limit.min(50) as i32;
        let rows: Vec<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT id, subject, from_address, status, created_at
            FROM email_messages
            ORDER BY created_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit_i)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, subject, from_address, status, created_at)| EmailSummary {
                id,
                subject,
                from_address,
                status,
                created_at,
            })
            .collect())
    }

    async fn get_email(&self, id: Uuid) -> Result<EmailDetail, ServiceError> {
        let row: Option<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT id, subject, from_address, to_address, body_text, status, direction, created_at
            FROM email_messages
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, subject, from_address, to_address, body_text, status, direction, created_at) =
            row.ok_or_else(|| ServiceError::NotFound(format!("E-Mail {id}")))?;

        Ok(EmailDetail {
            id,
            subject,
            from_address,
            to_address,
            body_text,
            status,
            direction,
            created_at,
        })
    }

    async fn list_thread(&self, customer_id: Uuid) -> Result<Vec<EmailDetail>, ServiceError> {
        let rows: Vec<(
            Uuid,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT m.id, COALESCE(m.subject, '')::text AS subject,
                   m.from_address, m.to_address, m.body_text, m.status,
                   m.direction, m.created_at
            FROM email_messages m
            JOIN email_threads t ON m.thread_id = t.id
            WHERE t.customer_id = $1
            ORDER BY m.created_at ASC
            LIMIT 100
            "#,
        )
        .bind(customer_id)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(
                |(id, subject, from_address, to_address, body_text, status, direction, created_at)| {
                    EmailDetail {
                        id,
                        subject,
                        from_address,
                        to_address,
                        body_text,
                        status,
                        direction,
                        created_at,
                    }
                },
            )
            .collect())
    }

    async fn mark_handled(&self, id: Uuid) -> Result<(), ServiceError> {
        sqlx::query("UPDATE email_messages SET status = 'handled' WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn categorize(&self, id: Uuid, label: &str) -> Result<(), ServiceError> {
        sqlx::query("UPDATE email_messages SET status = $1 WHERE id = $2")
            .bind(label)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), ServiceError> {
        let to = to.trim();
        if to.is_empty() || !to.contains('@') {
            return Err(ServiceError::Validation(format!(
                "Ungültige Empfängeradresse: '{to}'."
            )));
        }
        // Note: ad-hoc sends are not persisted into email_messages — that table
        // requires a thread_id (NOT NULL, FK to email_threads) and these messages
        // by definition have no thread. Threaded correspondence goes via draft_reply.
        crate::routes::admin_emails::send_plain_email(&self.config.email, to, subject, body)
            .await
            .map_err(|e| ServiceError::Db(anyhow::anyhow!("E-Mail-Versand fehlgeschlagen: {e}")))?;
        Ok(())
    }
}
