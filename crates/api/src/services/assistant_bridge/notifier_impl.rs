//! Concrete `TelegramNotifier` implementation using the Telegram Bot API.
//!
//! Posts plain-text messages to a given chat via `sendMessage` and returns the
//! Telegram message ID.

use async_trait::async_trait;
use reqwest::Client;
use tracing::error;

use aust_assistant::TelegramNotifier;
use aust_assistant::AssistantError;

pub struct TelegramNotifierImpl {
    client: Client,
    bot_token: String,
}

impl TelegramNotifierImpl {
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            bot_token: bot_token.into(),
        }
    }
}

#[async_trait]
impl TelegramNotifier for TelegramNotifierImpl {
    async fn post(&self, chat_id: i64, body: String) -> aust_assistant::Result<i64> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text": body,
        });

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| AssistantError::Internal(format!("Telegram sendMessage failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Telegram sendMessage failed ({status}): {body}");
            return Err(AssistantError::Internal(format!(
                "Telegram HTTP {status}: {body}"
            )));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AssistantError::Internal(format!("Parse sendMessage response: {e}")))?;

        let message_id = json["result"]["message_id"]
            .as_i64()
            .unwrap_or(0);

        Ok(message_id)
    }
}
