//! Morning briefing assembler.
//!
//! Collects the key operational data points Alex needs at the start of each day:
//! - Today's calendar appointments
//! - Overdue invoices
//! - Pending offers (in `offer_ready` status)
//! - Unhandled inbound emails
//!
//! The assembly (`assemble`) is pure read-only: no writes, no LLM calls. The
//! `run_briefing_tick` scheduler at the bottom of this file posts it to the owner
//! chat at two fixed daily slots (07:00 + 15:00 Europe/Berlin), driven by a 60s
//! loop in `src/main.rs`.

use chrono::{NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::Result;

/// A calendar appointment in the briefing.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingAppointment {
    pub id: uuid::Uuid,
    pub title: String,
    pub category: String,
    pub scheduled_date: Option<NaiveDate>,
    /// Start time of day, if set.
    pub start_time: Option<NaiveTime>,
    /// Free-text location/address, if set.
    pub location: Option<String>,
    /// `"termin"` (internal calendar item) or `"auftrag"` (a scheduled move).
    pub kind: String,
}

/// An overdue invoice summary in the briefing.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingInvoice {
    pub id: uuid::Uuid,
    pub invoice_number: String,
    pub status: String,
    pub due_date: Option<NaiveDate>,
}

/// A pending offer in the briefing.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingOffer {
    pub id: uuid::Uuid,
    pub inquiry_id: uuid::Uuid,
    pub status: String,
}

/// An unhandled inbound email in the briefing.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingEmail {
    pub id: uuid::Uuid,
    pub subject: String,
    pub from_address: String,
}

/// The assembled morning briefing.
#[derive(Debug, Default, Serialize)]
pub struct Briefing {
    /// Calendar items scheduled for today.
    pub todays_appointments: Vec<BriefingAppointment>,
    /// Invoices in `sent` status whose `due_date` is in the past.
    pub overdue_invoices: Vec<BriefingInvoice>,
    /// Offers in `offer_ready` status (generated but not yet sent).
    pub pending_offers: Vec<BriefingOffer>,
    /// Inbound emails with status `received` (not yet processed).
    pub unhandled_emails: Vec<BriefingEmail>,
    /// The date this briefing was assembled.
    pub briefing_date: NaiveDate,
}

impl Briefing {
    /// Format the briefing as a Telegram-ready markdown string (German),
    /// defaulting to the morning greeting.
    pub fn to_telegram_text(&self) -> String {
        self.to_telegram_text_with_greeting("☀️ *Guten Morgen!*")
    }

    /// Format the briefing with a caller-supplied greeting line prefix, so the
    /// afternoon slot doesn't say "Guten Morgen". The date sentence and body are
    /// identical across slots.
    pub fn to_telegram_text_with_greeting(&self, greeting: &str) -> String {
        let mut lines = vec![format!(
            "{greeting} Hier ist die Zusammenfassung für den {}.",
            self.briefing_date.format("%d.%m.%Y")
        )];

        if self.todays_appointments.is_empty() {
            lines.push("📅 Keine Termine heute.".to_string());
        } else {
            lines.push(format!(
                "📅 *Heute {} Termin(e)*:",
                self.todays_appointments.len()
            ));
            for a in &self.todays_appointments {
                let time = a
                    .start_time
                    .map(|t| format!("{} ", t.format("%H:%M")))
                    .unwrap_or_default();
                let loc = a
                    .location
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" — {s}"))
                    .unwrap_or_default();
                lines.push(format!("  • {time}{} ({}){loc}", a.title, a.category));
            }
        }

        if !self.overdue_invoices.is_empty() {
            lines.push(format!(
                "⚠️ *{} überfällige Rechnung(en)*:",
                self.overdue_invoices.len()
            ));
            for inv in &self.overdue_invoices {
                let due = inv
                    .due_date
                    .map(|d| d.format("%d.%m.%Y").to_string())
                    .unwrap_or_else(|| "unbekannt".to_string());
                lines.push(format!("  • Rechnung {} — fällig {}", inv.invoice_number, due));
            }
        }

        if !self.pending_offers.is_empty() {
            lines.push(format!(
                "📝 *{} Angebot(e) bereit zum Versand*",
                self.pending_offers.len()
            ));
        }

        if !self.unhandled_emails.is_empty() {
            lines.push(format!(
                "📧 *{} unbearbeitete E-Mail(s)*",
                self.unhandled_emails.len()
            ));
        }

        lines.join("\n")
    }
}

/// Assemble the morning briefing from live DB data.
pub async fn assemble(pool: &PgPool) -> Result<Briefing> {
    let today = Utc::now().date_naive();
    let mut briefing = Briefing {
        briefing_date: today,
        ..Default::default()
    };

    // 1. Today's appointments — BOTH internal calendar items AND scheduled
    //    moving jobs (inquiries). Use overlap logic (start <= today <= end) so
    //    multi-day jobs show on every day they run, not just the start day.
    //    Previously this queried calendar_items with `scheduled_date = today`
    //    only, so every scheduled Auftrag and every day-2+ of a multi-day job
    //    was invisible in the briefing.
    type ApptRow = (
        uuid::Uuid,
        String,
        String,
        Option<NaiveDate>,
        Option<NaiveTime>,
        Option<String>,
        String,
    );

    let cal_items: Vec<ApptRow> = sqlx::query_as(
        r#"
        SELECT id, title, category, scheduled_date, start_time, location, 'termin'::text AS kind
        FROM calendar_items
        WHERE scheduled_date <= $1
          AND COALESCE(end_date, scheduled_date) >= $1
          AND status IS DISTINCT FROM 'cancelled'
        ORDER BY start_time ASC NULLS LAST
        "#,
    )
    .bind(today)
    .fetch_all(pool)
    .await?;

    let jobs: Vec<ApptRow> = sqlx::query_as(
        r#"
        SELECT
            i.id,
            COALESCE(
                NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                c.name, c.email, 'Anfrage'
            ) AS title,
            COALESCE(i.service_type, 'umzug') AS category,
            i.scheduled_date,
            i.start_time,
            NULLIF(TRIM(
                COALESCE(a.street, '') || ' ' || COALESCE(a.house_number, '') || ', ' ||
                COALESCE(a.postal_code, '') || ' ' || COALESCE(a.city, '')
            ), ', ') AS location,
            'auftrag'::text AS kind
        FROM inquiries i
        JOIN customers c ON c.id = i.customer_id
        LEFT JOIN addresses a ON a.id = i.origin_address_id
        WHERE i.scheduled_date IS NOT NULL
          AND i.scheduled_date <= $1
          AND COALESCE(i.end_date, i.scheduled_date) >= $1
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        ORDER BY i.start_time ASC NULLS LAST
        "#,
    )
    .bind(today)
    .fetch_all(pool)
    .await?;

    briefing.todays_appointments = cal_items
        .into_iter()
        .chain(jobs)
        .map(|(id, title, category, scheduled_date, start_time, location, kind)| {
            BriefingAppointment {
                id,
                title,
                category,
                scheduled_date,
                start_time,
                location,
                kind,
            }
        })
        .collect();

    // 2. Overdue invoices.
    let overdue: Vec<(uuid::Uuid, String, String, Option<NaiveDate>)> = sqlx::query_as(
        r#"
        SELECT id, invoice_number, status, due_date
        FROM invoices
        WHERE status = 'sent'
          AND due_date IS NOT NULL
          AND due_date < $1
        ORDER BY due_date ASC
        "#,
    )
    .bind(today)
    .fetch_all(pool)
    .await?;

    briefing.overdue_invoices = overdue
        .into_iter()
        .map(|(id, invoice_number, status, due_date)| BriefingInvoice {
            id,
            invoice_number,
            status,
            due_date,
        })
        .collect();

    // 3. Pending offers (offer_ready status on inquiries).
    let offers: Vec<(uuid::Uuid, uuid::Uuid, String)> = sqlx::query_as(
        r#"
        SELECT o.id, o.inquiry_id, o.status
        FROM offers o
        WHERE o.status = 'draft'
        ORDER BY o.created_at DESC
        LIMIT 20
        "#,
    )
    .fetch_all(pool)
    .await?;

    briefing.pending_offers = offers
        .into_iter()
        .map(|(id, inquiry_id, status)| BriefingOffer {
            id,
            inquiry_id,
            status,
        })
        .collect();

    // 4. Unhandled inbound emails.
    let emails: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        r#"
        SELECT m.id, COALESCE(m.subject, '(kein Betreff)'), m.from_address
        FROM email_messages m
        WHERE m.direction = 'inbound'
          AND m.status = 'received'
        ORDER BY m.created_at DESC
        LIMIT 20
        "#,
    )
    .fetch_all(pool)
    .await?;

    briefing.unhandled_emails = emails
        .into_iter()
        .map(|(id, subject, from_address)| BriefingEmail {
            id,
            subject,
            from_address,
        })
        .collect();

    Ok(briefing)
}

// ── Scheduled delivery ────────────────────────────────────────────────────────

use chrono::Timelike;
use chrono_tz::Europe::Berlin;
use tracing::{info, warn};

use crate::events::notifier::TelegramNotifier;

/// The fixed daily slots at which the briefing is auto-posted, as
/// `(Europe/Berlin hour, slot key)`. The slot key is persisted in
/// `agent_briefing_log` and selects the greeting. Requested by Alex: 07:00 and
/// 15:00 (feedback report 68ff999e).
const BRIEFING_SLOTS: &[(u32, &str)] = &[(7, "morning"), (15, "afternoon")];

/// How many hours after a slot's start we may still deliver it (catch-up after
/// downtime). Kept below the 8h gap between slots so a missed morning briefing
/// can never bleed into the afternoon window.
const CATCHUP_HOURS: u32 = 3;

/// Greeting prefix for each slot key.
fn slot_greeting(slot: &str) -> &'static str {
    match slot {
        "afternoon" => "🌤️ *Nachmittags-Update.*",
        _ => "☀️ *Guten Morgen!*",
    }
}

/// Run one briefing tick: if a fixed daily slot is currently due and hasn't been
/// delivered today, assemble the briefing and post it to the owner chat.
///
/// Driven every 60s by a `tokio::spawn` loop in `src/main.rs` — the same cadence
/// as the reminder tick. Idempotency comes from claiming the `(slot_date, slot)`
/// row in `agent_briefing_log`: only the tick that wins the `INSERT ... ON
/// CONFLICT DO NOTHING` actually sends, so restarts and overlapping ticks never
/// double-post. A failed send releases the claim so the next tick retries.
pub async fn run_briefing_tick(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    // Only nag if there's an owner chat to nag.
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let Some((owner_chat,)) = owner else {
        return Ok(());
    };

    let now_berlin = Utc::now().with_timezone(&Berlin);
    let hour = now_berlin.hour();
    let today = now_berlin.date_naive();

    let Some((_, slot)) = BRIEFING_SLOTS
        .iter()
        .find(|(slot_hour, _)| hour >= *slot_hour && hour < slot_hour + CATCHUP_HOURS)
    else {
        return Ok(());
    };

    // Claim the slot atomically. If another tick (or a prior run today) already
    // holds it, `fetch_optional` yields None and we do nothing.
    let claimed: Option<(NaiveDate,)> = sqlx::query_as(
        "INSERT INTO agent_briefing_log (slot_date, slot, chat_id) VALUES ($1, $2, $3) \
         ON CONFLICT (slot_date, slot) DO NOTHING RETURNING slot_date",
    )
    .bind(today)
    .bind(*slot)
    .bind(owner_chat)
    .fetch_optional(pool)
    .await?;
    if claimed.is_none() {
        return Ok(());
    }

    // From here the slot is claimed. Any failure — assembling the briefing or
    // posting it — must RELEASE the claim so the next tick retries rather than
    // leaving the slot marked delivered when nothing reached Alex.
    let deliver = async {
        let briefing = assemble(pool).await?;
        let body = briefing.to_telegram_text_with_greeting(slot_greeting(slot));
        notifier.post(owner_chat, body).await?;
        Ok::<(), crate::error::AssistantError>(())
    };

    match deliver.await {
        Ok(()) => {
            info!(slot = %slot, date = %today, "Daily briefing posted");
            Ok(())
        }
        Err(e) => {
            let _ =
                sqlx::query("DELETE FROM agent_briefing_log WHERE slot_date = $1 AND slot = $2")
                    .bind(today)
                    .bind(*slot)
                    .execute(pool)
                    .await;
            warn!("Daily briefing delivery failed, will retry: {e}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn make_briefing() -> Briefing {
        Briefing {
            briefing_date: NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(),
            todays_appointments: vec![BriefingAppointment {
                id: Uuid::now_v7(),
                title: "Umzug Müller".to_string(),
                category: "Umzug".to_string(),
                scheduled_date: Some(NaiveDate::from_ymd_opt(2026, 6, 9).unwrap()),
                start_time: NaiveTime::from_hms_opt(9, 30, 0),
                location: Some("Hauptstr. 1, 31137 Hildesheim".to_string()),
                kind: "auftrag".to_string(),
            }],
            overdue_invoices: vec![BriefingInvoice {
                id: Uuid::now_v7(),
                invoice_number: "12026".to_string(),
                status: "sent".to_string(),
                due_date: Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
            }],
            pending_offers: vec![],
            unhandled_emails: vec![],
        }
    }

    #[test]
    fn telegram_text_contains_appointment() {
        let briefing = make_briefing();
        let text = briefing.to_telegram_text();
        assert!(text.contains("Umzug Müller"));
        assert!(text.contains("09.06.2026"));
        // Time and address must now be surfaced in the line.
        assert!(text.contains("09:30"));
        assert!(text.contains("Hauptstr. 1"));
    }

    #[test]
    fn telegram_text_contains_overdue_invoice() {
        let briefing = make_briefing();
        let text = briefing.to_telegram_text();
        assert!(text.contains("12026"));
        assert!(text.contains("überfällige"));
    }

    #[test]
    fn afternoon_greeting_replaces_morning_header() {
        let briefing = make_briefing();
        let text = briefing.to_telegram_text_with_greeting(slot_greeting("afternoon"));
        assert!(text.contains("Nachmittags-Update"));
        assert!(!text.contains("Guten Morgen"));
        // Body is unchanged across slots.
        assert!(text.contains("Umzug Müller"));
        assert!(text.contains("09.06.2026"));
    }

    #[test]
    fn morning_slot_keeps_default_greeting() {
        assert_eq!(slot_greeting("morning"), "☀️ *Guten Morgen!*");
        // Unknown/legacy slot keys fall back to the morning greeting.
        assert_eq!(slot_greeting("whatever"), "☀️ *Guten Morgen!*");
    }

    #[test]
    fn briefing_with_no_items_shows_no_appointments() {
        let briefing = Briefing {
            briefing_date: NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(),
            ..Default::default()
        };
        let text = briefing.to_telegram_text();
        assert!(text.contains("Keine Termine"));
    }
}
