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
            client: Client::new(),
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
            "timeout": 1,
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
            if let Some(callback) = &update.callback_query {
                if let Some(data) = &callback.data {
                    if let Some(resp) = self.handle_callback(data, callback).await {
                        responses.push(resp);
                    }
                }
            }

            // Handle text message (edit instructions from admin)
            if let Some(message) = &update.message {
                if message.chat.id == self.admin_chat_id {
                    if let Some(text) = &message.text {
                        // Skip bot commands
                        if !text.starts_with('/') {
                            debug!(
                                "Received text from admin: {}",
                                &text[..text.len().min(80)]
                            );
                            // This is edit instructions — the processor will match it
                            // to whichever draft is currently awaiting edit
                            responses.push(ApprovalResponse {
                                draft_id: "edit_instructions".to_string(),
                                decision: ApprovalDecision::EditInstructions(text.clone()),
                            });
                        }
                    }
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
                self.send_status_message(
                    "✏️ Was soll geändert werden? Schreib einfach deine Anweisungen \
                     (z.B. \"Mach es kürzer\" oder \"Frag auch nach dem Aufzug\").",
                )
                .await;
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
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    #[allow(dead_code)]
    message_id: i64,
    chat: TgChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}
