//! Domain event helpers — `EventEmitter` and `EventConsumer`.
//!
//! Domain events are lightweight rows in `domain_events` that record business
//! facts (inquiry created, offer drafted, invoice issued, …). They are written
//! by the API layer and consumed by the assistant's event loop.
//!
//! **Failure policy**: emission is non-fatal. If `emit` fails the caller logs a
//! warning and continues; the system of record is the primary transaction.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// A domain event row as returned by `EventConsumer::fetch_pending`.
#[derive(Debug, Clone)]
pub struct DomainEvent {
    pub id: Uuid,
    pub kind: String,
    pub aggregate: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

// ── Emitter ───────────────────────────────────────────────────────────────────

/// Writes domain events to the `domain_events` table.
///
/// Holds a pool reference; cheap to clone alongside `AppState`.
#[derive(Clone)]
pub struct EventEmitter {
    pool: PgPool,
}

impl EventEmitter {
    /// Create a new emitter backed by the given pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a new domain event and return its UUID.
    ///
    /// # Parameters
    /// - `kind` — event kind string, e.g. `"inquiry.created"`
    /// - `aggregate` — aggregate reference, e.g. `"inquiry:<uuid>"`
    /// - `payload` — small JSON payload with the relevant IDs
    pub async fn emit(
        &self,
        kind: &str,
        aggregate: &str,
        payload: Value,
    ) -> Result<Uuid, sqlx::Error> {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO domain_events (id, kind, aggregate, payload, created_at)
            VALUES ($1, $2, $3, $4, now())
            "#,
        )
        .bind(id)
        .bind(kind)
        .bind(aggregate)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }
}

// ── Consumer ──────────────────────────────────────────────────────────────────

/// Reads pending domain events and marks them consumed for a named consumer.
#[derive(Clone)]
pub struct EventConsumer {
    pool: PgPool,
    consumer_name: String,
}

impl EventConsumer {
    /// Create a consumer that tracks consumption under `consumer_name`.
    pub fn new(pool: PgPool, consumer_name: impl Into<String>) -> Self {
        Self {
            pool,
            consumer_name: consumer_name.into(),
        }
    }

    /// Fetch up to `limit` events not yet consumed by this consumer.
    pub async fn fetch_pending(&self, limit: u32) -> Result<Vec<DomainEvent>, sqlx::Error> {
        let rows: Vec<(Uuid, String, String, Value, DateTime<Utc>)> = sqlx::query_as(
            r#"
            SELECT id, kind, aggregate, payload, created_at
            FROM domain_events
            WHERE NOT (consumed_by ? $1)
            ORDER BY created_at ASC
            LIMIT $2
            "#,
        )
        .bind(&self.consumer_name)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, kind, aggregate, payload, created_at)| DomainEvent {
                id,
                kind,
                aggregate,
                payload,
                created_at,
            })
            .collect())
    }

    /// Mark an event as consumed by this consumer.
    ///
    /// Uses a JSONB merge so multiple consumers can each record their own mark
    /// without clobbering each other.
    pub async fn mark_consumed(&self, event_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE domain_events
            SET consumed_by = consumed_by || jsonb_build_object($1, now()::text)
            WHERE id = $2
            "#,
        )
        .bind(&self.consumer_name)
        .bind(event_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: open a pool from `DATABASE_URL` env var. Silently skips the test
    /// if the variable is unset (CI without DB).
    async fn try_pool() -> Option<sqlx::PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    #[tokio::test]
    async fn emit_fetch_mark_consumed_roundtrip() {
        let Some(pool) = try_pool().await else { return };

        let emitter = EventEmitter::new(pool.clone());
        let consumer = EventConsumer::new(pool.clone(), "test_consumer");

        let payload = serde_json::json!({"inquiry_id": "test-123"});
        let event_id = emitter.emit("test.event", "inquiry:test-123", payload).await
            .expect("emit should succeed");

        let pending = consumer.fetch_pending(10).await.expect("fetch_pending should succeed");
        assert!(
            pending.iter().any(|e| e.id == event_id),
            "emitted event should appear in pending list"
        );

        consumer.mark_consumed(event_id).await.expect("mark_consumed should succeed");

        let pending2 = consumer.fetch_pending(10).await.expect("second fetch_pending should succeed");
        assert!(
            !pending2.iter().any(|e| e.id == event_id),
            "event should not appear in pending list after being consumed"
        );

        // Clean up
        sqlx::query("DELETE FROM domain_events WHERE id = $1")
            .bind(event_id)
            .execute(&pool)
            .await
            .ok();
    }
}
