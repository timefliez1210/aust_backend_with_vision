use crate::EmailError;
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, error, info, warn};

/// Telegram bot client for human-in-the-loop email draft approval.
///
/// Flow: Agent drafts email → Telegram sends draft to Alex → Alex approves/edits/denies
/// → approved text gets sent via SMTP.
///
/// Edit flow is a chat loop:
///   1. Alex presses ✏️ Bearbeiten
///   2. Bot asks "Was soll geändert werden?"
///   3. Alex types instructions ("Mach es kürzer", "Frag auch nach dem Aufzug")
///   4. Agent revises draft via LLM
///   5. New draft sent with same 3 buttons → repeat until Approve/Deny
pub struct TelegramBot {
    client: Client,
    bot_token: String,
    admin_chat_id: i64,
    /// Last processed update_id (to avoid reprocessing)
    last_update_id: Option<i64>,
}

/// The result of sending a draft for approval.
#[derive(Debug)]
pub struct DraftMessage {
    /// Telegram message_id of the sent draft
    pub message_id: i64,
}

/// What the admin decided about a draft.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Send the draft as-is
    Approve,
    /// Admin pressed "Bearbeiten" — waiting for instructions (no text yet)
    AwaitingEditInstructions,
    /// Admin sent edit instructions (the instruction text, NOT replacement text)
    EditInstructions(String),
    /// Discard the draft entirely
    Deny,
    /// Admin approved overbooking via capacity question
    CapacityApprove(String),
    /// Admin denied overbooking via capacity question
    CapacityDeny(String),
    /// A calendar management command from the admin
    CalendarCommand(CalendarCommand),
    /// Admin approved sending an offer to the customer
    OfferApprove(String),
    /// Admin wants to edit an offer before sending
    OfferEdit(String),
    /// Admin rejected/discarded an offer
    OfferDeny(String),
    /// Free-text edit instructions for an offer (routed when no email draft is being edited)
    OfferEditText(String),
    /// A complete inquiry is ready to become a quote + offer
    InquiryComplete(Box<aust_core::models::MovingInquiry>),
    /// Free-text message from a chat that may be bound to the assistant agent.
    /// The orchestrator checks `telegram_chat_bindings` and routes accordingly.
    AssistantText {
        chat_id: i64,
        text: String,
        message_id: i64,
        from_user_id: Option<i64>,
    },
    /// A `pa:<uuid>:<action>` callback from an assistant-bound chat.
    AssistantCallback {
        chat_id: i64,
        message_id: i64,
        callback_data: String,
        callback_query_id: String,
    },
    /// A photo or document (PDF) message from a chat that may be bound to the
    /// assistant agent. The orchestrator downloads the file via `getFile`,
    /// rasterizes PDFs, and forwards the images to the assistant.
    AssistantMedia {
        chat_id: i64,
        /// Telegram `file_id` of the largest photo size, or of the document.
        file_id: String,
        /// `"photo"` or `"document"`.
        kind: String,
        /// Document MIME type when known (e.g. `application/pdf`); `None` for photos.
        mime_type: Option<String>,
        /// Optional caption Alex typed alongside the media.
        caption: Option<String>,
        message_id: i64,
        from_user_id: Option<i64>,
    },
}

/// Calendar commands from Telegram.
#[derive(Debug, Clone)]
pub enum CalendarCommand {
    /// Show schedule for a month (YYYY-MM) or current month
    ShowSchedule(Option<String>),
    /// Show next 7 days
    ShowUpcoming,
    /// Set capacity for a date: (date_str, capacity)
    SetCapacity(String, i32),
}

/// A callback response from Telegram (inline keyboard press or text reply).
#[derive(Debug)]
pub struct ApprovalResponse {
    pub draft_id: String,
    pub decision: ApprovalDecision,
}

impl TelegramBot {
    pub fn new(bot_token: String, admin_chat_id: i64) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            bot_token,
            admin_chat_id,
            last_update_id: None,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.bot_token, method
        )
    }

    /// Send a draft email to the admin for approval.
    /// Shows the draft text with Approve / Edit / Deny inline buttons.
    pub async fn send_draft_for_approval(
        &self,
        draft_id: &str,
        customer_email: &str,
        subject: &str,
        body: &str,
    ) -> Result<DraftMessage, EmailError> {
        // Truncate body for Telegram (max 4096 chars)
        let display_body = if body.len() > 3000 {
            format!("{}...\n\n[Text gekürzt]", &body[..3000])
        } else {
            body.to_string()
        };

        let text = format!(
            "📧 *Neuer E-Mail-Entwurf*\n\n\
             *An:* `{customer_email}`\n\
             *Betreff:* {subject}\n\n\
             ─────────────────\n\
             {display_body}\n\
             ─────────────────\n\n\
             Was möchtest du tun?"
        );

        let inline_keyboard = serde_json::json!({
            "inline_keyboard": [[
                {
                    "text": "✅ Senden",
                    "callback_data": format!("approve:{draft_id}")
                },
                {
                    "text": "✏️ Bearbeiten",
                    "callback_data": format!("edit:{draft_id}")
                },
                {
                    "text": "❌ Verwerfen",
                    "callback_data": format!("deny:{draft_id}")
                }
            ]]
        });

        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
            "parse_mode": "Markdown",
            "reply_markup": inline_keyboard,
        });

        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| EmailError::Telegram(format!("Failed to send draft: {e}")))?;

        let result: TelegramResponse<MessageResult> = response
            .json()
            .await
            .map_err(|e| EmailError::Telegram(format!("Failed to parse response: {e}")))?;

        if !result.ok {
            return Err(EmailError::Telegram(format!(
                "Telegram API error: {}",
                result.description.unwrap_or_default()
            )));
        }

        let message_id = result.result.map(|r| r.message_id).unwrap_or(0);

        info!("Sent draft {draft_id} to Telegram (message_id: {message_id})");
        Ok(DraftMessage { message_id })
    }

    /// Poll for approval decisions from the admin.
    /// Returns any new approval responses since the last poll.
    pub async fn poll_approvals(&mut self) -> Result<Vec<ApprovalResponse>, EmailError> {
        let mut params = serde_json::json!({
            "timeout": 2,
            "allowed_updates": ["callback_query", "message"],
        });

        if let Some(offset) = self.last_update_id {
            params["offset"] = serde_json::json!(offset + 1);
        }

        let response = self
            .client
            .post(self.api_url("getUpdates"))
            .json(&params)
            .send()
            .await
            .map_err(|e| EmailError::Telegram(format!("Poll failed: {e}")))?;

        let result: TelegramResponse<Vec<Update>> = response
            .json()
            .await
            .map_err(|e| EmailError::Telegram(format!("Parse poll response failed: {e}")))?;

        if !result.ok {
            return Err(EmailError::Telegram(format!(
                "Poll error: {}",
                result.description.unwrap_or_default()
            )));
        }

        let updates = result.result.unwrap_or_default();
        let mut responses = Vec::new();

        for update in &updates {
            self.last_update_id = Some(update.update_id);

            // Handle inline keyboard callback (Approve/Edit/Deny button press)
            if let Some(callback) = &update.callback_query
                && let Some(data) = &callback.data
                    && let Some(resp) = self.handle_callback(data, callback).await {
                        responses.push(resp);
                    }

            // Handle text message (edit instructions, calendar commands, or assistant input)
            if let Some(message) = &update.message
                && let Some(text) = &message.text {
                    let is_admin = message.chat.id == self.admin_chat_id;
                    if is_admin {
                        if let Some(cmd) = Self::parse_calendar_command(text) {
                            responses.push(ApprovalResponse {
                                draft_id: "calendar_command".to_string(),
                                decision: ApprovalDecision::CalendarCommand(cmd),
                            });
                        } else if !text.starts_with('/') {
                            debug!(
                                "Received text from admin (chat {}): {}",
                                message.chat.id,
                                &text[..text.len().min(80)]
                            );
                            // Emit EditInstructions for the legacy offer-edit flow AND
                            // AssistantText so the orchestrator can route to the agent if
                            // this chat is in telegram_chat_bindings.
                            responses.push(ApprovalResponse {
                                draft_id: "edit_instructions".to_string(),
                                decision: ApprovalDecision::EditInstructions(text.clone()),
                            });
                            responses.push(ApprovalResponse {
                                draft_id: "assistant_text".to_string(),
                                decision: ApprovalDecision::AssistantText {
                                    chat_id: message.chat.id,
                                    text: text.clone(),
                                    message_id: message.message_id,
                                    from_user_id: message.from.as_ref().map(|u| u.id),
                                },
                            });
                        }
                    } else {
                        // Non-admin chat — emit AssistantText for any non-command text.
                        if !text.starts_with('/') {
                            debug!(
                                "Received text from non-admin chat {}: {}",
                                message.chat.id,
                                &text[..text.len().min(80)]
                            );
                            responses.push(ApprovalResponse {
                                draft_id: "assistant_text".to_string(),
                                decision: ApprovalDecision::AssistantText {
                                    chat_id: message.chat.id,
                                    text: text.clone(),
                                    message_id: message.message_id,
                                    from_user_id: message.from.as_ref().map(|u| u.id),
                                },
                            });
                        }
                    }
                }

            // Handle photo / document (PDF) messages — emit AssistantMedia so the
            // orchestrator can download, rasterize PDFs and route to the agent if
            // the chat is bound. Telegram puts text in `text` and media in
            // `photo`/`document`, so this is mutually exclusive with the block above.
            if let Some(message) = &update.message {
                let media = message
                    .photo
                    .as_ref()
                    .and_then(|sizes| sizes.last())
                    .map(|largest| (largest.file_id.clone(), "photo".to_string(), None))
                    .or_else(|| {
                        message.document.as_ref().map(|d| {
                            (d.file_id.clone(), "document".to_string(), d.mime_type.clone())
                        })
                    });
                if let Some((file_id, kind, mime_type)) = media {
                    debug!(
                        "Received {kind} from chat {} (file_id {})",
                        message.chat.id, file_id
                    );
                    responses.push(ApprovalResponse {
                        draft_id: "assistant_media".to_string(),
                        decision: ApprovalDecision::AssistantMedia {
                            chat_id: message.chat.id,
                            file_id,
                            kind,
                            mime_type,
                            caption: message.caption.clone(),
                            message_id: message.message_id,
                            from_user_id: message.from.as_ref().map(|u| u.id),
                        },
                    });
                }
            }
        }

        Ok(responses)
    }

    /// Handle a callback query (inline button press).
    async fn handle_callback(
        &self,
        data: &str,
        callback: &CallbackQuery,
    ) -> Option<ApprovalResponse> {
        // Intercept `pa:` prefix — route to assistant agent via orchestrator.
        // Do NOT answer the callback here; the orchestrator will do it after dispatch.
        if data.starts_with("pa:") {
            let chat_id = callback
                .message
                .as_ref()
                .map(|m| m.chat.id)
                .unwrap_or(self.admin_chat_id);
            let message_id = callback
                .message
                .as_ref()
                .map(|m| m.message_id)
                .unwrap_or(0);
            return Some(ApprovalResponse {
                draft_id: "assistant_callback".to_string(),
                decision: ApprovalDecision::AssistantCallback {
                    chat_id,
                    message_id,
                    callback_data: data.to_string(),
                    callback_query_id: callback.id.clone(),
                },
            });
        }

        let parts: Vec<&str> = data.splitn(2, ':').collect();
        if parts.len() != 2 {
            warn!("Invalid callback data: {data}");
            return None;
        }

        let action = parts[0];
        let draft_id = parts[1].to_string();

        // Answer the callback to remove the "loading" state on the button
        self.answer_callback(&callback.id).await;

        match action {
            "approve" => {
                self.send_status_message("✅ Entwurf wird gesendet...").await;
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::Approve,
                })
            }
            "edit" => {
                // Context-aware edit prompt is sent by the processor,
                // which knows the customer email and can handle re-queuing.
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::AwaitingEditInstructions,
                })
            }
            "deny" => {
                self.send_status_message("❌ Entwurf verworfen.").await;
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::Deny,
                })
            }
            "cap_yes" => {
                self.send_status_message("✅ Zusätzlicher Umzug wird eingeplant.")
                    .await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::CapacityApprove(id),
                })
            }
            "cap_no" => {
                self.send_status_message("❌ Anfrage wird mit Alternativterminen beantwortet.")
                    .await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::CapacityDeny(id),
                })
            }
            "offer_approve" => {
                self.send_status_message("✅ Angebot wird versendet...").await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::OfferApprove(id),
                })
            }
            "offer_edit" => {
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::OfferEdit(id),
                })
            }
            "offer_deny" => {
                self.send_status_message("❌ Angebot verworfen.").await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::OfferDeny(id),
                })
            }
            _ => {
                warn!("Unknown callback action: {action}");
                None
            }
        }
    }

    /// Answer a callback query to dismiss the loading indicator.
    async fn answer_callback(&self, callback_id: &str) {
        let payload = serde_json::json!({
            "callback_query_id": callback_id,
        });

        if let Err(e) = self
            .client
            .post(self.api_url("answerCallbackQuery"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to answer callback: {e}");
        }
    }

    /// Send a simple status message to the admin.
    pub async fn send_status_message(&self, text: &str) {
        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
        });

        if let Err(e) = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to send status message: {e}");
        }
    }

    /// Notify admin that a draft was sent successfully.
    pub async fn notify_sent(&self, customer_email: &str, subject: &str) {
        let text = format!(
            "📬 E-Mail gesendet!\n\n*An:* `{customer_email}`\n*Betreff:* {subject}",
        );
        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        if let Err(e) = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to send notification: {e}");
        }
    }

    /// Send a capacity question to the admin when a date is overbooked.
    /// Shows existing bookings and the incoming request with full details.
    pub async fn send_capacity_question(
        &self,
        request_id: &str,
        date: &str,
        existing_bookings: &[String],
        incoming_summary: &str,
    ) -> Result<DraftMessage, EmailError> {
        let mut existing_text = String::new();
        for line in existing_bookings {
            existing_text.push_str(&format!("• {line}\n"));
        }

        let text = format!(
            "⚠️ *Kapazitätsanfrage für {date}*\n\n\
             Bereits bestätigt:\n{existing_text}\n\
             Neue Anfrage:\n\
             • {incoming_summary}\n\n\
             Hast du Kapazität für einen weiteren Umzug?"
        );

        let inline_keyboard = serde_json::json!({
            "inline_keyboard": [[
                {
                    "text": "✅ Ja",
                    "callback_data": format!("cap_yes:{request_id}")
                },
                {
                    "text": "❌ Nein",
                    "callback_data": format!("cap_no:{request_id}")
                }
            ]]
        });

        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
            "parse_mode": "Markdown",
            "reply_markup": inline_keyboard,
        });

        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| EmailError::Telegram(format!("Failed to send capacity question: {e}")))?;

        let result: TelegramResponse<MessageResult> = response
            .json()
            .await
            .map_err(|e| EmailError::Telegram(format!("Failed to parse response: {e}")))?;

        if !result.ok {
            return Err(EmailError::Telegram(format!(
                "Telegram API error: {}",
                result.description.unwrap_or_default()
            )));
        }

        let message_id = result.result.map(|r| r.message_id).unwrap_or(0);
        info!("Sent capacity question for {date} (message_id: {message_id})");
        Ok(DraftMessage { message_id })
    }

    /// Send a schedule summary to the admin.
    pub async fn send_schedule_message(&self, text: &str) {
        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        if let Err(e) = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to send schedule message: {e}");
        }
    }

    /// Parse a Telegram message as a calendar command.
    /// Returns None if the message is not a recognized command.
    fn parse_calendar_command(text: &str) -> Option<CalendarCommand> {
        let text = text.trim();

        if text == "/kalender" {
            return Some(CalendarCommand::ShowSchedule(None));
        }
        if let Some(month) = text.strip_prefix("/kalender ") {
            let month = month.trim();
            if !month.is_empty() {
                return Some(CalendarCommand::ShowSchedule(Some(month.to_string())));
            }
        }
        if text == "/termine" {
            return Some(CalendarCommand::ShowUpcoming);
        }
        if let Some(args) = text.strip_prefix("/kapazitaet ") {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.len() == 2
                && let Ok(cap) = parts[1].parse::<i32>() {
                    return Some(CalendarCommand::SetCapacity(parts[0].to_string(), cap));
                }
        }

        None
    }

    /// Notify admin about an incoming email.
    pub async fn notify_new_email(&self, from: &str, subject: &str, preview: &str) {
        let preview_short = if preview.len() > 200 {
            format!("{}...", &preview[..200])
        } else {
            preview.to_string()
        };

        let text = format!(
            "📩 *Neue E-Mail eingegangen*\n\n\
             *Von:* `{from}`\n\
             *Betreff:* {subject}\n\n\
             {preview_short}"
        );

        let payload = serde_json::json!({
            "chat_id": self.admin_chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        if let Err(e) = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to send new email notification: {e}");
        }
    }
}

// --- Unit tests ---

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper that mirrors the `pa:` interception logic in `handle_callback`.
    fn is_assistant_callback(data: &str) -> bool {
        data.starts_with("pa:")
    }

    #[test]
    fn pa_prefix_is_intercepted_as_assistant_callback() {
        let id = uuid::Uuid::new_v4();
        let data = format!("pa:{id}:confirm");
        assert!(is_assistant_callback(&data));
    }

    #[test]
    fn legacy_offer_approve_is_not_intercepted() {
        assert!(!is_assistant_callback("offer_approve:some-uuid"));
    }

    #[test]
    fn legacy_approve_is_not_intercepted() {
        assert!(!is_assistant_callback("approve:draft-id"));
    }

    #[test]
    fn legacy_cap_yes_is_not_intercepted() {
        assert!(!is_assistant_callback("cap_yes:some-id"));
    }

    #[test]
    fn assistant_text_variant_has_correct_fields() {
        let d = ApprovalDecision::AssistantText {
            chat_id: 42,
            text: "Hallo".to_string(),
            message_id: 99,
            from_user_id: Some(7),
        };
        match d {
            ApprovalDecision::AssistantText { chat_id, text, message_id, from_user_id } => {
                assert_eq!(chat_id, 42);
                assert_eq!(text, "Hallo");
                assert_eq!(message_id, 99);
                assert_eq!(from_user_id, Some(7));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn assistant_callback_variant_has_correct_fields() {
        let id = uuid::Uuid::new_v4();
        let data = format!("pa:{id}:confirm");
        let d = ApprovalDecision::AssistantCallback {
            chat_id: 42,
            message_id: 10,
            callback_data: data.clone(),
            callback_query_id: "cq-id".to_string(),
        };
        match d {
            ApprovalDecision::AssistantCallback {
                chat_id,
                callback_data,
                callback_query_id,
                ..
            } => {
                assert_eq!(chat_id, 42);
                assert_eq!(callback_data, data);
                assert_eq!(callback_query_id, "cq-id");
            }
            _ => panic!("wrong variant"),
        }
    }
}

// --- Telegram API types ---

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    description: Option<String>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct MessageResult {
    message_id: i64,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    callback_query: Option<CallbackQuery>,
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    id: String,
    data: Option<String>,
    /// The message the button was attached to (for extracting message_id).
    message: Option<CallbackMessage>,
}

#[derive(Debug, Deserialize)]
struct CallbackMessage {
    message_id: i64,
    chat: TgChat,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    chat: TgChat,
    text: Option<String>,
    from: Option<TgUser>,
    /// Present when the message is a photo — Telegram sends multiple sizes,
    /// ascending; the last entry is the largest.
    #[serde(default)]
    photo: Option<Vec<TgPhotoSize>>,
    /// Present when the message is a document (e.g. a PDF).
    #[serde(default)]
    document: Option<TgDocument>,
    /// Caption text typed alongside a photo/document.
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgPhotoSize {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TgDocument {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}
