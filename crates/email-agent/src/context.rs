//! What we already know about the sender of an incoming mail.
//!
//! The auto-responder used to see nothing but the mail itself: every message was
//! treated as a fresh moving inquiry, so a customer with a booked job got asked for
//! their moving date, and a real question ("Wo ist Ihr Lager?") was answered with
//! invented facts. [`MailContext`] loads the customer's jobs, KVAs, invoices and
//! the recent thread from the database; [`MailContext::facts`] renders them as the
//! only facts the reply may use.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

/// Inquiry statuses after which a job is over — a mail from such a customer is
/// treated like one from a new contact when it asks for something new.
const CLOSED: &[&str] = &["rejected", "expired", "cancelled", "paid"];

/// Statuses from the KVA on: the customer's details are settled, so a mail from
/// them is never run through intake again (a "complete" intake would create a
/// duplicate inquiry and KVA).
const COMMITTED: &[&str] = &["offer_ready", "offer_sent", "accepted", "scheduled", "completed", "invoiced"];

/// How many earlier thread messages the reply gets to see.
const THREAD_MESSAGES: i64 = 6;
/// Characters kept per earlier thread message.
const THREAD_MESSAGE_CHARS: usize = 700;

#[derive(Debug, Clone, Default)]
pub(crate) struct MailContext {
    pub customer_name: Option<String>,
    pub customer_phone: Option<String>,
    pub jobs: Vec<JobFacts>,
    pub invoices: Vec<InvoiceFacts>,
    /// Earlier messages of this thread, oldest first.
    pub thread: Vec<ThreadMessage>,
}

#[derive(Debug, Clone)]
pub(crate) struct JobFacts {
    pub status: String,
    pub service_type: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub start_time: Option<NaiveTime>,
    pub origin: Option<String>,
    pub destination: Option<String>,
    pub volume_m3: Option<f64>,
    pub offer_number: Option<String>,
    pub offer_brutto_cents: Option<i64>,
    pub offer_sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl JobFacts {
    pub fn is_open(&self) -> bool {
        !CLOSED.contains(&self.status.as_str())
    }

    /// Whether the date is a booked appointment rather than a request.
    pub fn is_booked(&self) -> bool {
        matches!(self.status.as_str(), "accepted" | "scheduled" | "completed" | "invoiced" | "paid")
    }

    pub fn is_committed(&self) -> bool {
        COMMITTED.contains(&self.status.as_str())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InvoiceFacts {
    pub invoice_number: String,
    pub status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub due_date: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadMessage {
    pub inbound: bool,
    pub at: DateTime<Utc>,
    pub body: String,
}

impl MailContext {
    /// Load everything about `customer_email` and the thread. Errors are logged and
    /// leave the affected part empty — a missing fact is safer than no reply at all,
    /// because the reply is instructed never to state what it was not given.
    pub async fn load(
        db: &PgPool,
        customer_email: &str,
        thread_id: Uuid,
        exclude_message: Option<Uuid>,
    ) -> Self {
        let mut ctx = MailContext::default();

        let customer: Option<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
            SELECT id,
                   COALESCE(NULLIF(TRIM(COALESCE(first_name,'') || ' ' || COALESCE(last_name,'')), ''), name),
                   phone
            FROM customers
            WHERE lower(email) = lower($1) AND merged_into IS NULL
            LIMIT 1
            "#,
        )
        .bind(customer_email)
        .fetch_optional(db)
        .await
        .unwrap_or_else(|e| {
            warn!("Mail context: customer lookup failed: {e}");
            None
        });

        // The thread may be linked to a customer the sender address does not match
        // (a relative writing for the customer); prefer the thread's customer.
        let thread_customer: Option<Uuid> =
            sqlx::query_scalar("SELECT customer_id FROM email_threads WHERE id = $1")
                .bind(thread_id)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .flatten();

        let customer_id = thread_customer.or(customer.as_ref().map(|c| c.0));
        if let Some((cid, name, phone)) = &customer
            && Some(*cid) == customer_id
        {
            ctx.customer_name = name.clone();
            ctx.customer_phone = phone.clone();
        }

        if let Some(cid) = customer_id {
            if ctx.customer_name.is_none() {
                ctx.customer_name = sqlx::query_scalar(
                    "SELECT COALESCE(NULLIF(TRIM(COALESCE(first_name,'') || ' ' || COALESCE(last_name,'')), ''), name) \
                     FROM customers WHERE id = $1",
                )
                .bind(cid)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .flatten();
            }
            ctx.jobs = load_jobs(db, cid).await;
            ctx.invoices = load_invoices(db, cid).await;
        }

        ctx.thread = load_thread(db, thread_id, exclude_message).await;
        ctx
    }

    /// Whether the sender has a KVA or a job in progress.
    pub fn has_committed_job(&self) -> bool {
        self.jobs.iter().any(JobFacts::is_committed)
    }

    /// The facts block for the LLM. Only what is in here may appear in a reply.
    pub fn facts(&self, today: NaiveDate) -> String {
        let mut out = vec![format!("Heute ist {}.", fmt_date_long(today))];

        match &self.customer_name {
            Some(n) => out.push(format!("Kunde: {n}")),
            None => out.push("Kunde: unbekannt (noch kein Kundendatensatz)".into()),
        }
        if let Some(p) = &self.customer_phone {
            out.push(format!("Telefon des Kunden: {p}"));
        }

        if self.jobs.is_empty() {
            out.push("Aufträge/Anfragen: keine".into());
        } else {
            out.push("Aufträge/Anfragen (neueste zuerst):".into());
            for j in &self.jobs {
                out.push(format!("- {}", job_line(j)));
            }
        }

        if !self.invoices.is_empty() {
            out.push("Rechnungen:".into());
            for v in &self.invoices {
                out.push(format!("- {}", invoice_line(v)));
            }
        }

        if !self.thread.is_empty() {
            out.push("Bisheriger E-Mail-Verlauf (älteste zuerst):".into());
            for m in &self.thread {
                out.push(format!(
                    "[{} {}]\n{}",
                    if m.inbound { "Kunde" } else { "Wir" },
                    m.at.with_timezone(&chrono_tz::Europe::Berlin).format("%d.%m.%Y %H:%M"),
                    m.body
                ));
            }
        }
        out.join("\n")
    }

    /// One line for Alex in Telegram: who this is and where their job stands.
    pub fn summary(&self) -> String {
        let name = self.customer_name.as_deref().unwrap_or("Unbekannter Absender");
        let Some(job) = self.jobs.iter().find(|j| j.is_open()).or(self.jobs.first()) else {
            return format!("{name} · kein Auftrag im System");
        };
        let mut s = format!("{name} · {}", job_line_short(job));
        let open = self.jobs.iter().filter(|j| j.is_open()).count();
        if open > 1 {
            s.push_str(&format!(" (+{} weitere offene)", open - 1));
        }
        s
    }
}

async fn load_jobs(db: &PgPool, customer_id: Uuid) -> Vec<JobFacts> {
    type Row = (
        String,
        Option<String>,
        Option<NaiveDate>,
        Option<NaiveDate>,
        Option<NaiveTime>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<String>,
        Option<i64>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    );
    let rows: Result<Vec<Row>, _> = sqlx::query_as(
        r#"
        SELECT i.status, i.service_type, i.scheduled_date, i.end_date, i.start_time,
               NULLIF(CONCAT_WS(', ', NULLIF(oa.street,''), NULLIF(CONCAT_WS(' ', oa.postal_code, oa.city),'')), ''),
               NULLIF(CONCAT_WS(', ', NULLIF(da.street,''), NULLIF(CONCAT_WS(' ', da.postal_code, da.city),'')), ''),
               i.estimated_volume_m3::float8,
               o.offer_number,
               o.price_cents::int8,
               o.sent_at,
               i.created_at
        FROM inquiries i
        LEFT JOIN addresses oa ON oa.id = i.origin_address_id
        LEFT JOIN addresses da ON da.id = i.destination_address_id
        LEFT JOIN LATERAL (
            SELECT offer_number, price_cents, sent_at FROM offers
            WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled', 'superseded')
            ORDER BY created_at DESC LIMIT 1
        ) o ON TRUE
        WHERE i.customer_id = $1
        ORDER BY i.created_at DESC
        LIMIT 8
        "#,
    )
    .bind(customer_id)
    .fetch_all(db)
    .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| JobFacts {
                status: r.0,
                service_type: r.1,
                scheduled_date: r.2,
                end_date: r.3,
                start_time: r.4,
                origin: r.5,
                destination: r.6,
                volume_m3: r.7,
                offer_number: r.8,
                offer_brutto_cents: r.9,
                offer_sent_at: r.10,
                created_at: r.11,
            })
            .collect(),
        Err(e) => {
            warn!("Mail context: job lookup failed: {e}");
            Vec::new()
        }
    }
}

async fn load_invoices(db: &PgPool, customer_id: Uuid) -> Vec<InvoiceFacts> {
    type Row = (String, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<NaiveDate>);
    let rows: Result<Vec<Row>, _> = sqlx::query_as(
        r#"
        SELECT v.invoice_number, v.status, v.sent_at, v.paid_at, v.due_date
        FROM invoices v
        LEFT JOIN inquiries i ON i.id = v.inquiry_id
        WHERE COALESCE(v.customer_id, i.customer_id) = $1
          AND v.status <> 'cancelled'
        ORDER BY v.created_at DESC
        LIMIT 5
        "#,
    )
    .bind(customer_id)
    .fetch_all(db)
    .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|r| InvoiceFacts {
                invoice_number: r.0,
                status: r.1,
                sent_at: r.2,
                paid_at: r.3,
                due_date: r.4,
            })
            .collect(),
        Err(e) => {
            warn!("Mail context: invoice lookup failed: {e}");
            Vec::new()
        }
    }
}

async fn load_thread(db: &PgPool, thread_id: Uuid, exclude: Option<Uuid>) -> Vec<ThreadMessage> {
    // Drafts that were never sent are not part of the conversation.
    type Row = (String, DateTime<Utc>, Option<String>);
    let rows: Result<Vec<Row>, _> = sqlx::query_as(
        r#"
        SELECT direction, created_at, body_text FROM email_messages
        WHERE thread_id = $1
          AND ($2::uuid IS NULL OR id <> $2)
          AND (direction = 'inbound' OR status IS NULL OR status NOT IN ('draft', 'discarded'))
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(thread_id)
    .bind(exclude)
    .bind(THREAD_MESSAGES)
    .fetch_all(db)
    .await;
    match rows {
        Ok(mut rows) => {
            rows.reverse();
            rows.into_iter()
                .map(|(direction, at, body)| {
                    let (text, _) = crate::email_notification::strip_quoted_history(
                        body.as_deref().unwrap_or(""),
                    );
                    let text = text.trim();
                    let body = if text.chars().count() > THREAD_MESSAGE_CHARS {
                        format!(
                            "{} […]",
                            crate::text::truncate_on_char_boundary(text, THREAD_MESSAGE_CHARS)
                        )
                    } else {
                        text.to_string()
                    };
                    ThreadMessage { inbound: direction == "inbound", at, body }
                })
                .collect()
        }
        Err(e) => {
            warn!("Mail context: thread lookup failed: {e}");
            Vec::new()
        }
    }
}

const WEEKDAYS: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];

fn fmt_date_long(d: NaiveDate) -> String {
    use chrono::Datelike;
    format!("{}, {}", WEEKDAYS[d.weekday().num_days_from_monday() as usize], d.format("%d.%m.%Y"))
}

fn fmt_euro(cents: i64) -> String {
    let euros = cents / 100;
    let rest = (cents % 100).abs();
    let mut digits = euros.abs().to_string();
    let mut grouped = String::new();
    while digits.len() > 3 {
        let tail = digits.split_off(digits.len() - 3);
        grouped = format!(".{tail}{grouped}");
    }
    format!("{}{digits}{grouped},{rest:02} €", if cents < 0 { "-" } else { "" })
}

/// German label for an inquiry status, phrased for the customer's situation.
pub(crate) fn status_label(status: &str) -> &str {
    match status {
        "pending" | "info_requested" | "estimating" | "estimated" => "Anfrage in Bearbeitung, noch kein Angebot",
        "offer_ready" => "Angebot erstellt, noch nicht verschickt",
        "offer_sent" => "Angebot verschickt, Antwort des Kunden offen",
        "accepted" => "Angebot angenommen (Auftrag)",
        "scheduled" => "Termin fest eingeplant",
        "completed" => "Auftrag durchgeführt, Rechnung offen",
        "invoiced" => "Rechnung gestellt",
        "paid" => "bezahlt, abgeschlossen",
        "rejected" => "Angebot abgelehnt",
        "expired" => "Angebot abgelaufen",
        "cancelled" => "storniert",
        other => other,
    }
}

fn date_span(j: &JobFacts) -> Option<String> {
    let start = j.scheduled_date?;
    Some(match j.end_date.filter(|e| *e > start) {
        Some(end) => format!("{} bis {}", start.format("%d.%m.%Y"), end.format("%d.%m.%Y")),
        None => start.format("%d.%m.%Y").to_string(),
    })
}

fn job_line(j: &JobFacts) -> String {
    let mut parts = vec![status_label(&j.status).to_string()];
    if let Some(t) = &j.service_type {
        parts.push(format!("Art: {t}"));
    }
    match date_span(j) {
        Some(d) => {
            let time = j
                .start_time
                .map(|t| format!(", Beginn {} Uhr", t.format("%H:%M")))
                .unwrap_or_default();
            // Before the customer accepts, the date is only what they asked for.
            let label = if j.is_booked() {
                "Termin (fest)"
            } else {
                "Wunschtermin (noch nicht fest eingeplant)"
            };
            parts.push(format!("{label}: {d}{time}"));
        }
        None => parts.push("Termin: noch keiner".into()),
    }
    if let Some(o) = &j.origin {
        parts.push(format!("von: {o}"));
    }
    if let Some(d) = &j.destination {
        parts.push(format!("nach: {d}"));
    }
    if let Some(v) = j.volume_m3.filter(|v| *v > 0.0) {
        parts.push(format!("Volumen: {v:.1} m³"));
    }
    if let Some(n) = &j.offer_number {
        let price = j
            .offer_brutto_cents
            .map(|c| format!(" über {} brutto", fmt_euro(c)))
            .unwrap_or_default();
        let sent = j
            .offer_sent_at
            .map(|s| format!(", verschickt am {}", s.format("%d.%m.%Y")))
            .unwrap_or_default();
        parts.push(format!("Angebot {n}{price}{sent}"));
    }
    parts.push(format!("angefragt am {}", j.created_at.format("%d.%m.%Y")));
    parts.join(" · ")
}

fn job_line_short(j: &JobFacts) -> String {
    let mut s = status_label(&j.status).to_string();
    if let Some(d) = date_span(j) {
        s.push_str(&format!(" · {d}"));
    }
    if let Some(n) = &j.offer_number {
        s.push_str(&format!(" · KVA {n}"));
        if let Some(c) = j.offer_brutto_cents {
            s.push_str(&format!(" ({})", fmt_euro(c)));
        }
    }
    s
}

fn invoice_line(v: &InvoiceFacts) -> String {
    let mut s = format!("Rechnung {}", v.invoice_number);
    if let Some(p) = v.paid_at {
        s.push_str(&format!(", bezahlt am {}", p.format("%d.%m.%Y")));
    } else if v.status == "sent" {
        s.push_str(", offen");
        if let Some(sent) = v.sent_at {
            s.push_str(&format!(", verschickt am {}", sent.format("%d.%m.%Y")));
        }
        if let Some(d) = v.due_date {
            s.push_str(&format!(", fällig am {}", d.format("%d.%m.%Y")));
        }
    } else {
        s.push_str(&format!(", Status {}", v.status));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(status: &str) -> JobFacts {
        JobFacts {
            status: status.into(),
            service_type: Some("entruempelung".into()),
            scheduled_date: NaiveDate::from_ymd_opt(2026, 10, 14),
            end_date: NaiveDate::from_ymd_opt(2026, 10, 15),
            start_time: NaiveTime::from_hms_opt(8, 0, 0),
            origin: Some("Knollenstr. 5, 31134 Hildesheim".into()),
            destination: None,
            volume_m3: Some(165.0),
            offer_number: Some("2026-0359".into()),
            offer_brutto_cents: Some(615_000),
            offer_sent_at: None,
            created_at: DateTime::parse_from_rfc3339("2026-10-05T19:24:45Z").unwrap().with_timezone(&Utc),
        }
    }

    #[test]
    fn facts_carry_the_booked_job() {
        let ctx = MailContext {
            customer_name: Some("Vera Sharma".into()),
            jobs: vec![job("accepted")],
            ..Default::default()
        };
        let f = ctx.facts(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap());
        assert!(f.contains("Donnerstag, 08.10.2026"));
        assert!(f.contains("Angebot angenommen"));
        assert!(f.contains("Termin (fest): 14.10.2026 bis 15.10.2026, Beginn 08:00 Uhr"));
        assert!(f.contains("Angebot 2026-0359 über 6.150,00 € brutto"));
        assert!(ctx.has_committed_job());
        assert_eq!(
            ctx.summary(),
            "Vera Sharma · Angebot angenommen (Auftrag) · 14.10.2026 bis 15.10.2026 · KVA 2026-0359 (6.150,00 €)"
        );
    }

    #[test]
    fn closed_and_early_jobs_are_not_committed() {
        let ctx = MailContext { jobs: vec![job("rejected"), job("paid"), job("info_requested")], ..Default::default() };
        assert!(!ctx.has_committed_job());
    }

    #[test]
    fn requested_date_is_not_a_booking() {
        let ctx = MailContext { jobs: vec![job("offer_sent")], ..Default::default() };
        let f = ctx.facts(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap());
        assert!(f.contains("Wunschtermin (noch nicht fest eingeplant): 14.10.2026"));
    }

    #[test]
    fn unknown_sender_summary() {
        assert_eq!(MailContext::default().summary(), "Unbekannter Absender · kein Auftrag im System");
    }

    #[test]
    fn euro_formatting() {
        assert_eq!(fmt_euro(615_000), "6.150,00 €");
        assert_eq!(fmt_euro(123_456_789), "1.234.567,89 €");
        assert_eq!(fmt_euro(5), "0,05 €");
    }
}
