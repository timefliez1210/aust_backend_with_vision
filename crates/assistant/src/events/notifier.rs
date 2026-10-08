//! `TelegramNotifier` trait — decouples assistant event handlers from the
//! concrete Telegram HTTP client, which lives in `crates/api`.
//!
//! The api crate provides a real implementation; tests inject a `MockNotifier`.

use async_trait::async_trait;

use aust_core::notifications::{self, NotificationKind};
use sqlx::PgPool;

use crate::error::Result;

/// Send plain-text messages to a Telegram chat.
///
/// The implementation lives in `crates/api::services::assistant_bridge` to avoid
/// a circular dependency (`assistant → api → assistant`). The assistant crate only
/// sees this trait.
#[async_trait]
pub trait TelegramNotifier: Send + Sync {
    /// Post a plain-text message to the given chat and return the Telegram message ID.
    async fn post(&self, chat_id: i64, body: String) -> Result<i64>;

    /// Post with an inline keyboard (`reply_markup`).
    async fn post_with_markup(
        &self,
        chat_id: i64,
        body: String,
        markup: serde_json::Value,
    ) -> Result<i64>;
}

/// Post a mutable notification: skipped when the office muted `kind`, otherwise
/// sent with a "🔕 Stumm schalten" button. Returns `None` when muted.
pub async fn notify(
    pool: &PgPool,
    notifier: &dyn TelegramNotifier,
    chat_id: i64,
    kind: NotificationKind,
    body: String,
) -> Result<Option<i64>> {
    if notifications::is_muted(pool, kind).await {
        return Ok(None);
    }
    notifier
        .post_with_markup(chat_id, body, notifications::mute_keyboard(kind))
        .await
        .map(Some)
}

// ── Mock for tests ────────────────────────────────────────────────────────────

/// A `TelegramNotifier` that records every call for assertion in unit tests.
pub struct MockNotifier {
    pub calls: std::sync::Mutex<Vec<(i64, String)>>,
}

impl MockNotifier {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(vec![]),
        }
    }

    pub fn recorded(&self) -> Vec<(i64, String)> {
        self.calls.lock().expect("mutex poisoned").clone()
    }
}

impl Default for MockNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TelegramNotifier for MockNotifier {
    async fn post(&self, chat_id: i64, body: String) -> Result<i64> {
        self.calls
            .lock()
            .expect("mutex poisoned")
            .push((chat_id, body));
        Ok(0)
    }

    async fn post_with_markup(
        &self,
        chat_id: i64,
        body: String,
        _markup: serde_json::Value,
    ) -> Result<i64> {
        self.post(chat_id, body).await
    }
}
