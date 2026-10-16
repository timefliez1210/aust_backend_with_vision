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

/// Telegram rejects messages over 4096 characters; stay under it with margin.
const MAX_MESSAGE_CHARS: usize = 4000;

/// Split `body` into Telegram-sized chunks, breaking at a newline (or else a
/// space) near the limit so no line is cut mid-word.
pub fn split_message(body: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest: &str = body;
    while rest.chars().count() > MAX_MESSAGE_CHARS {
        let limit = rest
            .char_indices()
            .nth(MAX_MESSAGE_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let window = &rest[..limit];
        let cut = window
            .rfind('\n')
            .filter(|i| *i > limit / 2)
            .or_else(|| window.rfind(' ').filter(|i| *i > limit / 2))
            .unwrap_or(limit);
        chunks.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() || chunks.is_empty() {
        chunks.push(rest.to_string());
    }
    chunks
}

/// Post one of Josie's replies of any length (split as needed) and return every
/// message ID. Markdown is rendered phone-friendly as Telegram HTML
/// ([`super::telegram_format`]); a chunk Telegram rejects as HTML goes out as plain text.
pub async fn post_long_text(client: &Client, bot_token: &str, chat_id: i64, body: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    for chunk in split_message(body) {
        let html = super::telegram_format::to_telegram_html(&chunk);
        let id = match post_html(client, bot_token, chat_id, &html).await {
            Some(id) => id,
            None => post_text(client, bot_token, chat_id, &chunk).await,
        };
        ids.push(id);
    }
    ids
}

/// Send `html` with `parse_mode: HTML`; `None` when Telegram refuses it.
async fn post_html(client: &Client, bot_token: &str, chat_id: i64, html: &str) -> Option<i64> {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": html,
        "parse_mode": "HTML",
        "link_preview_options": { "is_disabled": true },
    });
    match client.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            let json: serde_json::Value = resp.json().await.unwrap_or_default();
            json["result"]["message_id"].as_i64()
        }
        Ok(resp) => {
            error!("post_html rejected ({}), falling back to plain text: {}", resp.status(), resp.text().await.unwrap_or_default());
            None
        }
        // Not retried as plain text: the message may have gone out anyway.
        Err(e) => {
            error!("post_html network error: {e}");
            Some(0)
        }
    }
}

/// Show "schreibt …" while a turn runs (Telegram clears it after ~5 s or on reply).
pub async fn send_typing(client: &Client, bot_token: &str, chat_id: i64) {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendChatAction");
    let payload = serde_json::json!({ "chat_id": chat_id, "action": "typing" });
    if let Err(e) = client.post(&url).json(&payload).send().await {
        error!("send_typing failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_messages_are_not_split() {
        assert_eq!(split_message("Hallo"), vec!["Hallo".to_string()]);
    }

    #[test]
    fn long_messages_split_at_line_breaks_under_the_limit() {
        let line = "x".repeat(99);
        let body = (0..100).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&body);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= MAX_MESSAGE_CHARS));
        assert!(chunks.iter().all(|c| c.split('\n').all(|l| l.len() == 99)), "no line cut in half");
        assert_eq!(chunks.join("\n"), body);
    }
}
