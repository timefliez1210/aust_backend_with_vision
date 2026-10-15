use crate::EmailError;
use aust_core::notifications::{mute_keyboard, NotificationKind};
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
    /// Admin approved a storage-rental ("Lagerung") monthly invoice — send it
    StorageApprove(String),
    /// Admin rejected a storage-rental monthly invoice
    StorageReject(String),
    /// A complete inquiry is ready to become a quote + offer
    InquiryComplete(Box<aust_core::models::MovingInquiry>),
    /// Free-text message from a chat that may be bound to the assistant agent.
    /// The orchestrator checks `telegram_chat_bindings` and routes accordingly.
    AssistantText {
        chat_id: i64,
        text: String,
        message_id: i64,
        from_user_id: Option<i64>,
        /// Text of the message Alex replied to / quoted, when this message is a
        /// Telegram reply. Carries the referent (e.g. a bot notification with an
        /// inquiry/customer ID) the assistant needs to resolve "diese Anfrage".
        reply_to_text: Option<String>,
        /// Telegram ID of the replied-to message. When it is a bot message, its
        /// recorded entity IDs (`telegram_message_refs`) name the referent exactly.
        reply_to_message_id: Option<i64>,
    },
    /// A `pa:<uuid>:<action>` callback from an assistant-bound chat.
    AssistantCallback {
        chat_id: i64,
        message_id: i64,
        callback_data: String,
        callback_query_id: String,
    },
    /// A "🔕 Stumm schalten" / "🔔 Wieder einschalten" / settings-toggle press.
    NotificationMute {
        callback: aust_core::notifications::MuteCallback,
        chat_id: i64,
        message_id: i64,
        callback_query_id: String,
    },
    /// `/benachrichtigungen` — list every notification kind with on/off buttons.
    NotificationSettings,
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
        /// Text of the message Alex replied to / quoted, when the media is sent
        /// as a Telegram reply. See `AssistantText::reply_to_text`.
        reply_to_text: Option<String>,
        /// See `AssistantText::reply_to_message_id`.
        reply_to_message_id: Option<i64>,
        /// Album this photo belongs to, if any.
        media_group_id: Option<String>,
        /// The album's other photos (`file_id`s), merged in after polling.
        extra_file_ids: Vec<String>,
    },
    /// A voice note (or audio file) from a chat that may be bound to the
    /// assistant. The orchestrator transcribes it and runs a normal text turn.
    AssistantVoice {
        chat_id: i64,
        file_id: String,
        /// Length in seconds as reported by Telegram.
        duration: u32,
        message_id: i64,
        reply_to_text: Option<String>,
        reply_to_message_id: Option<i64>,
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

    pub fn admin_chat_id(&self) -> i64 {
        self.admin_chat_id
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "https://api.telegram.org/bot{}/{}",
            self.bot_token, method
        )
    }

    /// Send a draft email to the admin for approval.
    /// Shows the draft text with Approve / Edit / Deny inline buttons.
    /// `header` is the context block above the draft ("Einordnung", who the
    /// customer is, open points for Alex). Sent as plain text: customer names and
    /// LLM output routinely contain `*` and `_`, which break Telegram Markdown.
    pub async fn send_draft_for_approval(
        &self,
        draft_id: &str,
        customer_email: &str,
        subject: &str,
        body: &str,
        header: Option<&str>,
    ) -> Result<DraftMessage, EmailError> {
        // Truncate body for Telegram (max 4096 chars)
        let display_body = if body.len() > 3000 {
            format!("{}...\n\n[Text gekürzt]", crate::text::truncate_on_char_boundary(body, 3000))
        } else {
            body.to_string()
        };
        let header = header
            .map(|h| format!("{}\n\n", crate::text::truncate_on_char_boundary(h, 700)))
            .unwrap_or_default();

        let text = format!(
            "📧 Neuer E-Mail-Entwurf\n\n\
             {header}\
             An: {customer_email}\n\
             Betreff: {subject}\n\n\
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
                        if Self::is_notification_settings_command(text) {
                            responses.push(ApprovalResponse {
                                draft_id: "notification_settings".to_string(),
                                decision: ApprovalDecision::NotificationSettings,
                            });
                        } else if let Some(cmd) = Self::parse_calendar_command(text) {
                            responses.push(ApprovalResponse {
                                draft_id: "calendar_command".to_string(),
                                decision: ApprovalDecision::CalendarCommand(cmd),
                            });
                        } else if !text.starts_with('/') {
                            debug!(
                                "Received text from admin (chat {}): {}",
                                message.chat.id,
                                crate::text::truncate_on_char_boundary(text, 80)
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
                                    text: message.assistant_text(text),
                                    message_id: message.message_id,
                                    from_user_id: message.from.as_ref().map(|u| u.id),
                                    reply_to_text: message.quoted_text(),
                            reply_to_message_id: message.reply_to_message.as_ref().map(|m| m.message_id),
                                },
                            });
                        }
                    } else {
                        // Non-admin chat — emit AssistantText for any non-command text.
                        if !text.starts_with('/') {
                            debug!(
                                "Received text from non-admin chat {}: {}",
                                message.chat.id,
                                crate::text::truncate_on_char_boundary(text, 80)
                            );
                            responses.push(ApprovalResponse {
                                draft_id: "assistant_text".to_string(),
                                decision: ApprovalDecision::AssistantText {
                                    chat_id: message.chat.id,
                                    text: message.assistant_text(text),
                                    message_id: message.message_id,
                                    from_user_id: message.from.as_ref().map(|u| u.id),
                                    reply_to_text: message.quoted_text(),
                            reply_to_message_id: message.reply_to_message.as_ref().map(|m| m.message_id),
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
                            caption: message.caption.as_deref().map(|c| message.assistant_text(c)),
                            message_id: message.message_id,
                            from_user_id: message.from.as_ref().map(|u| u.id),
                            reply_to_text: message.quoted_text(),
                            reply_to_message_id: message.reply_to_message.as_ref().map(|m| m.message_id),
                            media_group_id: message.media_group_id.clone(),
                            extra_file_ids: Vec::new(),
                        },
                    });
                }
            }
            // Voice notes / audio files → transcription + a normal assistant turn.
            if let Some(message) = &update.message
                && let Some(audio) = message.voice.as_ref().or(message.audio.as_ref())
            {
                responses.push(ApprovalResponse {
                    draft_id: "assistant_voice".to_string(),
                    decision: ApprovalDecision::AssistantVoice {
                        chat_id: message.chat.id,
                        file_id: audio.file_id.clone(),
                        duration: audio.duration,
                        message_id: message.message_id,
                        reply_to_text: message.quoted_text(),
                        reply_to_message_id: message.reply_to_message.as_ref().map(|m| m.message_id),
                    },
                });
            }
        }

        Ok(merge_albums(responses))
    }

    /// Handle a callback query (inline button press).
    async fn handle_callback(
        &self,
        data: &str,
        callback: &CallbackQuery,
    ) -> Option<ApprovalResponse> {
        // Intercept `pa:` prefix — route to assistant agent via orchestrator.
        // Do NOT answer the callback here; the orchestrator will do it after dispatch.
        // `mp:` = Josie's nightly memory proposals (✅ Merken / ❌ Nein), same route.
        if data.starts_with("pa:") || data.starts_with("mp:") {
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

        // Mute buttons — answered by the processor once the switch is stored.
        if let Some(cb) = aust_core::notifications::parse_callback(data) {
            let msg = callback.message.as_ref();
            return Some(ApprovalResponse {
                draft_id: "notification_mute".to_string(),
                decision: ApprovalDecision::NotificationMute {
                    callback: cb,
                    chat_id: msg.map(|m| m.chat.id).unwrap_or(self.admin_chat_id),
                    message_id: msg.map(|m| m.message_id).unwrap_or(0),
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
            "storage_approve" => {
                self.send_status_message("✅ Lagerungsrechnung wird versendet...").await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::StorageApprove(id),
                })
            }
            "storage_reject" => {
                self.send_status_message("❌ Lagerungsrechnung abgelehnt.").await;
                let id = draft_id.clone();
                Some(ApprovalResponse {
                    draft_id,
                    decision: ApprovalDecision::StorageReject(id),
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

    /// Notify admin that a draft was sent successfully. The caller checks the
    /// mute switch.
    pub async fn notify_sent(&self, customer_email: &str, subject: &str) {
        let text = format!("📬 E-Mail gesendet\n\nAn: {customer_email}\nBetreff: {subject}");
        self.send_with_markup(
            self.admin_chat_id,
            &text,
            Some(mute_keyboard(NotificationKind::EmailSent)),
        )
        .await;
    }

    /// Send a plain-text message (no Markdown) with an optional inline keyboard.
    /// Returns the message id, `None` on failure (logged).
    pub async fn send_with_markup(
        &self,
        chat_id: i64,
        text: &str,
        markup: Option<serde_json::Value>,
    ) -> Option<i64> {
        let mut payload = serde_json::json!({ "chat_id": chat_id, "text": text });
        if let Some(m) = markup {
            payload["reply_markup"] = m;
        }
        let resp = match self.client.post(self.api_url("sendMessage")).json(&payload).send().await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to send Telegram message: {e}");
                return None;
            }
        };
        match resp.json::<TelegramResponse<MessageResult>>().await {
            Ok(r) if r.ok => r.result.map(|m| m.message_id),
            Ok(r) => {
                error!("Telegram sendMessage error: {}", r.description.unwrap_or_default());
                None
            }
            Err(e) => {
                error!("Parse sendMessage response failed: {e}");
                None
            }
        }
    }

    /// Replace the inline keyboard of a sent message.
    pub async fn edit_reply_markup(&self, chat_id: i64, message_id: i64, markup: serde_json::Value) {
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reply_markup": markup,
        });
        if let Err(e) = self
            .client
            .post(self.api_url("editMessageReplyMarkup"))
            .json(&payload)
            .send()
            .await
        {
            error!("Failed to edit reply markup: {e}");
        }
    }

    /// Answer a callback query with a short toast.
    pub async fn answer_callback_with_text(&self, callback_id: &str, text: &str) {
        let payload = serde_json::json!({ "callback_query_id": callback_id, "text": text });
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

    /// `/benachrichtigungen` (or `/stumm`) opens the notification switches.
    fn is_notification_settings_command(text: &str) -> bool {
        matches!(
            text.trim().split('@').next().unwrap_or(""),
            "/benachrichtigungen" | "/stumm"
        )
    }

    /// Notify admin about an incoming email, with its full text (split over
    /// several messages when long). The caller checks the mute switch; the mute
    /// button sits under the last message.
    pub async fn notify_new_email(
        &self,
        from: &str,
        subject: &str,
        body: &str,
        attachments: &[String],
    ) {
        let msgs = crate::email_notification::format_email_notification(from, subject, body, attachments);
        let last = msgs.len().saturating_sub(1);
        for (i, text) in msgs.iter().enumerate() {
            let markup = (i == last).then(|| mute_keyboard(NotificationKind::EmailIncoming));
            if self.send_with_markup(self.admin_chat_id, text, markup).await.is_none() {
                // Stop rather than deliver a mail with a hole in the middle.
                break;
            }
        }
    }
}

// --- Unit tests ---

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper that mirrors the `pa:` interception logic in `handle_callback`.
    fn is_assistant_callback(data: &str) -> bool {
        data.starts_with("pa:") || data.starts_with("mp:")
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
            reply_to_text: Some("Neue Anfrage von Frederike".to_string()),
            reply_to_message_id: Some(55),
        };
        match d {
            ApprovalDecision::AssistantText { chat_id, text, message_id, from_user_id, reply_to_text, reply_to_message_id } => {
                assert_eq!(reply_to_message_id, Some(55));
                assert_eq!(chat_id, 42);
                assert_eq!(text, "Hallo");
                assert_eq!(message_id, 99);
                assert_eq!(from_user_id, Some(7));
                assert_eq!(reply_to_text.as_deref(), Some("Neue Anfrage von Frederike"));
            }
            _ => panic!("wrong variant"),
        }
    }

    fn media(chat_id: i64, file_id: &str, group: Option<&str>, caption: Option<&str>) -> ApprovalResponse {
        ApprovalResponse {
            draft_id: "assistant_media".to_string(),
            decision: ApprovalDecision::AssistantMedia {
                chat_id,
                file_id: file_id.to_string(),
                kind: "photo".to_string(),
                mime_type: None,
                caption: caption.map(str::to_string),
                message_id: 1,
                from_user_id: None,
                reply_to_text: None,
                reply_to_message_id: None,
                media_group_id: group.map(str::to_string),
                extra_file_ids: Vec::new(),
            },
        }
    }

    #[test]
    fn album_photos_merge_into_one_turn() {
        let merged = merge_albums(vec![
            media(1, "a", Some("g1"), None),
            media(1, "b", Some("g1"), Some("Inventar Keller")),
            media(1, "c", Some("g1"), None),
            media(1, "solo", None, None),
            media(2, "d", Some("g1"), None),
        ]);
        assert_eq!(merged.len(), 3, "one album turn, the single photo, the other chat's photo");
        match &merged[0].decision {
            ApprovalDecision::AssistantMedia { file_id, extra_file_ids, caption, .. } => {
                assert_eq!(file_id, "a");
                assert_eq!(extra_file_ids, &vec!["b".to_string(), "c".to_string()]);
                assert_eq!(caption.as_deref(), Some("Inventar Keller"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn voice_note_parses_from_the_bot_api_shape() {
        let msg: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 3,
            "chat": { "id": 1 },
            "voice": { "file_id": "AwAC", "duration": 12, "mime_type": "audio/ogg" }
        }))
        .unwrap();
        let v = msg.voice.as_ref().unwrap();
        assert_eq!((v.file_id.as_str(), v.duration), ("AwAC", 12));
    }

    #[test]
    fn forwarded_text_is_labelled_with_its_origin() {
        let msg: TgMessage = serde_json::from_value(serde_json::json!({
            "message_id": 1,
            "chat": { "id": 1 },
            "text": "Können Sie am Freitag kommen?",
            "forward_origin": { "type": "user", "sender_user": { "first_name": "Petra", "last_name": "Lange" } }
        }))
        .unwrap();
        assert_eq!(
            msg.assistant_text("Können Sie am Freitag kommen?"),
            "[Weitergeleitete Nachricht von Petra Lange]\nKönnen Sie am Freitag kommen?"
        );
    }

    #[test]
    fn quoted_text_prefers_partial_quote_then_reply_body() {
        // Partial quote (Bot API 7.0+) wins when present.
        let with_quote = TgMessage {
            message_id: 1,
            chat: TgChat { id: 1 },
            text: Some("setze eine Erinnerung".to_string()),
            from: None,
            photo: None,
            document: None,
            caption: None,
            reply_to_message: Some(Box::new(TgMessage {
                message_id: 0,
                chat: TgChat { id: 1 },
                text: Some("voller Benachrichtigungstext".to_string()),
                from: None,
                photo: None,
                document: None,
                caption: None,
                reply_to_message: None,
                quote: None,
                media_group_id: None,
                forward_origin: None,
                voice: None,
                audio: None,
            })),
            quote: Some(TgQuote { text: Some("Anfrage 1234".to_string()) }),
            media_group_id: None,
            forward_origin: None,
            voice: None,
            audio: None,
        };
        let quoted = with_quote.quoted_text().unwrap();
        assert!(quoted.contains("«Anfrage 1234»"), "{quoted}");
        assert!(quoted.contains("voller Benachrichtigungstext"), "the full message must survive a partial quote");

        // Falls back to the replied-to message body when there is no partial quote.
        let reply_only = TgMessage {
            message_id: 1,
            chat: TgChat { id: 1 },
            text: Some("setze eine Erinnerung".to_string()),
            from: None,
            photo: None,
            document: None,
            caption: None,
            reply_to_message: Some(Box::new(TgMessage {
                message_id: 0,
                chat: TgChat { id: 1 },
                text: Some("Neue Anfrage von Frederike".to_string()),
                from: None,
                photo: None,
                document: None,
                caption: None,
                reply_to_message: None,
                quote: None,
                media_group_id: None,
                forward_origin: None,
                voice: None,
                audio: None,
            })),
            quote: None,
            media_group_id: None,
            forward_origin: None,
            voice: None,
            audio: None,
        };
        assert_eq!(reply_only.quoted_text().as_deref(), Some("Neue Anfrage von Frederike"));

        // No reply at all → None.
        let plain = TgMessage {
            message_id: 1,
            chat: TgChat { id: 1 },
            text: Some("hallo".to_string()),
            from: None,
            photo: None,
            document: None,
            caption: None,
            reply_to_message: None,
            quote: None,
            media_group_id: None,
            forward_origin: None,
            voice: None,
            audio: None,
        };
        assert_eq!(plain.quoted_text(), None);
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

/// Fold the photos of one album into a single `AssistantMedia` decision.
///
/// Telegram sends an album as one update per photo, all sharing a
/// `media_group_id` and normally arriving in the same poll. Handled one by one,
/// each photo started its own assistant turn — five "Einen Moment …" messages
/// and five answers for one inventory. The first photo keeps its caption and
/// collects the rest; a caption on a later photo is used if the first had none.
fn merge_albums(responses: Vec<ApprovalResponse>) -> Vec<ApprovalResponse> {
    let mut out: Vec<ApprovalResponse> = Vec::with_capacity(responses.len());
    for resp in responses {
        if let ApprovalDecision::AssistantMedia { media_group_id: Some(group), chat_id, file_id, caption, .. } =
            &resp.decision
        {
            let target = out.iter_mut().find(|r| {
                matches!(&r.decision, ApprovalDecision::AssistantMedia {
                    media_group_id: Some(g), chat_id: c, ..
                } if g == group && c == chat_id)
            });
            if let Some(first) = target {
                if let ApprovalDecision::AssistantMedia { extra_file_ids, caption: first_caption, .. } =
                    &mut first.decision
                {
                    extra_file_ids.push(file_id.clone());
                    if first_caption.is_none() {
                        *first_caption = caption.clone();
                    }
                }
                continue;
            }
        }
        out.push(resp);
    }
    out
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
    /// The message this one replies to. When Alex replies to (quotes) a bot
    /// notification and asks the assistant to act ("Erinnerung für diese Anfrage
    /// setzen"), the referent — including any inquiry/customer IDs — lives here.
    /// Boxed because `TgMessage` would otherwise be infinitely sized.
    #[serde(default)]
    reply_to_message: Option<Box<TgMessage>>,
    /// Bot API 7.0+ partial quote: the specific span of the replied-to message
    /// that Telegram highlights. Preferred over the full `reply_to_message` text
    /// when present, since it is exactly what Alex pointed at.
    #[serde(default)]
    quote: Option<TgQuote>,
    /// Set on every message of a photo album; Telegram delivers each photo as
    /// its own update, and they are merged into one assistant turn.
    #[serde(default)]
    media_group_id: Option<String>,
    /// Where a forwarded message came from (Bot API 7.0+ `MessageOrigin`).
    #[serde(default)]
    forward_origin: Option<serde_json::Value>,
    /// A recorded voice note (OGG/Opus).
    #[serde(default)]
    voice: Option<TgAudio>,
    /// An audio file sent as music/audio rather than recorded in-app.
    #[serde(default)]
    audio: Option<TgAudio>,
}

#[derive(Debug, Deserialize)]
struct TgAudio {
    file_id: String,
    #[serde(default)]
    duration: u32,
}

impl TgMessage {
    /// Who a forwarded message originally came from, as a display name.
    fn forwarded_from(&self) -> Option<String> {
        let o = self.forward_origin.as_ref()?;
        let name = match o["type"].as_str() {
            Some("user") => {
                let u = &o["sender_user"];
                [u["first_name"].as_str(), u["last_name"].as_str()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            Some("hidden_user") => o["sender_user_name"].as_str().unwrap_or_default().to_string(),
            Some("chat") => o["sender_chat"]["title"].as_str().unwrap_or_default().to_string(),
            Some("channel") => o["chat"]["title"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        Some(if name.trim().is_empty() { "unbekannt".to_string() } else { name })
    }

    /// The text the assistant sees: a forwarded message is labelled with its
    /// origin, so Josie treats it as someone else's words, not Alex's request.
    fn assistant_text(&self, text: &str) -> String {
        match self.forwarded_from() {
            Some(from) => format!("[Weitergeleitete Nachricht von {from}]\n{text}"),
            None => text.to_string(),
        }
    }

    /// The text Alex quoted/replied to, if any. Prefers the highlighted partial
    /// quote (Bot API 7.0+), then falls back to the full replied-to message's
    /// text or caption.
    fn quoted_text(&self) -> Option<String> {
        let full = self
            .reply_to_message
            .as_ref()
            .and_then(|r| r.text.as_ref().or(r.caption.as_ref()))
            .filter(|t| !t.trim().is_empty());
        let partial = self
            .quote
            .as_ref()
            .and_then(|q| q.text.as_ref())
            .filter(|t| !t.trim().is_empty());
        match (partial, full) {
            // The highlight says what Alex pointed at; the full message keeps the
            // rest (name, date, ID) the highlight alone used to drop.
            (Some(p), Some(f)) if p.trim() != f.trim() => {
                Some(format!("Markiert: «{}»\nGanze Nachricht:\n{f}", p.trim()))
            }
            (Some(p), _) => Some(p.clone()),
            (None, Some(f)) => Some(f.clone()),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TgQuote {
    #[serde(default)]
    text: Option<String>,
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
