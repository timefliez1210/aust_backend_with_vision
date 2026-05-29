//! Bridge impl for `TodoService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{ServiceError, TodoRecord, TodoService};

pub struct TodoServiceImpl {
    pool: PgPool,
}

impl TodoServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TodoService for TodoServiceImpl {
    async fn create(
        &self,
        session_id: Uuid,
        text: &str,
        due: Option<NaiveDate>,
    ) -> Result<TodoRecord, ServiceError> {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO agent_todos (id, session_id, text, due, status)
            VALUES ($1, $2, $3, $4, 'open')
            "#,
        )
        .bind(id)
        .bind(session_id)
        .bind(text)
        .bind(due)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let row: (
            Uuid,
            Uuid,
            String,
            Option<NaiveDate>,
            String,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(
            "SELECT id, session_id, text, due, status, created_at, resolved_at FROM agent_todos WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, session_id, text, due, status, created_at, resolved_at) = row;
        Ok(TodoRecord {
            id,
            session_id,
            text,
            due,
            status,
            created_at,
            resolved_at,
        })
    }

    async fn list(
        &self,
        session_id: Uuid,
        open_only: bool,
    ) -> Result<Vec<TodoRecord>, ServiceError> {
        let rows: Vec<(
            Uuid,
            Uuid,
            String,
            Option<NaiveDate>,
            String,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            r#"
            SELECT id, session_id, text, due, status, created_at, resolved_at
            FROM agent_todos
            WHERE session_id = $1 AND ($2::bool = false OR status = 'open')
            ORDER BY created_at DESC
            LIMIT 100
            "#,
        )
        .bind(session_id)
        .bind(open_only)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(
                |(id, session_id, text, due, status, created_at, resolved_at)| TodoRecord {
                    id,
                    session_id,
                    text,
                    due,
                    status,
                    created_at,
                    resolved_at,
                },
            )
            .collect())
    }

    async fn resolve(&self, id: Uuid) -> Result<(), ServiceError> {
        sqlx::query(
            "UPDATE agent_todos SET status = 'resolved', resolved_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(())
    }
}
