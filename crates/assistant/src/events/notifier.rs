//! `TelegramNotifier` trait — decouples assistant event handlers from the
//! concrete Telegram HTTP client, which lives in `crates/api`.
//!
//! The api crate provides a real implementation; tests inject a `MockNotifier`.

use async_trait::async_trait;

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
}
