//! Telegram posting primitives for the assistant bridge.
//!
//! Wraps the raw Telegram Bot API calls used by the assistant:
//! - `post_text` — send a plain-text message, return the Telegram message_id.
//! - `post_pending_action` — post a message with [✅ Bestätigen][✏️ Anpassen][❌ Abbrechen] buttons.
//! - `edit_message_remove_keyboard` — replace a keyed message body and remove buttons after resolution.

use reqwest::Client;
use tracing::error;
use uuid::Uuid;

/// Send a plain-text message to the given chat.
///
/// Returns the Telegram `message_id` of the sent message, or `0` on failure
/// (errors are logged, never propagated — Telegram is best-effort).
pub async fn post_text(client: &Client, bot_token: &str, chat_id: i64, body: &str) -> i64 {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": body,
    });
    match client.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            let json: serde_json::Value = resp.json().await.unwrap_or_default();
            json["result"]["message_id"].as_i64().unwrap_or(0)
        }
        Ok(resp) => {
            error!("post_text failed ({}): {}", resp.status(), resp.text().await.unwrap_or_default());
            0
        }
        Err(e) => {
            error!("post_text network error: {e}");
            0
        }
    }
}

/// Post an inline-keyboard message for a pending action.
///
/// Buttons:
/// - `✅ Bestätigen` → callback_data `pa:<uuid>:confirm`
/// - `✏️ Anpassen`  → callback_data `pa:<uuid>:edit`
/// - `❌ Abbrechen` → callback_data `pa:<uuid>:cancel`
///
/// Returns the Telegram `message_id` of the sent message.
pub async fn post_pending_action(
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_action_id: Uuid,
    summary_de: &str,
) -> i64 {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let keyboard = serde_json::json!({
        "inline_keyboard": [[
            { "text": "✅ Bestätigen", "callback_data": format!("pa:{pending_action_id}:confirm") },
            { "text": "✏️ Anpassen",  "callback_data": format!("pa:{pending_action_id}:edit") },
            { "text": "❌ Abbrechen", "callback_data": format!("pa:{pending_action_id}:cancel") },
        ]]
    });
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": summary_de,
        "reply_markup": keyboard,
    });
    match client.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            let json: serde_json::Value = resp.json().await.unwrap_or_default();
            json["result"]["message_id"].as_i64().unwrap_or(0)
        }
        Ok(resp) => {
            error!("post_pending_action failed ({}): {}", resp.status(), resp.text().await.unwrap_or_default());
            0
        }
        Err(e) => {
            error!("post_pending_action network error: {e}");
            0
        }
    }
}

/// Edit a message to remove its inline keyboard and update its text.
///
/// Used after a pending action is resolved (confirmed / cancelled / expired).
pub async fn edit_message_remove_keyboard(
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    message_id: i64,
    new_body: &str,
) {
    // First update the text.
    let edit_url = format!("https://api.telegram.org/bot{bot_token}/editMessageText");
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": new_body,
    });
    if let Err(e) = client.post(&edit_url).json(&payload).send().await {
        error!("edit_message_remove_keyboard (text) failed: {e}");
    }

    // Then remove the reply_markup (buttons).
    let markup_url = format!("https://api.telegram.org/bot{bot_token}/editMessageReplyMarkup");
    let payload2 = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reply_markup": { "inline_keyboard": [] },
    });
    if let Err(e) = client.post(&markup_url).json(&payload2).send().await {
        error!("edit_message_remove_keyboard (markup) failed: {e}");
    }
}
