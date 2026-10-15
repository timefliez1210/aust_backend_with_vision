//! Bridge impl for `EmailService`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::events::EventEmitter;
use aust_core::services::{EmailDetail, EmailDraft, EmailService, EmailSummary, ServiceError};

use crate::repositories::{admin_repo, email_repo};
use crate::ApiError;

pub struct EmailServiceImpl {
    pool: PgPool,
    config: Arc<aust_core::Config>,
    storage: Arc<dyn aust_storage::StorageProvider>,
    events: EventEmitter,
}

impl EmailServiceImpl {
    pub fn new(
        pool: PgPool,
        config: Arc<aust_core::Config>,
        storage: Arc<dyn aust_storage::StorageProvider>,
    ) -> Self {
        let events = EventEmitter::new(pool.clone());
        Self { pool, config, storage, events }
    }
}

fn api_err(e: ApiError) -> ServiceError {
    match e {
        ApiError::NotFound(m) => ServiceError::NotFound(m),
        ApiError::Validation(m) | ApiError::BadRequest(m) => ServiceError::Validation(m),
        other => ServiceError::Db(anyhow::anyhow!(other.to_string())),
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
        crate::routes::admin_emails::send_plain_email(&self.config.email(), to, subject, body)
            .await
            .map_err(|e| ServiceError::Db(anyhow::anyhow!("E-Mail-Versand fehlgeschlagen: {e}")))?;
        Ok(())
    }

    async fn create_reply_draft(&self, email_id: Uuid, body: &str) -> Result<EmailDraft, ServiceError> {
        let thread_id: Uuid = sqlx::query_scalar("SELECT thread_id FROM email_messages WHERE id = $1")
            .bind(email_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(super::map_sqlx)?
            .ok_or_else(|| ServiceError::NotFound(format!("E-Mail {email_id}")))?;
        // Same recipient resolution and row as the admin reply composer.
        let (_customer, recipient, subject) = admin_repo::fetch_thread_for_reply(&self.pool, thread_id)
            .await
            .map_err(super::map_sqlx)?
            .ok_or_else(|| ServiceError::NotFound(format!("E-Mail-Thread {thread_id}")))?;
        let to_address = recipient.unwrap_or_default();
        let id = Uuid::now_v7();
        admin_repo::insert_reply_draft(
            &self.pool,
            id,
            thread_id,
            &self.config.email().from_address,
            &to_address,
            subject.as_deref(),
            body,
            chrono::Utc::now(),
        )
        .await
        .map_err(super::map_sqlx)?;
        Ok(EmailDraft { id, thread_id, to_address, subject, body: body.to_string() })
    }

    async fn create_offer_draft(&self, inquiry_id: Uuid, body: Option<&str>) -> Result<EmailDraft, ServiceError> {
        let email: Option<String> = sqlx::query_scalar(
            "SELECT c.email FROM inquiries i JOIN customers c ON c.id = i.customer_id WHERE i.id = $1",
        )
        .bind(inquiry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?
        .flatten();
        let to_address = email
            .filter(|e| e.contains('@'))
            .ok_or_else(|| ServiceError::Validation("Für diesen Kunden ist keine E-Mail-Adresse hinterlegt.".into()))?;
        // The thread the Telegram ✅ approval also uses, so the KVA goes out in the
        // customer's existing conversation and its PDF is attached on send.
        let thread_id = crate::services::email_dispatch::find_or_create_offer_thread(&self.pool, inquiry_id, &to_address)
            .await
            .ok_or_else(|| ServiceError::Db(anyhow::anyhow!("E-Mail-Thread konnte nicht angelegt werden")))?;
        let profile = crate::repositories::tenant_repo::profile(&self.pool).await.map_err(super::map_sqlx)?;
        let subject = crate::services::email::offer_mail_subject(&profile);
        let body = body
            .map(str::to_string)
            .unwrap_or_else(|| crate::services::email::offer_mail_draft_body(&profile));
        let id = Uuid::now_v7();
        email_repo::insert_message(
            &self.pool,
            id,
            thread_id,
            "outbound",
            &self.config.email().from_address,
            &to_address,
            &subject,
            &body,
            false,
            "draft",
        )
        .await
        .map_err(super::map_sqlx)?;
        Ok(EmailDraft { id, thread_id, to_address, subject: Some(subject), body })
    }

    async fn get_draft(&self, draft_id: Uuid) -> Result<EmailDraft, ServiceError> {
        let d = admin_repo::fetch_draft_for_send(&self.pool, draft_id)
            .await
            .map_err(super::map_sqlx)?
            .ok_or_else(|| ServiceError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;
        Ok(EmailDraft {
            id: draft_id,
            thread_id: d.thread_id,
            to_address: d.to_address.unwrap_or_default(),
            subject: d.subject,
            body: d.body_text.unwrap_or_default(),
        })
    }

    async fn send_draft(&self, draft_id: Uuid) -> Result<String, ServiceError> {
        crate::services::email_drafts::send_draft(&self.pool, self.storage.as_ref(), &self.config, &self.events, draft_id)
            .await
            .map_err(api_err)
    }
}
