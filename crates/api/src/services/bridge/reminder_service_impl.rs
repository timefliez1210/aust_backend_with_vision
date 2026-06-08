//! Bridge impl for `ReminderService`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{ReminderRecord, ReminderService, ServiceError};

pub struct ReminderServiceImpl {
    pool: PgPool,
}

impl ReminderServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Column tuple shared by the create/list/cancel queries.
type Row = (
    Uuid,
    i64,
    String,
    DateTime<Utc>,
    String,
    String,
    bool,
    i32,
    DateTime<Utc>,
);

fn row_to_record(r: Row) -> ReminderRecord {
    let (id, chat_id, text, due_at, recurrence, source, active, fired_count, created_at) = r;
    ReminderRecord {
        id,
        chat_id,
        text,
        due_at,
        recurrence,
        source,
        active,
        fired_count,
        created_at,
    }
}

const SELECT_COLS: &str =
    "id, chat_id, text, due_at, recurrence, source, active, fired_count, created_at";

#[async_trait]
impl ReminderService for ReminderServiceImpl {
    async fn create(
        &self,
        chat_id: i64,
        text: &str,
        due_at: DateTime<Utc>,
        recurring: bool,
    ) -> Result<ReminderRecord, ServiceError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ServiceError::Validation(
                "Erinnerungstext darf nicht leer sein.".to_string(),
            ));
        }
        let recurrence = if recurring { "recurring" } else { "none" };
        let id = Uuid::now_v7();

        let row: Row = sqlx::query_as(&format!(
            "INSERT INTO agent_reminders (id, chat_id, text, due_at, recurrence, source) \
             VALUES ($1, $2, $3, $4, $5, 'manual') RETURNING {SELECT_COLS}"
        ))
        .bind(id)
        .bind(chat_id)
        .bind(text)
        .bind(due_at)
        .bind(recurrence)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(row_to_record(row))
    }

    async fn list(
        &self,
        chat_id: i64,
        active_only: bool,
    ) -> Result<Vec<ReminderRecord>, ServiceError> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM agent_reminders \
             WHERE chat_id = $1 AND ($2::bool = false OR active) \
             ORDER BY active DESC, due_at ASC LIMIT 100"
        ))
        .bind(chat_id)
        .bind(active_only)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows.into_iter().map(row_to_record).collect())
    }

    async fn cancel(&self, id: Uuid) -> Result<ReminderRecord, ServiceError> {
        let row: Option<Row> = sqlx::query_as(&format!(
            "UPDATE agent_reminders SET active = FALSE WHERE id = $1 RETURNING {SELECT_COLS}"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        row.map(row_to_record)
            .ok_or_else(|| ServiceError::NotFound(format!("Erinnerung {id}")))
    }
}
