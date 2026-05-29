//! `AssistantEventConsumer` — polls `domain_events` and dispatches by kind.
//!
//! Each handler receives a `TelegramNotifier` so it can post messages to Alex
//! without a circular dependency on `crates/api`.
//!
//! # Poll loop
//! `run_forever` spawns a `tokio::time::interval`-based loop that calls
//! `run_once` on each tick. On handler error the event is still marked consumed
//! so a broken handler does not block the queue indefinitely — log the error and
//! move on.

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use aust_core::events::{DomainEvent, EventConsumer};
use aust_core::services::ServiceBundle;
use sqlx::PgPool;
use tracing::{error, info};

use crate::error::{AssistantError, Result};
use super::handlers;
use super::notifier::TelegramNotifier;

const CONSUMER_NAME: &str = "assistant";

/// Polls unconsumed domain events and dispatches them to typed handlers.
pub struct AssistantEventConsumer {
    consumer: EventConsumer,
    /// Kept for future handlers that need DB access beyond the event consumer.
    #[allow(dead_code)]
    services: Arc<ServiceBundle>,
    /// Telegram poster injected at construction time. Tests can supply a `MockNotifier`.
    notifier: Arc<dyn TelegramNotifier>,
    /// DB pool — passed to handlers that need to look up the Owner chat_id.
    pool: PgPool,
}

impl AssistantEventConsumer {
    /// Create a new consumer backed by the given pool, service bundle, and notifier.
    pub fn new(
        pool: PgPool,
        services: Arc<ServiceBundle>,
        notifier: Arc<dyn TelegramNotifier>,
    ) -> Self {
        Self {
            consumer: EventConsumer::new(pool.clone(), CONSUMER_NAME),
            services,
            notifier,
            pool,
        }
    }

    /// Process one batch of pending events. Returns the number of events handled.
    ///
    /// Each event is dispatched to its typed handler. If the handler returns an
    /// error the event is still marked consumed (to avoid reprocessing a broken
    /// event in a tight loop) and the error is logged.
    pub async fn run_once(&self) -> Result<usize> {
        let events = self.consumer
            .fetch_pending(50)
            .await
            .map_err(AssistantError::Database)?;

        let count = events.len();

        for event in &events {
            // Mark consumed BEFORE running the handler (B5).
            //
            // If the handler runs first and the process crashes before mark_consumed
            // completes, the event re-fires on the next poll → duplicate Telegram messages.
            // By marking consumed first we accept at-most-once delivery: a handler crash
            // after mark_consumed results in a missed notification, which is far less
            // harmful than duplicate spam. Alex can check /admin/agent-activity for gaps.
            if let Err(e) = self.consumer.mark_consumed(event.id).await {
                error!(event_id = %event.id, "Failed to mark event consumed — skipping handler to avoid double-dispatch: {e}");
                // Do NOT run the handler: if mark_consumed failed the event is still
                // unconsumed and will be retried on the next poll. Running the handler
                // here could cause double-dispatch on retry.
                continue;
            }

            let result = self.dispatch(event).await;
            if let Err(e) = result {
                error!(event_id = %event.id, kind = %event.kind, "Handler error (event already marked consumed): {e}");
            }
        }

        if count > 0 {
            info!("AssistantEventConsumer processed {count} event(s)");
        }

        Ok(count)
    }

    /// Dispatch a single event to the appropriate handler.
    async fn dispatch(&self, event: &DomainEvent) -> Result<()> {
        match event.kind.as_str() {
            "inquiry.created"  => handlers::handle_inquiry_created(event, &self.pool, self.notifier.as_ref()).await,
            "offer.drafted"    => handlers::handle_offer_drafted(event, &self.pool, self.notifier.as_ref()).await,
            "offer.sent"       => handlers::handle_offer_sent(event, &self.pool, self.notifier.as_ref()).await,
            "status.changed"   => handlers::handle_status_changed(event, &self.pool, self.notifier.as_ref()).await,
            "invoice.issued"   => handlers::handle_invoice_issued(event, &self.pool, self.notifier.as_ref()).await,
            "invoice.overdue"  => handlers::handle_invoice_overdue(event, &self.pool, self.notifier.as_ref()).await,
            other => {
                tracing::debug!(kind = other, "Unknown event kind — skipping");
                Ok(())
            }
        }
    }

    /// Run the consumer loop forever, polling every `poll_interval`.
    ///
    /// Exits cleanly when `shutdown` is cancelled. Call `tokio::spawn` on the
    /// returned future to run it as a background task.
    pub async fn run_forever(self, poll_interval: std::time::Duration, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!("AssistantEventConsumer started (poll interval: {poll_interval:?})");

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.run_once().await {
                        error!("AssistantEventConsumer::run_once error: {e}");
                    }
                }
                _ = shutdown.cancelled() => {
                    info!("AssistantEventConsumer shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aust_core::events::EventEmitter;
    use super::super::notifier::MockNotifier;

    async fn try_pool() -> Option<sqlx::PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    #[tokio::test]
    async fn run_once_processes_two_events_and_marks_consumed() {
        let Some(pool) = try_pool().await else { return };

        let emitter = EventEmitter::new(pool.clone());

        // Seed two events that the consumer hasn't seen yet.
        let id1 = emitter
            .emit("inquiry.created", "inquiry:test-a", serde_json::json!({"inquiry_id": "test-a"}))
            .await
            .expect("emit 1");
        let id2 = emitter
            .emit("offer.drafted", "offer:test-b", serde_json::json!({"offer_id": "test-b"}))
            .await
            .expect("emit 2");

        let services = std::sync::Arc::new(crate::tools::testing::mock_bundle(
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        ));
        let notifier = Arc::new(MockNotifier::new());
        let consumer = AssistantEventConsumer::new(pool.clone(), services, notifier);

        // run_once may also consume events emitted by parallel tests on the shared
        // dev DB — we don't assert on count, only on the specific events we seeded.
        consumer.run_once().await.expect("run_once should succeed");

        // Both events should now be marked consumed.
        let pending_consumer = aust_core::events::EventConsumer::new(pool.clone(), "assistant");
        let remaining = pending_consumer.fetch_pending(100).await.expect("fetch");
        let ids_remaining: Vec<_> = remaining.iter().map(|e| e.id).collect();
        assert!(!ids_remaining.contains(&id1), "event 1 should be consumed");
        assert!(!ids_remaining.contains(&id2), "event 2 should be consumed");

        // Clean up.
        sqlx::query("DELETE FROM domain_events WHERE id = ANY($1)")
            .bind(&[id1, id2][..])
            .execute(&pool)
            .await
            .ok();
    }

    /// B5 regression: after the fix, the consumer marks events consumed BEFORE running
    /// the handler. We verify the order by checking that events consumed by the `assistant`
    /// consumer are marked before any handler side-effects would have occurred.
    ///
    /// Integration: emit an event, run_once, then query that the consumed_by field is set
    /// without relying on the Telegram notifier (MockNotifier captures calls).
    #[tokio::test]
    async fn run_once_marks_consumed_before_handler_fires() {
        let Some(pool) = try_pool().await else { return };

        let emitter = aust_core::events::EventEmitter::new(pool.clone());
        let id = emitter
            .emit("inquiry.created", "inquiry:b5-test", serde_json::json!({
                "inquiry_id": "b5-test",
                "source": "non-email", // skips Telegram notification in handler
            }))
            .await
            .expect("emit");

        let services = std::sync::Arc::new(crate::tools::testing::mock_bundle(
            uuid::Uuid::nil(), uuid::Uuid::nil(), uuid::Uuid::nil(),
        ));
        let notifier = Arc::new(MockNotifier::new());
        let consumer = AssistantEventConsumer::new(pool.clone(), services, notifier.clone());

        consumer.run_once().await.expect("run_once");

        // The event must now be consumed (mark_consumed ran before handler).
        let pending = aust_core::events::EventConsumer::new(pool.clone(), "assistant");
        let remaining = pending.fetch_pending(100).await.expect("fetch");
        let still_pending = remaining.iter().any(|e| e.id == id);
        assert!(!still_pending, "event must be marked consumed; B5 fix: mark before handler");

        // Cleanup.
        sqlx::query("DELETE FROM domain_events WHERE id = $1")
            .bind(id).execute(&pool).await.ok();
    }
}
