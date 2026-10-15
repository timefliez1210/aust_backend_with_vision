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
#[allow(clippy::too_many_arguments)]
pub async fn handle_text_message(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    text: &str,
    reply_to_text: Option<&str>,
    reply_to_message_id: Option<i64>,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) -> bool {
    // Check binding — only agent-bound chats are handled here.
    if bindings::resolve(pool, chat_id).await.is_err() {
        // Unbound chat — fall through to legacy handler.
        return false;
    }

    info!(chat_id, has_quote = reply_to_text.is_some(), "Agent handling text message");

    let input = Input {
        text: text.to_string(),
        chat_id,
        images: Vec::new(),
        quoted_text: reply_to_text.map(|s| s.to_string()),
        quoted_refs: quoted_refs(pool, chat_id, reply_to_message_id).await,
        prior_action: None,
    };

    telegram_output::send_typing(client, bot_token, chat_id).await;
    match driver::process_turn(pool, llm, registry, soul, services, input).await {
        // When awaiting confirmation the keyboard message body IS the reply (M3):
        // posted once, with the tool's concrete German summary.
        Ok(result) => post_turn_result(pool, client, bot_token, chat_id, &result).await,
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

/// Handle a photo / document (PDF) message from a Telegram chat.
///
/// Downloads the file, rasterizes PDFs to images, and runs a normal assistant
/// turn with the images attached so the vision-capable model can see them.
/// Returns `true` if handled (bound chat), `false` to fall through.
#[allow(clippy::too_many_arguments)]
pub async fn handle_media_message(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    file_id: &str,
    extra_file_ids: &[String],
    kind: &str,
    mime_type: Option<&str>,
    caption: Option<&str>,
    reply_to_text: Option<&str>,
    reply_to_message_id: Option<i64>,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) -> bool {
    if bindings::resolve(pool, chat_id).await.is_err() {
        return false; // unbound chat — not ours
    }

    info!(chat_id, kind, "Agent handling media message");

    // Let Alex know we're working — vision turns take a few seconds (download +
    // rasterize + model).
    telegram_output::post_text(client, bot_token, chat_id, "🖼️ Einen Moment, ich schaue mir das an …").await;

    let mut images = match super::media::prepare_images(client, bot_token, file_id, kind, mime_type).await {
        Ok(imgs) => imgs,
        Err(msg) => {
            telegram_output::post_text(client, bot_token, chat_id, &msg).await;
            return true;
        }
    };
    // The rest of a photo album: one turn sees every photo.
    for extra in extra_file_ids {
        if images.len() >= super::media::MAX_IMAGES {
            warn!(chat_id, dropped = extra_file_ids.len(), "album exceeds MAX_IMAGES — extra photos ignored");
            break;
        }
        match super::media::prepare_images(client, bot_token, extra, "photo", None).await {
            Ok(more) => images.extend(more),
            Err(msg) => warn!(chat_id, "album photo skipped: {msg}"),
        }
    }
    images.truncate(super::media::MAX_IMAGES);

    // Caption becomes the prompt; with no caption, a neutral default instruction.
    let text = caption
        .filter(|c| !c.trim().is_empty())
        .map(|c| c.to_string())
        .unwrap_or_else(|| {
            "Der Nutzer hat dir eine Datei (Bild oder PDF) geschickt. Sieh sie dir an, \
             fasse zusammen was relevant ist und frage nach, was er damit tun möchte."
                .to_string()
        });

    let input = Input { text, chat_id, images, quoted_text: reply_to_text.map(|s| s.to_string()),
        quoted_refs: quoted_refs(pool, chat_id, reply_to_message_id).await,
        prior_action: None,
    };

    match driver::process_turn(pool, llm, registry, soul, services, input).await {
        Ok(result) => post_turn_result(pool, client, bot_token, chat_id, &result).await,
        Err(e) => {
            warn!(chat_id, "driver::process_turn (media) error: {e}");
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

/// Handle a Telegram voice note: transcribe locally, show Alex what was heard,
/// then run a normal assistant turn on the transcript.
///
/// The echo matters: a misheard name or date is visible before Josie acts on
/// it, and the transcript in the chat doubles as the record of what was asked.
#[allow(clippy::too_many_arguments)]
pub async fn handle_voice_message(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    file_id: &str,
    duration_secs: u32,
    max_duration_secs: u32,
    reply_to_text: Option<&str>,
    reply_to_message_id: Option<i64>,
    transcriber: Arc<dyn aust_assistant::voice::VoiceTranscriber>,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) -> bool {
    if bindings::resolve(pool, chat_id).await.is_err() {
        return false;
    }
    if duration_secs > max_duration_secs {
        telegram_output::post_text(
            client,
            bot_token,
            chat_id,
            &format!(
                "🎤 Die Sprachnachricht ist {} min lang — ich verarbeite höchstens {} min. Bitte kürzer oder als Text.",
                duration_secs.div_ceil(60),
                max_duration_secs / 60
            ),
        )
        .await;
        return true;
    }

    info!(chat_id, duration_secs, "Agent handling voice message");
    telegram_output::post_text(client, bot_token, chat_id, "🎤 Ich höre zu …").await;

    let audio = match super::media::download_file(client, bot_token, file_id).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(chat_id, "voice download failed: {e}");
            telegram_output::post_text(client, bot_token, chat_id, "Ich konnte die Sprachnachricht nicht von Telegram laden. Bitte versuche es erneut.").await;
            return true;
        }
    };
    let text = match transcriber.transcribe(&audio).await {
        Ok(t) => t,
        Err(aust_assistant::AssistantError::VoiceUnsupported) => {
            telegram_output::post_text(client, bot_token, chat_id, "🎤 Sprachnachrichten sind auf diesem Server noch nicht eingerichtet — bitte als Text schicken.").await;
            return true;
        }
        Err(e) => {
            warn!(chat_id, "voice transcription failed: {e}");
            telegram_output::post_text(client, bot_token, chat_id, &format!("🎤 Ich konnte die Sprachnachricht nicht verstehen ({e}). Bitte nochmal oder als Text.")).await;
            return true;
        }
    };
    telegram_output::post_text(client, bot_token, chat_id, &format!("🎤 „{text}“")).await;

    let input = Input {
        text: format!("[Sprachnachricht, automatisch transkribiert — Namen/Zahlen können verhört sein]\n{text}"),
        chat_id,
        images: Vec::new(),
        quoted_text: reply_to_text.map(|s| s.to_string()),
        quoted_refs: quoted_refs(pool, chat_id, reply_to_message_id).await,
        prior_action: None,
    };
    telegram_output::send_typing(client, bot_token, chat_id).await;
    match driver::process_turn(pool, llm, registry, soul, services, input).await {
        Ok(result) => post_turn_result(pool, client, bot_token, chat_id, &result).await,
        Err(e) => {
            warn!(chat_id, "driver::process_turn (voice) error: {e}");
            telegram_output::post_text(client, bot_token, chat_id, "⚠️ Es ist ein Fehler aufgetreten. Bitte versuche es erneut.").await;
        }
    }
    true
}

/// Handle a `pa:<uuid>:<action>` callback query (inline button tap).
///
/// Returns `true` if the callback was consumed. Returns `false` for unknown
/// callback prefixes so they fall through to the legacy handler.
#[allow(clippy::too_many_arguments)]
pub async fn handle_callback_query(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    message_id: i64,
    callback_data: &str,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) -> bool {
    if let Some(rest) = callback_data.strip_prefix("mp:") {
        handle_memory_proposal(pool, client, bot_token, chat_id, message_id, rest).await;
        return true;
    }

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
                llm, registry, soul, services, binding,
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

#[allow(clippy::too_many_arguments)]
async fn handle_confirm(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    pending_id: Uuid,
    pending: &aust_assistant::confirmation::PendingAction,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
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

    match driver::resume_confirmed(pool, llm.clone(), registry, services.clone(), params).await {
        Ok(result) => {
            let acted = result["ok"].as_bool() != Some(false);
            // A tool that could not do what was asked reports it in-band as
            // {"ok": false, "message": ...} rather than erroring — SMTP refused the
            // address, the reminder was already closed, the customer has no email.
            // That shape carries no "summary", so it used to fall through to
            // "✅ Erledigt." and Alex was told an email had gone out that never did.
            let body = if result["ok"].as_bool() == Some(false) {
                let why = result["message"]
                    .as_str()
                    .unwrap_or("Grund unbekannt");
                format!("⚠️ Nicht ausgeführt: {why}")
            } else {
                // Prefer a tool-supplied human summary; otherwise show a clean generic
                // confirmation rather than dumping the raw result JSON at Alex.
                match result["summary"].as_str() {
                    Some(s) => format!("✅ Bestätigt: {s}"),
                    None => "✅ Erledigt.".to_string(),
                }
            };

            // Update the original message to remove keyboard.
            if let Some(msg_id) = pending.telegram_message_id {
                telegram_output::edit_message_remove_keyboard(
                    client, bot_token, chat_id, msg_id, &body,
                ).await;
            } else {
                telegram_output::post_text(client, bot_token, chat_id, &body).await;
            }

            if acted {
                continue_after_confirm(
                    pool, client, bot_token, chat_id, &pending.tool_name, result,
                    llm, registry, soul, services,
                )
                .await;
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

/// The entity IDs recorded for the bot message Alex replied to, if any.
async fn quoted_refs(pool: &PgPool, chat_id: i64, reply_to_message_id: Option<i64>) -> Option<serde_json::Value> {
    aust_assistant::message_refs::lookup(pool, chat_id, reply_to_message_id?).await
}

/// ✅ Merken / ❌ Nein on a nightly lesson (`mp:<proposal_id>:ok|no`).
///
/// Only the owner may teach Josie: approval writes `agent_memory`, which shapes
/// every later prompt.
async fn handle_memory_proposal(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    message_id: i64,
    rest: &str,
) {
    use aust_assistant::memory::proposals;
    let Some((id, action)) = rest.split_once(':') else { return };
    let Ok(id) = Uuid::parse_str(id) else { return };
    let binding = match bindings::resolve(pool, chat_id).await {
        Ok(b) if b.role == aust_assistant::Role::Owner => b,
        _ => {
            telegram_output::post_text(client, bot_token, chat_id, "Nur der Inhaber kann Josie etwas beibringen.").await;
            return;
        }
    };
    let (result, done) = match action {
        "ok" => (proposals::approve(pool, id, binding.user_id).await, "✅ Gemerkt."),
        "no" => (proposals::reject(pool, id, binding.user_id, Some("telegram")).await, "❌ Verworfen."),
        _ => return,
    };
    let lesson = match proposals::get(pool, id).await {
        Ok(Some(p)) => match p.value {
            serde_json::Value::String(v) => v,
            other => other.to_string(),
        },
        _ => String::new(),
    };
    let body = match result {
        Ok(()) if lesson.is_empty() => done.to_string(),
        Ok(()) => format!("{done}\n\n{lesson}"),
        Err(aust_assistant::AssistantError::NotFound(_)) => "Dieser Vorschlag ist bereits erledigt.".to_string(),
        Err(e) => {
            warn!("memory proposal {id} ({action}) failed: {e}");
            format!("⚠️ Fehler: {e}")
        }
    };
    if message_id != 0 {
        telegram_output::edit_message_remove_keyboard(client, bot_token, chat_id, message_id, &body).await;
    } else {
        telegram_output::post_text(client, bot_token, chat_id, &body).await;
    }
}

/// Reply Josie gives on a continuation turn when nothing of the task is left.
const NOTHING_LEFT: &str = "FERTIG";

/// After a confirmed action, give Josie one turn to finish the original task.
///
/// A turn stops at the first action that needs confirmation, so "storniere den
/// Termin und trag ihn am Freitag neu ein" used to end after the cancel: the
/// tap ran it, and the re-booking was simply never attempted. The continuation
/// sees the result, runs what is still open (which may queue the next
/// confirmation), and stays silent when the confirmed action was the last step.
#[allow(clippy::too_many_arguments)]
async fn continue_after_confirm(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    tool_name: &str,
    result: serde_json::Value,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
) {
    let text = format!(
        "[System-Hinweis] Alex hat '{tool_name}' bestätigt, die Aktion ist ausgeführt. Ergebnis: {}\n\
         Sind aus seinem ursprünglichen Auftrag noch Schritte offen, führe sie jetzt aus. \
         Ist nichts mehr offen, antworte ausschließlich mit {NOTHING_LEFT}.",
        aust_assistant::driver::cap_tool_result(&result.to_string()),
    );
    let input = Input {
        text,
        chat_id,
        images: Vec::new(),
        quoted_text: None,
        quoted_refs: None,
        prior_action: Some(driver::PriorAction { tool_name: tool_name.to_string(), result }),
    };
    match driver::process_turn(pool, llm, registry, soul, services, input).await {
        Ok(r) if !r.awaiting_confirmation && is_nothing_left(&r.reply) => {}
        Ok(r) => post_turn_result(pool, client, bot_token, chat_id, &r).await,
        Err(e) => warn!(chat_id, "continuation after confirm failed: {e}"),
    }
}

/// Whether a continuation reply is just the "nothing left" marker ("FERTIG", "Fertig.").
fn is_nothing_left(reply: &str) -> bool {
    reply.trim().trim_matches(|c: char| !c.is_alphanumeric()).eq_ignore_ascii_case(NOTHING_LEFT)
}

/// Post a finished turn: the confirmation keyboard if one is pending, else the reply.
///
/// The reply is split to Telegram's size limit, and every part records the IDs
/// the turn's tools touched, so replying to it later resolves to those entities.
async fn post_turn_result(
    pool: &PgPool,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    result: &driver::TurnResult,
) {
    if result.awaiting_confirmation {
        let summary = result.pending_summary_de.as_deref().unwrap_or(result.reply.as_str());
        confirm_dispatcher::maybe_post_keyboard(pool, client, bot_token, chat_id, result.pending_action_id, summary).await;
    } else {
        let refs = aust_assistant::message_refs::from_ids(result.cited_ids.iter().cloned());
        for message_id in telegram_output::post_long_text(client, bot_token, chat_id, &result.reply).await {
            aust_assistant::message_refs::record(pool, chat_id, message_id, refs.clone()).await;
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
    // Chat-scoped, like the confirm path: an action queued in one authorised chat is
    // not another chat's to resolve. Cancel used the unscoped resolve.
    if let Err(e) =
        confirmation::resolve_from_chat(pool, pending_id, Resolution::Canceled, chat_id).await
    {
        warn!("cancel pending_action {pending_id}: {e}");
    }
    driver::record_resolution(pool, chat_id, &pending.tool_name, &pending.proposed_args, "von Alex abgebrochen — NICHT ausgeführt").await;

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
    // H1: previous behaviour resolved the action as Edited(proposed_args) and
    // asked "Was soll ich ändern?" — but the follow-up message had no link to
    // the pending action, so the edit was silently dropped. Cancel the original
    // pending action and prompt Alex to describe the change. The next user
    // message goes through the normal LLM-driven path, which has the cancelled
    // tool call in session history and can re-propose with adjusted args.
    if let Err(e) = confirmation::resolve(pool, pending_id, Resolution::Canceled).await {
        warn!("edit→cancel pending_action {pending_id}: {e}");
    }
    driver::record_resolution(
        pool, chat_id, &pending.tool_name, &pending.proposed_args,
        "Alex will Änderungen — verworfen, NICHT ausgeführt; neuen Vorschlag nach seiner Beschreibung machen",
    ).await;

    let body = "✏️ Was soll ich anders machen? Beschreib die Änderung \
                (z. B. anderer Empfänger, Betrag oder Datum) und ich \
                erstelle einen neuen Vorschlag zur Bestätigung.";
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
    fn nothing_left_marker_tolerates_case_and_punctuation() {
        assert!(super::is_nothing_left("FERTIG"));
        assert!(super::is_nothing_left(" Fertig. "));
        assert!(!super::is_nothing_left("Fertig — ich habe außerdem den Termin verschoben."));
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
