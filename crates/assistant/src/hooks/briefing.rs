//! Morning briefing assembler.
//!
//! Collects the key operational data points Alex needs at the start of each day:
//! - Today's calendar appointments
//! - Overdue invoices
//! - Pending offers (in `offer_ready` status)
//! - Unhandled inbound emails
//!
//! The assembly is pure read-only: no writes, no LLM calls. The scheduler wires
//! this into a morning cron job (not yet implemented — Phase 3).

use chrono::{NaiveDate, Utc};
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
    /// Format the briefing as a Telegram-ready markdown string (German).
    pub fn to_telegram_text(&self) -> String {
        let mut lines = vec![format!(
            "☀️ *Guten Morgen!* Hier ist die Zusammenfassung für den {}.",
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
                lines.push(format!("  • {} ({})", a.title, a.category));
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

    // 1. Today's calendar appointments.
    let appointments: Vec<(uuid::Uuid, String, String, Option<NaiveDate>)> = sqlx::query_as(
        r#"
        SELECT id, title, category, scheduled_date
        FROM calendar_items
        WHERE scheduled_date = $1
        ORDER BY start_time ASC
        "#,
    )
    .bind(today)
    .fetch_all(pool)
    .await?;

    briefing.todays_appointments = appointments
        .into_iter()
        .map(|(id, title, category, scheduled_date)| BriefingAppointment {
            id,
            title,
            category,
            scheduled_date,
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
    }

    #[test]
    fn telegram_text_contains_overdue_invoice() {
        let briefing = make_briefing();
        let text = briefing.to_telegram_text();
        assert!(text.contains("12026"));
        assert!(text.contains("überfällige"));
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
