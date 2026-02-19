use crate::telegram::{ApprovalDecision, TelegramBot};
use crate::{EmailParser, EmailResponder, EmailResponse, ImapClient, SmtpClient};
use aust_core::config::{EmailConfig, TelegramConfig};
use aust_core::models::{MovingInquiry, ParsedEmail};
use aust_llm_providers::LlmProvider;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Pending draft waiting for Telegram approval.
#[derive(Debug, Clone)]
struct PendingDraft {
    pub draft_id: String,
    pub customer_email: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub inquiry: MovingInquiry,
}

/// The main email processing loop.
/// Orchestrates: IMAP polling → parsing → LLM draft → Telegram approval → SMTP send.
pub struct EmailProcessor {
    imap: ImapClient,
    smtp: SmtpClient,
    telegram: Arc<Mutex<TelegramBot>>,
    parser: EmailParser,
    responder: EmailResponder,
    /// Active inquiries keyed by customer email.
    inquiries: HashMap<String, MovingInquiry>,
    /// Drafts awaiting approval, keyed by draft_id.
    pending_drafts: HashMap<String, PendingDraft>,
    /// The draft currently in edit mode (Alex pressed "Bearbeiten", waiting for instructions).
    /// Only one draft can be in edit mode at a time.
    editing_draft: Option<PendingDraft>,
}

impl EmailProcessor {
    pub fn new(
        email_config: EmailConfig,
        telegram_config: TelegramConfig,
        llm: Arc<dyn LlmProvider>,
    ) -> Self {
        let imap = ImapClient::new(email_config.clone());
        let smtp = SmtpClient::new(email_config);
        let telegram = TelegramBot::new(
            telegram_config.bot_token,
            telegram_config.admin_chat_id,
        );

        Self {
            imap,
            smtp,
            telegram: Arc::new(Mutex::new(telegram)),
            parser: EmailParser::new(),
            responder: EmailResponder::new(llm),
            inquiries: HashMap::new(),
            pending_drafts: HashMap::new(),
            editing_draft: None,
        }
    }

    /// Run one cycle of the processing loop:
    /// 1. Fetch new emails
    /// 2. Process each email (parse → draft → send to Telegram)
    /// 3. Check for Telegram approval decisions
    /// 4. Send approved emails via SMTP
    pub async fn process_cycle(&mut self) {
        // Step 1: Fetch new emails
        match self.imap.fetch_unread().await {
            Ok(emails) => {
                if !emails.is_empty() {
                    info!("Processing {} new email(s)", emails.len());
                }
                for email in emails {
                    self.process_incoming_email(email).await;
                }
            }
            Err(e) => {
                error!("Failed to fetch emails: {e}");
            }
        }

        // Step 2: Check for Telegram approval decisions
        self.check_approvals().await;
    }

    /// Process a single incoming email.
    async fn process_incoming_email(&mut self, email: ParsedEmail) {
        let customer_email = email.from.clone();
        info!(
            "Processing email from {} — subject: {}",
            customer_email, email.subject
        );

        // Notify Alex about the new email
        {
            let tg = self.telegram.lock().await;
            tg.notify_new_email(
                &customer_email,
                &email.subject,
                &email.body_text[..email.body_text.len().min(300)],
            )
            .await;
        }

        // Get or create inquiry for this customer
        let inquiry = self
            .inquiries
            .entry(customer_email.clone())
            .or_insert_with(|| MovingInquiry {
                id: Uuid::now_v7(),
                email: customer_email.clone(),
                ..Default::default()
            });

        // Parse the email and extract structured data
        let updated = self.parser.parse_inquiry(&email);

        // Merge extracted data into existing inquiry
        merge_inquiry(inquiry, &updated);

        // Try to extract additional data from free-text via LLM
        if matches!(
            updated.source,
            aust_core::models::InquirySource::DirectEmail
                | aust_core::models::InquirySource::MediaEmail
        ) {
            match self
                .responder
                .extract_data_from_text(inquiry, &email.body_text)
                .await
            {
                Ok(enriched) => {
                    merge_inquiry(inquiry, &enriched);
                }
                Err(e) => {
                    warn!("LLM data extraction failed: {e}");
                }
            }
        }

        // Generate draft response
        let inquiry_snapshot = inquiry.clone();
        match self
            .responder
            .generate_response(&inquiry_snapshot, &email.body_text)
            .await
        {
            Ok(response) => {
                self.submit_draft_for_approval(
                    &customer_email,
                    response,
                    email.message_id.clone(),
                    inquiry_snapshot,
                )
                .await;
            }
            Err(e) => {
                error!("Failed to generate response for {customer_email}: {e}");
                let tg = self.telegram.lock().await;
                tg.send_status_message(&format!(
                    "Fehler bei Antwort-Generierung für {customer_email}: {e}"
                ))
                .await;
            }
        }

        // Mark as read
        if !email.message_id.is_empty() {
            if let Err(e) = self.imap.mark_as_read(&email.message_id).await {
                warn!("Failed to mark email as read: {e}");
            }
        }
    }

    /// Send a draft response to Telegram for approval.
    async fn submit_draft_for_approval(
        &mut self,
        customer_email: &str,
        response: EmailResponse,
        in_reply_to: String,
        inquiry: MovingInquiry,
    ) {
        let draft_id = Uuid::now_v7().to_string();

        let draft = PendingDraft {
            draft_id: draft_id.clone(),
            customer_email: customer_email.to_string(),
            subject: response.subject.clone(),
            body: response.body.clone(),
            in_reply_to: if in_reply_to.is_empty() {
                None
            } else {
                Some(in_reply_to)
            },
            inquiry,
        };

        let tg = self.telegram.lock().await;
        match tg
            .send_draft_for_approval(
                &draft_id,
                customer_email,
                &response.subject,
                &response.body,
            )
            .await
        {
            Ok(_msg) => {
                info!("Draft {draft_id} sent to Telegram for approval");
                drop(tg);
                self.pending_drafts.insert(draft_id, draft);
            }
            Err(e) => {
                error!("Failed to send draft to Telegram: {e}");
            }
        }
    }

    /// Re-send a revised draft to Telegram (after edit loop iteration).
    async fn resubmit_draft(&mut self, draft: PendingDraft) {
        let new_draft_id = Uuid::now_v7().to_string();

        let tg = self.telegram.lock().await;
        match tg
            .send_draft_for_approval(
                &new_draft_id,
                &draft.customer_email,
                &draft.subject,
                &draft.body,
            )
            .await
        {
            Ok(_msg) => {
                info!("Revised draft {new_draft_id} sent to Telegram");
                drop(tg);
                let new_draft = PendingDraft {
                    draft_id: new_draft_id.clone(),
                    ..draft
                };
                self.pending_drafts.insert(new_draft_id, new_draft);
            }
            Err(e) => {
                error!("Failed to send revised draft to Telegram: {e}");
            }
        }
    }

    /// Check Telegram for approval decisions and process them.
    async fn check_approvals(&mut self) {
        let responses = {
            let mut tg = self.telegram.lock().await;
            match tg.poll_approvals().await {
                Ok(r) => r,
                Err(e) => {
                    error!("Telegram poll failed: {e}");
                    return;
                }
            }
        };

        for response in responses {
            // Handle edit instructions (free text from admin while a draft is in edit mode)
            if response.draft_id == "edit_instructions" {
                if let ApprovalDecision::EditInstructions(instructions) = response.decision {
                    self.handle_edit_instructions(&instructions).await;
                }
                continue;
            }

            // Handle inline button callbacks
            let draft_id = response.draft_id.clone();
            if let Some(draft) = self.pending_drafts.remove(&draft_id) {
                match response.decision {
                    ApprovalDecision::Approve => {
                        info!("Draft {} approved, sending email", draft.draft_id);
                        self.send_approved_email(&draft).await;
                    }
                    ApprovalDecision::AwaitingEditInstructions => {
                        // Alex pressed "Bearbeiten" — park the draft and wait for instructions
                        info!("Draft {} entering edit mode", draft.draft_id);
                        self.editing_draft = Some(draft);
                    }
                    ApprovalDecision::EditInstructions(instructions) => {
                        // Shouldn't happen from a callback, but handle it anyway
                        self.editing_draft = Some(draft);
                        self.handle_edit_instructions(&instructions).await;
                    }
                    ApprovalDecision::Deny => {
                        info!("Draft {} denied, discarding", draft.draft_id);
                    }
                }
            } else {
                // Could be a decision for a draft we don't know about (e.g. from a previous session)
                if !matches!(response.decision, ApprovalDecision::EditInstructions(_)) {
                    warn!("Received decision for unknown draft: {}", draft_id);
                }
            }
        }
    }

    /// Handle edit instructions from Alex.
    /// Takes the current editing draft, revises it via LLM, and re-sends to Telegram.
    async fn handle_edit_instructions(&mut self, instructions: &str) {
        let draft = match self.editing_draft.take() {
            Some(d) => d,
            None => {
                // No draft in edit mode — Alex sent a message without pressing "Bearbeiten" first
                warn!("Received edit instructions but no draft is in edit mode");
                let tg = self.telegram.lock().await;
                tg.send_status_message(
                    "Kein Entwurf zum Bearbeiten vorhanden. \
                     Drücke zuerst ✏️ Bearbeiten bei einem Entwurf.",
                )
                .await;
                return;
            }
        };

        info!(
            "Revising draft {} with instructions: {}",
            draft.draft_id,
            &instructions[..instructions.len().min(80)]
        );

        // Show "working on it" feedback
        {
            let tg = self.telegram.lock().await;
            tg.send_status_message("⏳ Überarbeite den Entwurf...").await;
        }

        // Revise the draft via LLM
        match self
            .responder
            .revise_draft(&draft.body, instructions, &draft.subject)
            .await
        {
            Ok(revised) => {
                // Create updated draft and re-send to Telegram with buttons
                let updated_draft = PendingDraft {
                    body: revised.body,
                    subject: revised.subject,
                    ..draft
                };
                self.resubmit_draft(updated_draft).await;
            }
            Err(e) => {
                error!("Failed to revise draft: {e}");
                let tg = self.telegram.lock().await;
                tg.send_status_message(&format!(
                    "Fehler beim Überarbeiten: {e}\nDer ursprüngliche Entwurf bleibt erhalten."
                ))
                .await;
                drop(tg);
                // Put the draft back into edit mode so Alex can try again
                self.editing_draft = Some(draft);
            }
        }
    }

    /// Send an approved email via SMTP.
    async fn send_approved_email(&self, draft: &PendingDraft) {
        match self
            .smtp
            .send(
                &draft.customer_email,
                &draft.subject,
                &draft.body,
                draft.in_reply_to.as_deref(),
            )
            .await
        {
            Ok(status) => {
                info!("Email sent to {}: {status}", draft.customer_email);
                let tg = self.telegram.lock().await;
                tg.notify_sent(&draft.customer_email, &draft.subject).await;
            }
            Err(e) => {
                error!("Failed to send email to {}: {e}", draft.customer_email);
                let tg = self.telegram.lock().await;
                tg.send_status_message(&format!(
                    "FEHLER: E-Mail an {} konnte nicht gesendet werden: {e}",
                    draft.customer_email
                ))
                .await;
            }
        }
    }

    /// Run the processing loop continuously.
    pub async fn run(&mut self, poll_interval_secs: u64) {
        info!("Email processor started — polling every {poll_interval_secs}s");

        let tg = self.telegram.lock().await;
        tg.send_status_message("🟢 E-Mail-Agent gestartet. Ich überwache das Postfach.")
            .await;
        drop(tg);

        loop {
            self.process_cycle().await;
            tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
        }
    }
}

/// Merge data from a parsed inquiry into an existing one (only fill empty fields).
fn merge_inquiry(target: &mut MovingInquiry, source: &MovingInquiry) {
    if target.name.is_none() {
        target.name = source.name.clone();
    }
    if target.phone.is_none() {
        target.phone = source.phone.clone();
    }
    if target.preferred_date.is_none() {
        target.preferred_date = source.preferred_date;
    }
    if target.departure_address.is_none() {
        target.departure_address = source.departure_address.clone();
    }
    if target.departure_floor.is_none() {
        target.departure_floor = source.departure_floor.clone();
    }
    if target.departure_parking_ban.is_none() {
        target.departure_parking_ban = source.departure_parking_ban;
    }
    if target.arrival_address.is_none() {
        target.arrival_address = source.arrival_address.clone();
    }
    if target.arrival_floor.is_none() {
        target.arrival_floor = source.arrival_floor.clone();
    }
    if target.arrival_parking_ban.is_none() {
        target.arrival_parking_ban = source.arrival_parking_ban;
    }
    if target.intermediate_address.is_none() {
        target.intermediate_address = source.intermediate_address.clone();
    }
    if target.intermediate_floor.is_none() {
        target.intermediate_floor = source.intermediate_floor.clone();
    }
    if target.intermediate_parking_ban.is_none() {
        target.intermediate_parking_ban = source.intermediate_parking_ban;
    }
    if target.volume_m3.is_none() {
        target.volume_m3 = source.volume_m3;
    }
    if target.items_list.is_none() && source.items_list.is_some() {
        target.items_list = source.items_list.clone();
    }
    if !target.has_photos && source.has_photos {
        target.has_photos = true;
        target.photo_count = source.photo_count;
    }
    if !target.service_packing && source.service_packing {
        target.service_packing = true;
    }
    if !target.service_assembly && source.service_assembly {
        target.service_assembly = true;
    }
    if !target.service_disassembly && source.service_disassembly {
        target.service_disassembly = true;
    }
    if !target.service_storage && source.service_storage {
        target.service_storage = true;
    }
    if !target.service_disposal && source.service_disposal {
        target.service_disposal = true;
    }
    if source.notes.is_some() {
        let existing = target.notes.clone().unwrap_or_default();
        let new_notes = source.notes.as_deref().unwrap_or("");
        if !new_notes.is_empty() && !existing.contains(new_notes) {
            target.notes = Some(if existing.is_empty() {
                new_notes.to_string()
            } else {
                format!("{existing}\n{new_notes}")
            });
        }
    }
    if matches!(target.source, aust_core::models::InquirySource::DirectEmail) {
        target.source = source.source.clone();
    }
}
