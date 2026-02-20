use crate::telegram::{ApprovalDecision, CalendarCommand, TelegramBot};
use crate::{EmailParser, EmailResponder, EmailResponse, ImapClient, SmtpClient};
use aust_calendar::{AvailabilityResult, CalendarService};
use aust_core::config::{EmailConfig, TelegramConfig};
use chrono::Datelike;
use aust_core::models::{MovingInquiry, ParsedEmail};
use aust_llm_providers::LlmProvider;
use chrono::NaiveDate;
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

/// Pending capacity override request waiting for Alex's decision.
#[derive(Debug, Clone)]
struct PendingCapacityRequest {
    pub customer_email: String,
    pub inquiry: MovingInquiry,
    pub original_body: String,
    pub in_reply_to: Option<String>,
    pub availability: AvailabilityResult,
}

/// The main email processing loop.
/// Orchestrates: IMAP polling → parsing → LLM draft → Telegram approval → SMTP send.
pub struct EmailProcessor {
    imap: ImapClient,
    smtp: SmtpClient,
    telegram: Arc<Mutex<TelegramBot>>,
    parser: EmailParser,
    responder: EmailResponder,
    calendar: Arc<CalendarService>,
    /// Active inquiries keyed by customer email.
    inquiries: HashMap<String, MovingInquiry>,
    /// Drafts awaiting approval, keyed by draft_id.
    pending_drafts: HashMap<String, PendingDraft>,
    /// The draft currently in edit mode (Alex pressed "Bearbeiten", waiting for instructions).
    /// Only one draft can be in edit mode at a time.
    editing_draft: Option<PendingDraft>,
    /// Pending capacity decisions: request_id → (inquiry snapshot, in_reply_to, email body)
    pending_capacity: HashMap<String, PendingCapacityRequest>,
}

impl EmailProcessor {
    pub fn new(
        email_config: EmailConfig,
        telegram_config: TelegramConfig,
        llm: Arc<dyn LlmProvider>,
        calendar: Arc<CalendarService>,
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
            calendar,
            inquiries: HashMap::new(),
            pending_drafts: HashMap::new(),
            editing_draft: None,
            pending_capacity: HashMap::new(),
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

        // Check calendar availability if a preferred date is set
        let availability = if let Some(date) = inquiry.preferred_date {
            match self.calendar.check_availability(date).await {
                Ok(avail) => Some(avail),
                Err(e) => {
                    warn!("Calendar availability check failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // If date is fully booked, send capacity question to Alex via Telegram
        let inquiry_snapshot = inquiry.clone();
        if let Some(ref avail) = availability {
            if !avail.requested_date_available {
                info!(
                    "Date {} is fully booked, sending capacity question to Telegram",
                    avail.requested_date
                );
                self.send_capacity_question_to_admin(
                    &customer_email,
                    &inquiry_snapshot,
                    &email.body_text,
                    email.message_id.clone(),
                    avail.clone(),
                )
                .await;
            }
        }

        // Generate draft response (with availability context)
        match self
            .responder
            .generate_response(&inquiry_snapshot, &email.body_text, availability.as_ref())
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
            // Handle calendar commands
            if response.draft_id == "calendar_command" {
                if let ApprovalDecision::CalendarCommand(cmd) = response.decision {
                    self.handle_calendar_command(cmd).await;
                }
                continue;
            }

            // Handle edit instructions (free text from admin while a draft is in edit mode)
            if response.draft_id == "edit_instructions" {
                if let ApprovalDecision::EditInstructions(instructions) = response.decision {
                    self.handle_edit_instructions(&instructions).await;
                }
                continue;
            }

            // Handle capacity approve/deny
            match &response.decision {
                ApprovalDecision::CapacityApprove(request_id) => {
                    if let Some(req) = self.pending_capacity.remove(request_id) {
                        info!("Capacity approved for {}", req.availability.requested_date);
                        // Force-create a booking for the approved date
                        let booking = aust_calendar::NewBooking {
                            booking_date: req.availability.requested_date,
                            quote_id: req.inquiry.quote_id,
                            customer_name: req.inquiry.name.clone(),
                            customer_email: Some(req.customer_email.clone()),
                            departure_address: req.inquiry.departure_address.clone(),
                            arrival_address: req.inquiry.arrival_address.clone(),
                            volume_m3: req.inquiry.volume_m3,
                            distance_km: None,
                            description: None,
                            status: "confirmed".to_string(),
                        };
                        if let Err(e) = self.calendar.force_create_booking(booking).await {
                            error!("Failed to create forced booking: {e}");
                        }
                    }
                    continue;
                }
                ApprovalDecision::CapacityDeny(request_id) => {
                    if let Some(req) = self.pending_capacity.remove(request_id) {
                        info!(
                            "Capacity denied for {}, alternatives will be suggested in email",
                            req.availability.requested_date
                        );
                    }
                    continue;
                }
                _ => {}
            }

            // Handle inline button callbacks for drafts
            let draft_id = response.draft_id.clone();
            if let Some(draft) = self.pending_drafts.remove(&draft_id) {
                match response.decision {
                    ApprovalDecision::Approve => {
                        info!("Draft {} approved, sending email", draft.draft_id);
                        self.send_approved_email(&draft).await;
                    }
                    ApprovalDecision::AwaitingEditInstructions => {
                        // If there's already a draft being edited, re-queue it
                        if let Some(old_draft) = self.editing_draft.take() {
                            info!(
                                "Re-queuing draft {} (replaced by {})",
                                old_draft.draft_id, draft.draft_id
                            );
                            let old_email = old_draft.customer_email.clone();
                            self.resubmit_draft(old_draft).await;
                            let tg = self.telegram.lock().await;
                            tg.send_status_message(&format!(
                                "⬅️ Entwurf an {old_email} wurde zurückgestellt."
                            ))
                            .await;
                        }
                        info!("Draft {} entering edit mode", draft.draft_id);
                        let tg = self.telegram.lock().await;
                        tg.send_status_message(&format!(
                            "✏️ Bearbeite Entwurf an {}.\n\
                             Was soll geändert werden?",
                            draft.customer_email
                        ))
                        .await;
                        drop(tg);
                        self.editing_draft = Some(draft);
                    }
                    ApprovalDecision::EditInstructions(instructions) => {
                        // Shouldn't happen from a callback, but handle it anyway
                        if let Some(old_draft) = self.editing_draft.take() {
                            self.resubmit_draft(old_draft).await;
                        }
                        self.editing_draft = Some(draft);
                        self.handle_edit_instructions(&instructions).await;
                    }
                    ApprovalDecision::Deny => {
                        info!("Draft {} denied, discarding", draft.draft_id);
                    }
                    _ => {}
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

    /// Send a capacity question to Alex when a date is overbooked.
    async fn send_capacity_question_to_admin(
        &mut self,
        customer_email: &str,
        inquiry: &MovingInquiry,
        original_body: &str,
        in_reply_to: String,
        availability: AvailabilityResult,
    ) {
        let request_id = Uuid::now_v7().to_string();
        let date_str = availability
            .requested_date
            .format("%d.%m.%Y")
            .to_string();

        // Build summaries of existing bookings
        let existing_summaries: Vec<String> = match self
            .calendar
            .get_bookings_for_date(availability.requested_date)
            .await
        {
            Ok(bookings) => bookings
                .iter()
                .map(|b| {
                    let name = b.customer_name.as_deref().unwrap_or("Unbekannt");
                    let from = b.departure_address.as_deref().unwrap_or("?");
                    let to = b.arrival_address.as_deref().unwrap_or("?");
                    let vol = b
                        .volume_m3
                        .map(|v| format!("{v:.1} m³"))
                        .unwrap_or_else(|| "? m³".to_string());
                    let dist = b
                        .distance_km
                        .map(|d| format!("{d:.0} km"))
                        .unwrap_or_else(|| "? km".to_string());
                    format!("{name} — {from} → {to} ({vol}, {dist})")
                })
                .collect(),
            Err(e) => {
                warn!("Failed to get existing bookings: {e}");
                vec!["(Fehler beim Laden der bestehenden Buchungen)".to_string()]
            }
        };

        // Build incoming request summary
        let name = inquiry.name.as_deref().unwrap_or("Unbekannt");
        let from = inquiry.departure_address.as_deref().unwrap_or("?");
        let to = inquiry.arrival_address.as_deref().unwrap_or("?");
        let vol = inquiry
            .volume_m3
            .map(|v| format!("{v:.1} m³"))
            .unwrap_or_else(|| "Volumen noch unbekannt".to_string());
        let dist = "Entfernung noch unbekannt".to_string();
        let incoming_summary = format!("{name} — {from} → {to} ({vol}, {dist})");

        let tg = self.telegram.lock().await;
        match tg
            .send_capacity_question(&request_id, &date_str, &existing_summaries, &incoming_summary)
            .await
        {
            Ok(_) => {
                info!("Sent capacity question {request_id} for {date_str}");
                drop(tg);
                self.pending_capacity.insert(
                    request_id,
                    PendingCapacityRequest {
                        customer_email: customer_email.to_string(),
                        inquiry: inquiry.clone(),
                        original_body: original_body.to_string(),
                        in_reply_to: if in_reply_to.is_empty() {
                            None
                        } else {
                            Some(in_reply_to)
                        },
                        availability,
                    },
                );
            }
            Err(e) => {
                error!("Failed to send capacity question: {e}");
            }
        }
    }

    /// Handle calendar commands from Telegram.
    async fn handle_calendar_command(&self, cmd: CalendarCommand) {
        match cmd {
            CalendarCommand::ShowSchedule(month_opt) => {
                let (from, to) = if let Some(ref month_str) = month_opt {
                    match NaiveDate::parse_from_str(&format!("{month_str}-01"), "%Y-%m-%d") {
                        Ok(start) => {
                            let end = if start.month() == 12 {
                                NaiveDate::from_ymd_opt(start.year() + 1, 1, 1).unwrap()
                                    - chrono::Days::new(1)
                            } else {
                                NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
                                    .unwrap()
                                    - chrono::Days::new(1)
                            };
                            (start, end)
                        }
                        Err(_) => {
                            let tg = self.telegram.lock().await;
                            tg.send_status_message(
                                "Ungültiges Format. Verwende: /kalender YYYY-MM",
                            )
                            .await;
                            return;
                        }
                    }
                } else {
                    let today = chrono::Utc::now().date_naive();
                    let start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
                    let end = if today.month() == 12 {
                        NaiveDate::from_ymd_opt(today.year() + 1, 1, 1).unwrap()
                            - chrono::Days::new(1)
                    } else {
                        NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1).unwrap()
                            - chrono::Days::new(1)
                    };
                    (start, end)
                };

                match self.calendar.get_schedule(from, to).await {
                    Ok(schedule) => {
                        let text = format_schedule_message(&schedule, from);
                        let tg = self.telegram.lock().await;
                        tg.send_schedule_message(&text).await;
                    }
                    Err(e) => {
                        error!("Failed to get schedule: {e}");
                        let tg = self.telegram.lock().await;
                        tg.send_status_message(&format!("Fehler beim Laden des Kalenders: {e}"))
                            .await;
                    }
                }
            }
            CalendarCommand::ShowUpcoming => {
                let today = chrono::Utc::now().date_naive();
                let end = today + chrono::Days::new(7);

                match self.calendar.get_schedule(today, end).await {
                    Ok(schedule) => {
                        let text = format_upcoming_message(&schedule);
                        let tg = self.telegram.lock().await;
                        tg.send_schedule_message(&text).await;
                    }
                    Err(e) => {
                        error!("Failed to get upcoming schedule: {e}");
                        let tg = self.telegram.lock().await;
                        tg.send_status_message(&format!("Fehler beim Laden der Termine: {e}"))
                            .await;
                    }
                }
            }
            CalendarCommand::SetCapacity(date_str, capacity) => {
                match NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") {
                    Ok(date) => match self.calendar.set_capacity(date, capacity).await {
                        Ok(_) => {
                            let tg = self.telegram.lock().await;
                            tg.send_status_message(&format!(
                                "✅ Kapazität für {} auf {} gesetzt.",
                                date.format("%d.%m.%Y"),
                                capacity
                            ))
                            .await;
                        }
                        Err(e) => {
                            let tg = self.telegram.lock().await;
                            tg.send_status_message(&format!("Fehler: {e}")).await;
                        }
                    },
                    Err(_) => {
                        let tg = self.telegram.lock().await;
                        tg.send_status_message(
                            "Ungültiges Datumsformat. Verwende: /kapazitaet YYYY-MM-DD Anzahl",
                        )
                        .await;
                    }
                }
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

/// Format a month schedule for Telegram.
fn format_schedule_message(
    schedule: &[aust_calendar::ScheduleEntry],
    month_start: NaiveDate,
) -> String {
    use chrono::Datelike;
    let month_name = match month_start.month() {
        1 => "Januar",
        2 => "Februar",
        3 => "März",
        4 => "April",
        5 => "Mai",
        6 => "Juni",
        7 => "Juli",
        8 => "August",
        9 => "September",
        10 => "Oktober",
        11 => "November",
        12 => "Dezember",
        _ => "",
    };

    let mut text = format!("📅 *Kalender {} {}*\n\n", month_name, month_start.year());

    for entry in schedule {
        let day_name = match entry.date.weekday() {
            chrono::Weekday::Mon => "Mo",
            chrono::Weekday::Tue => "Di",
            chrono::Weekday::Wed => "Mi",
            chrono::Weekday::Thu => "Do",
            chrono::Weekday::Fri => "Fr",
            chrono::Weekday::Sat => "Sa",
            chrono::Weekday::Sun => "So",
        };

        let active_bookings: Vec<_> = entry
            .bookings
            .iter()
            .filter(|b| b.status != "cancelled")
            .collect();

        let status_icon = if active_bookings.is_empty() {
            "⬜"
        } else if entry.availability.available {
            "🟡"
        } else {
            "🔴"
        };

        let date_str = entry.date.format("%d.%m").to_string();
        text.push_str(&format!("{status_icon} {day_name} {date_str}"));

        if !active_bookings.is_empty() {
            for b in &active_bookings {
                let name = b.customer_name.as_deref().unwrap_or("?");
                text.push_str(&format!(" — {name}"));
            }
        }

        text.push('\n');
    }

    text
}

/// Format the next 7 days for Telegram.
fn format_upcoming_message(schedule: &[aust_calendar::ScheduleEntry]) -> String {
    let mut text = "📋 *Nächste 7 Tage*\n\n".to_string();

    for entry in schedule {
        let day_name = match entry.date.weekday() {
            chrono::Weekday::Mon => "Montag",
            chrono::Weekday::Tue => "Dienstag",
            chrono::Weekday::Wed => "Mittwoch",
            chrono::Weekday::Thu => "Donnerstag",
            chrono::Weekday::Fri => "Freitag",
            chrono::Weekday::Sat => "Samstag",
            chrono::Weekday::Sun => "Sonntag",
        };

        let date_str = entry.date.format("%d.%m.%Y").to_string();
        text.push_str(&format!("*{day_name}, {date_str}*\n"));

        let active_bookings: Vec<_> = entry
            .bookings
            .iter()
            .filter(|b| b.status != "cancelled")
            .collect();

        if active_bookings.is_empty() {
            text.push_str("  Frei\n");
        } else {
            for b in &active_bookings {
                let name = b.customer_name.as_deref().unwrap_or("Unbekannt");
                let from = b.departure_address.as_deref().unwrap_or("?");
                let to = b.arrival_address.as_deref().unwrap_or("?");
                let vol = b
                    .volume_m3
                    .map(|v| format!("{v:.1} m³"))
                    .unwrap_or_else(|| "? m³".to_string());
                text.push_str(&format!("  • {name}: {from} → {to} ({vol})\n"));
            }
        }

        let remaining = entry.availability.remaining;
        if remaining > 0 {
            text.push_str(&format!(
                "  ({remaining} Platz/Plätze frei)\n"
            ));
        }
        text.push('\n');
    }

    text
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
