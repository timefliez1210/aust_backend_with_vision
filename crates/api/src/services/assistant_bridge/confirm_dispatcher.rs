//! Detects `pending_confirmation` payloads from Wave-2 tools and converts them
//! into real `pending_actions` DB rows + Telegram inline-keyboard messages.
//!
//! The driver already calls `confirmation::enqueue` before returning the
//! `TurnResult`. This module is called *after* that, to:
//! 1. Detect the pending-action shape in the driver reply.
//! 2. Post the inline keyboard to Telegram (via `telegram_output::post_pending_action`).
//! 3. Store the resulting Telegram `message_id` on the pending_actions row.

use reqwest::Client;
use sqlx::PgPool;
use uuid::Uuid;
use tracing::warn;

use aust_assistant::confirmation;

use super::telegram_output;

/// If `turn_result` indicates `awaiting_confirmation`, post an inline-keyboard
/// message to Telegram and persist the returned message_id on the pending_actions row.
///
/// No-op when `awaiting_confirmation` is false.
pub async fn maybe_post_keyboard(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_action_id: Option<Uuid>,
    summary_de: &str,
) {
    let Some(pending_id) = pending_action_id else {
        return;
    };

    let message_id = telegram_output::post_pending_action(
        client,
        bot_token,
        chat_id,
        pending_id,
        summary_de,
    )
    .await;

    if message_id != 0 {
        if let Err(e) = confirmation::set_telegram_message_id(pool, pending_id, message_id).await {
            warn!("Could not persist telegram_message_id on pending_action {pending_id}: {e}");
        }
    }
}
