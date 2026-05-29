//! Routes incoming Telegram updates to the assistant driver.
//!
//! Called from `orchestrator.rs` when the existing Telegram poller receives a
//! text message or callback query from a chat that is registered in
//! `telegram_chat_bindings`.
//!
//! # Routing logic
//! - Text message in a *bound* chat → `driver::process_turn`.
//! - Callback `pa:<uuid>:<action>` → inline-button resolution handler.
//! - Anything else (unbound chat, unrecognized callback) → falls through to the
//!   legacy orchestrator so the existing offer-approval flow continues to work
//!   when `agent_owns_approval = false`.

use reqwest::Client;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use aust_assistant::confirmation::{self, Resolution};
use aust_assistant::driver::{self, Input, ResumeParams};
use aust_assistant::bindings;
use aust_assistant::{Soul, ToolRegistry};
use aust_assistant::llm::AssistantLlmProvider;

use super::confirm_dispatcher;
use super::telegram_output;

// ── Public entry points ───────────────────────────────────────────────────────

/// Handle a free-text message from a Telegram chat.
///
/// Returns `true` if the message was handled by the agent (the caller should
/// not route it further). Returns `false` if the chat is unbound and the
/// message should fall through to the legacy orchestrator.
pub async fn handle_text_message(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    text: &str,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) -> bool {
    // Check binding — only agent-bound chats are handled here.
    match bindings::resolve(pool, chat_id).await {
        Err(_) => {
            // Unbound chat — fall through to legacy handler.
            return false;
        }
        Ok(_) => {}
    }

    info!(chat_id, "Agent handling text message");

    let input = Input {
        text: text.to_string(),
        chat_id,
    };

    match driver::process_turn(pool, llm, registry, soul, services, input).await {
        Ok(result) => {
            if result.awaiting_confirmation {
                // M3: avoid the duplicate message — when awaiting confirmation, the
                // keyboard message body IS the user-visible reply. Post it once,
                // attached to the inline keyboard, with the rich German summary
                // built by the tool's `summarize()`.
                let summary = result
                    .pending_summary_de
                    .as_deref()
                    .unwrap_or(result.reply.as_str());
                confirm_dispatcher::maybe_post_keyboard(
                    pool,
                    client,
                    bot_token,
                    chat_id,
                    result.pending_action_id,
                    summary,
                )
                .await;
            } else {
                telegram_output::post_text(client, bot_token, chat_id, &result.reply).await;
            }
        }
        Err(e) => {
            warn!(chat_id, "driver::process_turn error: {e}");
            telegram_output::post_text(
                client,
                bot_token,
                chat_id,
                "⚠️ Es ist ein Fehler aufgetreten. Bitte versuche es erneut.",
            )
            .await;
        }
    }

    true
}

/// Handle a `pa:<uuid>:<action>` callback query (inline button tap).
///
/// Returns `true` if the callback was consumed. Returns `false` for unknown
/// callback prefixes so they fall through to the legacy handler.
pub async fn handle_callback_query(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    callback_data: &str,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    services: aust_core::services::ServiceBundle,
) -> bool {
    // Parse `pa:<uuid>:<action>`
    let parts: Vec<&str> = callback_data.splitn(3, ':').collect();
    if parts.len() != 3 || parts[0] != "pa" {
        return false; // not our prefix
    }

    let pending_id = match Uuid::parse_str(parts[1]) {
        Ok(id) => id,
        Err(_) => return false,
    };
    let action = parts[2];

    // Load the pending action to get the session and tool info.
    let pending = match confirmation::fetch(pool, pending_id).await {
        Ok(p) => p,
        Err(e) => {
            warn!("pa callback: could not fetch pending_action {pending_id}: {e}");
            telegram_output::post_text(client, bot_token, chat_id, "⚠️ Aktion nicht gefunden oder bereits erledigt.").await;
            return true;
        }
    };

    if pending.status != "pending" {
        telegram_output::post_text(client, bot_token, chat_id, "⚠️ Diese Aktion ist bereits erledigt.").await;
        return true;
    }

    // Resolve the binding to get the role and user_id.
    let binding = match bindings::resolve(pool, chat_id).await {
        Ok(b) => b,
        Err(_) => {
            telegram_output::post_text(client, bot_token, chat_id, "Dieser Chat ist nicht freigeschaltet.").await;
            return true;
        }
    };

    match action {
        "confirm" => {
            handle_confirm(
                pool, client, bot_token, chat_id, pending_id, &pending,
                llm, registry, services, binding,
            ).await;
        }
        "cancel" => {
            handle_cancel(pool, client, bot_token, chat_id, pending_id, &pending).await;
        }
        "edit" => {
            handle_edit_request(pool, client, bot_token, chat_id, pending_id, &pending).await;
        }
        other => {
            warn!("Unknown pa action: {other}");
            return false;
        }
    }

    true
}

// ── Resolution sub-handlers ───────────────────────────────────────────────────

async fn handle_confirm(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_id: Uuid,
    pending: &aust_assistant::confirmation::PendingAction,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    services: aust_core::services::ServiceBundle,
    binding: aust_assistant::bindings::ChatBinding,
) {
    let params = ResumeParams {
        pending_id,
        resolution: Resolution::Confirmed,
        role: binding.role,
        user_id: binding.user_id,
        chat_id,
    };

    match driver::resume_confirmed(pool, llm, registry, services, params).await {
        Ok(result) => {
            let summary = result["summary"].as_str()
                .unwrap_or(result.to_string().as_str())
                .to_string();
            let body = format!("✅ Bestätigt: {summary}");

            // Update the original message to remove keyboard.
            if let Some(msg_id) = pending.telegram_message_id {
                telegram_output::edit_message_remove_keyboard(
                    client, bot_token, chat_id, msg_id, &body,
                ).await;
            } else {
                telegram_output::post_text(client, bot_token, chat_id, &body).await;
            }
        }
        Err(e) => {
            warn!("resume_confirmed error: {e}");
            telegram_output::post_text(
                client, bot_token, chat_id,
                &format!("⚠️ Fehler bei der Ausführung: {e}"),
            ).await;
        }
    }
}

async fn handle_cancel(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_id: Uuid,
    pending: &aust_assistant::confirmation::PendingAction,
) {
    if let Err(e) = confirmation::resolve(pool, pending_id, Resolution::Canceled).await {
        warn!("cancel pending_action {pending_id}: {e}");
    }

    let body = "❌ Abgebrochen.";
    if let Some(msg_id) = pending.telegram_message_id {
        telegram_output::edit_message_remove_keyboard(client, bot_token, chat_id, msg_id, body).await;
    } else {
        telegram_output::post_text(client, bot_token, chat_id, body).await;
    }
}

async fn handle_edit_request(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_id: Uuid,
    pending: &aust_assistant::confirmation::PendingAction,
) {
    // Mark as 'edited' so it's no longer pending.
    // The user's follow-up text will be routed back through the driver with
    // context that they're overriding pending action <id>.
    if let Err(e) = confirmation::resolve(
        pool,
        pending_id,
        Resolution::Edited(pending.proposed_args.clone()),
    )
    .await
    {
        warn!("edit pending_action {pending_id}: {e}");
    }

    let body = "✏️ Was soll ich ändern? Schreib mir deine Anpassung.";
    if let Some(msg_id) = pending.telegram_message_id {
        telegram_output::edit_message_remove_keyboard(client, bot_token, chat_id, msg_id, body).await;
    } else {
        telegram_output::post_text(client, bot_token, chat_id, body).await;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Routing logic tests for the callback parser.
///
/// The bridge handles three callback shapes:
/// - prefix != "pa"  → fall-through to legacy (return false)
/// - prefix == "pa" but UUID is invalid → return false
/// - prefix == "pa", valid UUID, known action → return true (handled by agent)
///
/// These tests exercise only the parsing path, not the DB or Telegram network.
#[cfg(test)]
mod tests {
    /// Parse a `pa:<uuid>:<action>` callback data string.
    ///
    /// Returns `None` if the format is wrong or the UUID is not valid.
    fn parse_pa_callback(data: &str) -> Option<(uuid::Uuid, &str)> {
        let parts: Vec<&str> = data.splitn(3, ':').collect();
        if parts.len() != 3 || parts[0] != "pa" {
            return None;
        }
        let id = uuid::Uuid::parse_str(parts[1]).ok()?;
        Some((id, parts[2]))
    }

    #[test]
    fn unknown_prefix_is_none() {
        assert!(parse_pa_callback("offer_approve:some-uuid").is_none());
    }

    #[test]
    fn invalid_uuid_is_none() {
        assert!(parse_pa_callback("pa:not-a-uuid:confirm").is_none());
    }

    #[test]
    fn valid_pa_confirm_parses() {
        let id = uuid::Uuid::new_v4();
        let data = format!("pa:{id}:confirm");
        let result = parse_pa_callback(&data);
        assert!(result.is_some());
        let (parsed_id, action) = result.unwrap();
        assert_eq!(parsed_id, id);
        assert_eq!(action, "confirm");
    }

    #[test]
    fn valid_pa_cancel_parses() {
        let id = uuid::Uuid::new_v4();
        let data = format!("pa:{id}:cancel");
        let (_, action) = parse_pa_callback(&data).unwrap();
        assert_eq!(action, "cancel");
    }

    #[test]
    fn valid_pa_edit_parses() {
        let id = uuid::Uuid::new_v4();
        let data = format!("pa:{id}:edit");
        let (_, action) = parse_pa_callback(&data).unwrap();
        assert_eq!(action, "edit");
    }
}
