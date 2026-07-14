//! Dunning (Zahlungserinnerung / Mahnung) and review-request logic.
//!
//! Both the admin dashboard routes and the assistant's service bridge drive this
//! flow, so the rules live here rather than inside a route handler: what a dunning
//! level is called, what its email says, when the next one comes due, and what
//! happens to an inquiry when its last invoice is paid.
//!
//! Two ladders, deliberately separate:
//!
//! * **Invoices** — `invoice_reminders` holds one row per sent invoice. It climbs
//!   level 1 → 3 (Zahlungserinnerung → 1. Mahnung → 2. Mahnung), 7 days apart, and
//!   closes when the invoice is paid or the last level has been sent.
//! * **Reviews** — `review_requests` holds one row per inquiry: send the Google
//!   review mail now, defer it by N days, or skip it for good.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repositories::{
    customer_repo, invoice_reminder_repo, invoice_repo, review_repo, storage_repo,
};
use crate::routes::admin_emails;
use crate::ApiError;

/// Days between one dunning level and the next.
const DUNNING_INTERVAL_DAYS: u64 = 7;

/// Default deferral when Alex picks "später" on a review request.
pub(crate) const DEFAULT_REVIEW_SNOOZE_DAYS: u32 = 3;

/// Human label per dunning level. Index = `level - 1`.
pub(crate) const DUNNING_LEVEL_LABEL: [&str; 3] =
    ["Zahlungserinnerung", "1. Mahnung", "2. Mahnung"];

/// Google-review link sent to customers in the review request email.
/// Direct write-a-review URL for Aust Umzüge & Haushaltsauflösungen.
const GOOGLE_REVIEW_URL: &str =
    "https://www.google.com/search?q=Aust+Umz%C3%BCge+%26+Haushaltsaufl%C3%B6sungen+Reviews";

/// Label for a dunning level, clamped to the three we have copy for.
pub(crate) fn dunning_label(level: i32) -> &'static str {
    DUNNING_LEVEL_LABEL[((level - 1).max(0) as usize).min(2)]
}

// ---------------------------------------------------------------------------
// Invoice dunning
// ---------------------------------------------------------------------------

/// A dunning step that has come due — one unpaid, already-sent invoice.
#[derive(Debug, Serialize)]
pub(crate) struct DueInvoiceReminder {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub inquiry_id: Uuid,
    pub invoice_number: String,
    pub level: i32,
    pub level_label: &'static str,
    pub remind_after: NaiveDate,
    /// Days since this step became due. 0 = due today.
    pub days_overdue: i64,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
}

/// Every dunning step that is due today or overdue.
pub(crate) async fn list_due_invoice_reminders(
    db: &PgPool,
) -> Result<Vec<DueInvoiceReminder>, ApiError> {
    let today = Utc::now().date_naive();
    let rows = invoice_reminder_repo::fetch_due(db).await?;
    Ok(rows
        .into_iter()
        .map(|r| DueInvoiceReminder {
            level_label: dunning_label(r.level),
            days_overdue: (today - r.remind_after).num_days(),
            id: r.id,
            invoice_id: r.invoice_id,
            inquiry_id: r.inquiry_id,
            invoice_number: r.invoice_number,
            level: r.level,
            remind_after: r.remind_after,
            customer_name: r.customer_name,
            customer_email: r.customer_email,
        })
        .collect())
}

/// Sends the dunning email at the reminder's current level and advances the ladder.
///
/// After a send, the next level comes due in [`DUNNING_INTERVAL_DAYS`]; past level 3
/// the reminder closes instead — we don't escalate past the 2. Mahnung automatically.
///
/// Returns the level that was just sent, and its label.
pub(crate) async fn send_dunning(
    db: &PgPool,
    email_config: &aust_core::config::EmailConfig,
    reminder_id: Uuid,
) -> Result<(i32, &'static str), ApiError> {
    let row = invoice_reminder_repo::fetch_one(db, reminder_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Erinnerung nicht gefunden".into()))?;

    let email = row
        .customer_email
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest("Kunde hat keine E-Mail-Adresse".into()))?;
    let name = row
        .customer_name
        .as_deref()
        .unwrap_or("Sehr geehrte Damen und Herren");
    let label = dunning_label(row.level);
    let subject = format!("{label}: Rechnung {}", row.invoice_number);
    let body = build_dunning_email(name, &row.invoice_number, label, row.level);

    admin_emails::send_plain_email(email_config, email, &subject, &body)
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

    let next = Utc::now().date_naive() + chrono::Days::new(DUNNING_INTERVAL_DAYS);
    invoice_reminder_repo::advance(db, reminder_id, next).await?;

    Ok((row.level, label))
}

/// Pushes a dunning step out by `days`, leaving its level untouched.
pub(crate) async fn snooze_dunning(
    db: &PgPool,
    reminder_id: Uuid,
    days: u32,
) -> Result<NaiveDate, ApiError> {
    let remind_after = Utc::now().date_naive() + chrono::Days::new(days as u64);
    invoice_reminder_repo::snooze(db, reminder_id, remind_after).await?;
    Ok(remind_after)
}

fn build_dunning_email(name: &str, invoice_number: &str, label: &str, level: i32) -> String {
    let urgency = match level {
        1 => "Möglicherweise ist die Zahlung in Bearbeitung — bitte prüfen Sie Ihre Unterlagen.",
        2 => "Wir bitten Sie dringend, den ausstehenden Betrag umgehend zu begleichen.",
        _ => "Dies ist unsere letzte Erinnerung vor weiteren rechtlichen Schritten.",
    };
    format!(
        "Guten Tag {name},\n\n\
         {label} für Rechnung {invoice_number}\n\n\
         laut unseren Unterlagen ist die oben genannte Rechnung noch offen.\n\
         {urgency}\n\n\
         Sollten Sie die Zahlung bereits veranlasst haben, bitten wir Sie, \
         diese E-Mail als gegenstandslos zu betrachten.\n\n\
         Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
         Mit freundlichen Grüßen\n\
         Ihr Team von Aust Umzüge & Haushaltsauflösungen",
    )
}

// ---------------------------------------------------------------------------
// Marking paid
// ---------------------------------------------------------------------------

/// What marking an invoice paid did, and what the caller should do next.
#[derive(Debug, Serialize)]
pub(crate) struct PaidOutcome {
    /// `"umzug"` (core invoice) or `"lagerung"` (storage invoice).
    pub kind: &'static str,
    pub paid_at: DateTime<Utc>,
    /// Set for `"umzug"` only — storage invoices hang off a contract, not an inquiry.
    pub inquiry_id: Option<Uuid>,
    pub customer_name: Option<String>,
    /// True when the whole inquiry just flipped to `paid` (its last invoice settled).
    pub inquiry_settled: bool,
    /// True when the caller should ask about a Google review: the job is fully
    /// settled and nobody has answered the review question for it yet.
    pub review_prompt: bool,
}

/// Marks an invoice paid, whichever register it lives in.
///
/// Accepts an id from either table because the Rechnungsausgangsbuch merges both
/// and its rows only carry an opaque id. Storage is probed first (the cheaper,
/// narrower table); a miss falls through to the core `invoices` table.
///
/// Side effects for a core invoice: any open dunning row is closed, and once the
/// inquiry has no unpaid invoices left it transitions to `paid`.
pub(crate) async fn mark_invoice_paid(
    db: &PgPool,
    id: Uuid,
    paid_at: DateTime<Utc>,
) -> Result<PaidOutcome, ApiError> {
    // Storage invoice?
    if storage_repo::mark_invoice_paid(db, id, paid_at).await? > 0 {
        return Ok(PaidOutcome {
            kind: "lagerung",
            paid_at,
            inquiry_id: None,
            customer_name: None,
            inquiry_settled: false,
            review_prompt: false,
        });
    }

    // Core invoice.
    let (inquiry_id, customer_name) = invoice_repo::fetch_inquiry_and_customer(db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Rechnung nicht gefunden".into()))?;

    invoice_repo::mark_paid(db, id, paid_at).await?;
    invoice_reminder_repo::close_for_invoice(db, id).await?;

    // A partial invoice pair only settles the inquiry once both halves are in.
    let inquiry_settled = invoice_repo::count_unpaid(db, inquiry_id).await? == 0;
    if inquiry_settled {
        invoice_repo::transition_inquiry_to_paid(db, inquiry_id, paid_at).await?;
    }

    // Don't re-ask a question Alex has already answered (sent / skipped / deferred).
    let review_answered = review_repo::status_for(db, inquiry_id).await?.is_some();

    Ok(PaidOutcome {
        kind: "umzug",
        paid_at,
        inquiry_id: Some(inquiry_id),
        customer_name,
        inquiry_settled,
        review_prompt: inquiry_settled && !review_answered,
    })
}

// ---------------------------------------------------------------------------
// Review requests
// ---------------------------------------------------------------------------

/// A review request whose deferral has run out.
#[derive(Debug, Serialize)]
pub(crate) struct DueReviewRequest {
    pub inquiry_id: Uuid,
    pub remind_after: NaiveDate,
    /// Days since the deferral expired. 0 = due today.
    pub days_overdue: i64,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
}

/// Every deferred review request that has come due.
pub(crate) async fn list_due_review_requests(
    db: &PgPool,
) -> Result<Vec<DueReviewRequest>, ApiError> {
    let today = Utc::now().date_naive();
    let rows = review_repo::fetch_pending_reminders(db).await?;
    Ok(rows
        .into_iter()
        .map(|r| DueReviewRequest {
            days_overdue: (today - r.remind_after).num_days(),
            inquiry_id: r.inquiry_id,
            remind_after: r.remind_after,
            customer_name: r.customer_name,
            customer_email: r.customer_email,
        })
        .collect())
}

/// Outcome of deciding what to do about an inquiry's review request.
#[derive(Debug, Serialize)]
pub(crate) struct ReviewRequestOutcome {
    /// `"sent"` | `"pending"` | `"skipped"` — mirrors `review_requests.status`.
    pub status: &'static str,
    /// Set for `"pending"`: when the reminder resurfaces.
    pub remind_after: Option<NaiveDate>,
}

/// Sends, defers, or skips the Google-review email for an inquiry.
///
/// `action` is one of:
/// * `"now"` — send the review email immediately and record it as sent
/// * `"later"` — defer by `remind_after_days` (default [`DEFAULT_REVIEW_SNOOZE_DAYS`])
/// * `"skip"` — never ask for this job
///
/// The row is upserted, so Alex can change his mind: a deferred request can still
/// be sent or skipped later.
pub(crate) async fn decide_review_request(
    db: &PgPool,
    email_config: &aust_core::config::EmailConfig,
    inquiry_id: Uuid,
    action: &str,
    remind_after_days: Option<u32>,
) -> Result<ReviewRequestOutcome, ApiError> {
    match action {
        "now" => {
            let customer = customer_repo::fetch_by_inquiry_id(db, inquiry_id).await?;
            let email = customer
                .email
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest("Kunde hat keine E-Mail-Adresse".into()))?;
            let subject = "Wie war Ihr Umzug? Wir freuen uns über Ihre Bewertung!";
            let body = build_review_email(&customer.display_name());

            admin_emails::send_plain_email(email_config, email, subject, &body)
                .await
                .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

            review_repo::upsert(db, inquiry_id, "sent", None, Some(Utc::now())).await?;
            Ok(ReviewRequestOutcome { status: "sent", remind_after: None })
        }
        "later" => {
            let days = remind_after_days.unwrap_or(DEFAULT_REVIEW_SNOOZE_DAYS);
            let remind_after = Utc::now().date_naive() + chrono::Days::new(days as u64);
            review_repo::upsert(db, inquiry_id, "pending", Some(remind_after), None).await?;
            Ok(ReviewRequestOutcome { status: "pending", remind_after: Some(remind_after) })
        }
        "skip" => {
            review_repo::upsert(db, inquiry_id, "skipped", None, None).await?;
            Ok(ReviewRequestOutcome { status: "skipped", remind_after: None })
        }
        _ => Err(ApiError::BadRequest(
            "Ungültige Aktion. Erlaubt: now, later, skip".into(),
        )),
    }
}

fn build_review_email(customer_name: &str) -> String {
    format!(
        "Guten Tag {customer_name},\n\n\
         vielen Dank, dass Sie Aust Umzüge & Haushaltsauflösungen für Ihren Umzug gewählt haben.\n\n\
         Wir würden uns sehr freuen, wenn Sie uns eine kurze Bewertung hinterlassen würden:\n\
         {GOOGLE_REVIEW_URL}\n\n\
         Ihre Meinung hilft uns, unsere Dienstleistungen stetig zu verbessern.\n\n\
         Mit freundlichen Grüßen\n\
         Ihr Team von Aust Umzüge & Haushaltsauflösungen",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dunning_labels_climb_and_clamp() {
        assert_eq!(dunning_label(1), "Zahlungserinnerung");
        assert_eq!(dunning_label(2), "1. Mahnung");
        assert_eq!(dunning_label(3), "2. Mahnung");
        // The repo clamps escalation at 3, but a stray level must not panic.
        assert_eq!(dunning_label(9), "2. Mahnung");
        assert_eq!(dunning_label(0), "Zahlungserinnerung");
    }

    #[test]
    fn dunning_email_carries_number_and_escalates_tone() {
        let first = build_dunning_email("Frau Schilling", "2026-0042", dunning_label(1), 1);
        assert!(first.contains("Frau Schilling"));
        assert!(first.contains("2026-0042"));
        assert!(first.contains("Zahlungserinnerung"));
        assert!(first.contains("in Bearbeitung"));

        let last = build_dunning_email("Frau Schilling", "2026-0042", dunning_label(3), 3);
        assert!(last.contains("2. Mahnung"));
        assert!(last.contains("rechtlichen Schritten"));
    }

    #[test]
    fn review_email_links_google() {
        let body = build_review_email("Herr Aust");
        assert!(body.contains("Herr Aust"));
        assert!(body.contains(GOOGLE_REVIEW_URL));
    }
}
