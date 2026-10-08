//! Daily briefing and the evening early-start preview.
//!
//! The briefing is the day at a glance for Alex:
//! - **Heute**: every appointment (moves, Termine, Besichtigungen/Halteverbote)
//!   with time, route, volume and crew, flagging appointments nobody is assigned to
//! - **Morgen**: a one-line preview
//! - **Geld**: open and overdue invoices, finished jobs without an invoice
//! - **Zu tun**: new inquiries, inquiries waiting for a KVA, KVAs not sent or not
//!   answered, unanswered emails, missing worker hours, vehicle deadlines
//!
//! Sections with nothing to say are left out, so a quiet day is a short message.
//!
//! The assembly (`assemble`) is read-only: no writes, no LLM calls. Two schedulers
//! at the bottom post to the owner chat, both driven by a 60s loop in
//! `src/main.rs`: `run_briefing_tick` at 07:00 + 15:00 Europe/Berlin, and
//! `run_evening_preview_tick` at 21:00 with tomorrow's appointments that start
//! before 09:00 (sent only when there are some).

use chrono::{Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::Result;

/// How far back missing worker hours are reported.
const MISSING_HOURS_DAYS: i64 = 7;
/// How far back finished-but-uninvoiced jobs are reported. Older ones are
/// bookkeeping archaeology, not today's work.
const UNINVOICED_DAYS: i64 = 60;
/// Inquiries and KVAs older than this are treated as dead and left out.
const PIPELINE_DAYS: i64 = 45;
/// A sent invoice without a due date counts as overdue after this many days.
const DEFAULT_PAYMENT_DAYS: i64 = 14;
/// Vehicle deadlines are announced this many days ahead.
const VEHICLE_LOOKAHEAD_DAYS: i64 = 14;
/// Appointments starting before this time get the 21:00 heads-up the day before.
const EARLY_START: (u32, u32) = (9, 0);
/// List entries shown per section before "… und n weitere".
const LIST_CAP: usize = 5;

/// One appointment on a given day.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingAppointment {
    pub id: uuid::Uuid,
    pub title: String,
    pub category: String,
    pub scheduled_date: Option<NaiveDate>,
    /// Start time of day, if set.
    pub start_time: Option<NaiveTime>,
    /// Address (moves: the origin), if set.
    pub location: Option<String>,
    /// Moves only: the destination address.
    pub destination: Option<String>,
    /// Moves only: estimated volume.
    pub volume_m3: Option<f64>,
    /// First names of the workers assigned for that day.
    pub crew: Vec<String>,
    /// `"termin"` (calendar item), `"auftrag"` (a move) or `"zusatztermin"`
    /// (Besichtigung, Halteverbot … attached to an inquiry).
    pub kind: String,
}

/// An open invoice.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingInvoice {
    pub id: uuid::Uuid,
    pub invoice_number: String,
    pub customer_name: Option<String>,
    /// When it is (or was assumed to be) due.
    pub due_date: NaiveDate,
}

/// A job whose work is done but which has no invoice yet.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingUninvoicedJob {
    pub inquiry_id: uuid::Uuid,
    pub customer_name: String,
    pub last_day: NaiveDate,
}

/// A worker's day with no hours recorded.
#[derive(Debug, Clone, Serialize)]
pub struct BriefingMissingHours {
    pub employee: String,
    pub job: String,
    pub job_date: NaiveDate,
}

/// An upcoming vehicle deadline (TÜV, Inspektion, …).
#[derive(Debug, Clone, Serialize)]
pub struct BriefingVehicleDeadline {
    pub vehicle: String,
    pub label: String,
    pub due_date: NaiveDate,
}

/// The assembled briefing.
#[derive(Debug, Default, Serialize)]
pub struct Briefing {
    /// The day this briefing is about (Europe/Berlin).
    pub briefing_date: NaiveDate,
    pub todays_appointments: Vec<BriefingAppointment>,
    pub tomorrows_appointments: Vec<BriefingAppointment>,
    /// Sent, unpaid invoices.
    pub open_invoice_count: i64,
    /// The open invoices past their due date (or `DEFAULT_PAYMENT_DAYS` after sending).
    pub overdue_invoices: Vec<BriefingInvoice>,
    pub uninvoiced_jobs: Vec<BriefingUninvoicedJob>,
    /// Customer names of inquiries created in the last 24 hours.
    pub new_inquiries: Vec<String>,
    /// Inquiries still waiting for a KVA.
    pub inquiries_without_offer: i64,
    /// KVAs generated but not sent.
    pub offers_not_sent: i64,
    /// KVAs sent and not yet answered.
    pub offers_awaiting_reply: i64,
    /// Days since the oldest unanswered KVA was sent.
    pub oldest_offer_wait_days: Option<i64>,
    /// Inbound emails nobody has handled.
    pub unhandled_emails: i64,
    pub missing_hours: Vec<BriefingMissingHours>,
    pub vehicle_deadlines: Vec<BriefingVehicleDeadline>,
}

fn weekday_de(d: NaiveDate) -> &'static str {
    match d.weekday() {
        Weekday::Mon => "Montag",
        Weekday::Tue => "Dienstag",
        Weekday::Wed => "Mittwoch",
        Weekday::Thu => "Donnerstag",
        Weekday::Fri => "Freitag",
        Weekday::Sat => "Samstag",
        Weekday::Sun => "Sonntag",
    }
}

/// "1 Termin" / "3 Termine".
fn count_de(n: impl Into<i64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

fn fmt_date(d: NaiveDate) -> String {
    d.format("%d.%m.").to_string()
}

/// Push up to `LIST_CAP` lines, then a "… und n weitere" line.
fn push_capped<T>(lines: &mut Vec<String>, items: &[T], f: impl Fn(&T) -> String) {
    for it in items.iter().take(LIST_CAP) {
        lines.push(f(it));
    }
    if items.len() > LIST_CAP {
        lines.push(format!("  … und {} weitere", items.len() - LIST_CAP));
    }
}

impl BriefingAppointment {
    /// Lines for one appointment: headline, route, crew.
    fn lines(&self) -> Vec<String> {
        let time = self
            .start_time
            .map(|t| format!("{} ", t.format("%H:%M")))
            .unwrap_or_default();
        let volume = self
            .volume_m3
            .filter(|v| *v > 0.0)
            .map(|v| format!(" · {v:.0} m³"))
            .unwrap_or_default();
        let mut out = vec![format!("• {time}{}{volume}", self.title)];

        let from = self.location.as_deref().filter(|s| !s.is_empty());
        let to = self.destination.as_deref().filter(|s| !s.is_empty());
        match (from, to) {
            (Some(f), Some(t)) => out.push(format!("   {f} → {t}")),
            (Some(f), None) => out.push(format!("   {f}")),
            (None, Some(t)) => out.push(format!("   → {t}")),
            (None, None) => {}
        }

        if self.crew.is_empty() {
            out.push("   ⚠️ niemand eingeteilt".to_string());
        } else {
            out.push(format!("   👷 {}", self.crew.join(", ")));
        }
        out
    }
}

impl Briefing {
    /// Telegram text with the morning greeting.
    pub fn to_telegram_text(&self) -> String {
        self.to_telegram_text_with_greeting("☀️ Guten Morgen!")
    }

    /// Telegram text (plain, no Markdown) with a caller-supplied greeting.
    pub fn to_telegram_text_with_greeting(&self, greeting: &str) -> String {
        let d = self.briefing_date;
        let mut lines = vec![format!(
            "{greeting} {} {}{}",
            weekday_de(d),
            fmt_date(d),
            d.year()
        )];

        // ── Heute ──
        lines.push(String::new());
        if self.todays_appointments.is_empty() {
            lines.push("📅 Heute keine Termine.".to_string());
        } else {
            lines.push(format!("📅 Heute {}:", count_de(self.todays_appointments.len() as i64, "Termin", "Termine")));
            for a in &self.todays_appointments {
                lines.extend(a.lines());
            }
        }

        // ── Morgen ──
        let tomorrow = d + chrono::Duration::days(1);
        lines.push(String::new());
        match self.tomorrows_appointments.first() {
            None => lines.push(format!("🔜 Morgen ({}) frei.", weekday_de(tomorrow))),
            Some(first) => {
                let unstaffed = self
                    .tomorrows_appointments
                    .iter()
                    .filter(|a| a.crew.is_empty())
                    .count();
                let first_time = first
                    .start_time
                    .map(|t| format!("ab {} ", t.format("%H:%M")))
                    .unwrap_or_default();
                let mut line = format!(
                    "🔜 Morgen ({}): {}, {first_time}{}",
                    weekday_de(tomorrow),
                    count_de(self.tomorrows_appointments.len() as i64, "Termin", "Termine"),
                    first.title
                );
                if self.tomorrows_appointments.len() > 1 {
                    line.push_str(" …");
                }
                lines.push(line);
                if unstaffed > 0 {
                    lines.push(format!("   ⚠️ {unstaffed} davon ohne Team"));
                }
            }
        }

        // ── Geld ──
        let mut money = Vec::new();
        if self.open_invoice_count > 0 {
            let overdue = if self.overdue_invoices.is_empty() {
                String::new()
            } else {
                format!(", davon {} überfällig:", self.overdue_invoices.len())
            };
            money.push(format!("• {} offen{overdue}", count_de(self.open_invoice_count, "Rechnung", "Rechnungen")));
            push_capped(&mut money, &self.overdue_invoices, |inv| {
                let days = (d - inv.due_date).num_days();
                let who = inv
                    .customer_name
                    .as_deref()
                    .map(|n| format!(" {n}"))
                    .unwrap_or_default();
                format!("   – {}{who}, {days} Tage drüber", inv.invoice_number)
            });
        }
        if !self.uninvoiced_jobs.is_empty() {
            money.push(format!(
                "• {} ohne Rechnung:",
                count_de(self.uninvoiced_jobs.len() as i64, "erledigter Auftrag", "erledigte Aufträge")
            ));
            push_capped(&mut money, &self.uninvoiced_jobs, |j| {
                format!("   – {} ({})", j.customer_name, fmt_date(j.last_day))
            });
        }
        if !money.is_empty() {
            lines.push(String::new());
            lines.push("💶 Geld".to_string());
            lines.extend(money);
        }

        // ── Zu tun ──
        let mut todo = Vec::new();
        if !self.new_inquiries.is_empty() {
            let names = self
                .new_inquiries
                .iter()
                .take(LIST_CAP)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if self.new_inquiries.len() > LIST_CAP { " …" } else { "" };
            todo.push(format!(
                "• {} seit gestern: {names}{more}",
                count_de(self.new_inquiries.len() as i64, "neue Anfrage", "neue Anfragen")
            ));
        }
        if self.inquiries_without_offer > 0 {
            todo.push(format!(
                "• {} auf einen KVA",
                count_de(self.inquiries_without_offer, "Anfrage wartet", "Anfragen warten")
            ));
        }
        if self.offers_not_sent > 0 {
            todo.push(format!("• {} fertig, aber nicht verschickt", count_de(self.offers_not_sent, "KVA", "KVAs")));
        }
        if self.offers_awaiting_reply > 0 {
            let oldest = self
                .oldest_offer_wait_days
                .map(|n| format!(" (ältester seit {n} Tagen)"))
                .unwrap_or_default();
            todo.push(format!(
                "• {} ohne Antwort{oldest}",
                count_de(self.offers_awaiting_reply, "KVA", "KVAs")
            ));
        }
        if self.unhandled_emails > 0 {
            todo.push(format!("• {}", count_de(self.unhandled_emails, "unbeantwortete E-Mail", "unbeantwortete E-Mails")));
        }
        if !self.missing_hours.is_empty() {
            todo.push(format!("• 🕒 Stunden fehlen ({}):", self.missing_hours.len()));
            push_capped(&mut todo, &self.missing_hours, |m| {
                format!("   – {}: {} ({})", m.employee, m.job, fmt_date(m.job_date))
            });
        }
        for v in &self.vehicle_deadlines {
            let when = if v.due_date < d {
                format!("seit {} überfällig", fmt_date(v.due_date))
            } else {
                format!("fällig {}", fmt_date(v.due_date))
            };
            todo.push(format!("• 🚚 {}: {} {when}", v.vehicle, v.label));
        }
        if !todo.is_empty() {
            lines.push(String::new());
            lines.push("📋 Zu tun".to_string());
            lines.extend(todo);
        }

        lines.join("\n")
    }
}

/// Telegram text for the 21:00 preview, or `None` when nothing starts early.
pub fn evening_preview_text(tomorrow: NaiveDate, appts: &[BriefingAppointment]) -> Option<String> {
    let cutoff = NaiveTime::from_hms_opt(EARLY_START.0, EARLY_START.1, 0).expect("valid time");
    let early: Vec<&BriefingAppointment> = appts
        .iter()
        .filter(|a| a.start_time.is_some_and(|t| t < cutoff))
        .collect();
    if early.is_empty() {
        return None;
    }
    let mut lines = vec![
        format!(
            "🌙 Morgen früh ({} {}) geht's zeitig los:",
            weekday_de(tomorrow),
            fmt_date(tomorrow)
        ),
        String::new(),
    ];
    for a in early {
        lines.extend(a.lines());
    }
    Some(lines.join("\n"))
}

type ApptRow = (
    uuid::Uuid,
    String,
    String,
    Option<NaiveDate>,
    Option<NaiveTime>,
    Option<String>,
    Option<String>,
    Option<f64>,
    Vec<String>,
    String,
);

/// Every appointment on `day`: calendar items, moves (multi-day ones on each of
/// their days) and the Zusatztermine attached to inquiries, ordered by start time.
pub async fn appointments_on(pool: &PgPool, day: NaiveDate) -> Result<Vec<BriefingAppointment>> {
    let cal_items: Vec<ApptRow> = sqlx::query_as(
        r#"
        SELECT ci.id, ci.title, ci.category, ci.scheduled_date, ci.start_time, ci.location,
               NULL::text, NULL::float8,
               ARRAY(SELECT e.first_name::text FROM calendar_item_employees cie
                     JOIN employees e ON e.id = cie.employee_id
                     WHERE cie.calendar_item_id = ci.id AND cie.job_date = $1
                     ORDER BY e.first_name),
               'termin'::text
        FROM calendar_items ci
        WHERE ci.scheduled_date <= $1
          AND COALESCE(ci.end_date, ci.scheduled_date) >= $1
          AND ci.status IS DISTINCT FROM 'cancelled'
        "#,
    )
    .bind(day)
    .fetch_all(pool)
    .await?;

    let jobs: Vec<ApptRow> = sqlx::query_as(
        r#"
        SELECT
            i.id,
            INITCAP(COALESCE(i.service_type, 'umzug')) || ' ' || COALESCE(
                NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                c.name, c.email, 'Anfrage'
            ),
            COALESCE(i.service_type, 'umzug'),
            i.scheduled_date,
            i.start_time,
            NULLIF(TRIM(BOTH ', ' FROM CONCAT_WS(', ',
                NULLIF(TRIM(COALESCE(o.street,'') || ' ' || COALESCE(o.house_number,'')), ''),
                NULLIF(o.city, ''))), ''),
            NULLIF(TRIM(BOTH ', ' FROM CONCAT_WS(', ',
                NULLIF(TRIM(COALESCE(d.street,'') || ' ' || COALESCE(d.house_number,'')), ''),
                NULLIF(d.city, ''))), ''),
            i.estimated_volume_m3::float8,
            ARRAY(SELECT e.first_name::text FROM inquiry_employees ie
                  JOIN employees e ON e.id = ie.employee_id
                  WHERE ie.inquiry_id = i.id AND ie.job_date = $1
                  ORDER BY e.first_name),
            'auftrag'::text
        FROM inquiries i
        JOIN customers c ON c.id = i.customer_id
        LEFT JOIN addresses o ON o.id = i.origin_address_id
        LEFT JOIN addresses d ON d.id = i.destination_address_id
        WHERE i.scheduled_date IS NOT NULL
          AND i.scheduled_date <= $1
          AND COALESCE(i.end_date, i.scheduled_date) >= $1
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        "#,
    )
    .bind(day)
    .fetch_all(pool)
    .await?;

    let extras: Vec<ApptRow> = sqlx::query_as(
        r#"
        SELECT
            ia.id,
            INITCAP(ia.kind) || COALESCE(' ' || NULLIF(TRIM(
                COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''), ' ' || c.name, ''),
            ia.kind,
            ia.scheduled_date,
            ia.start_time,
            COALESCE(NULLIF(ia.location, ''),
                NULLIF(TRIM(BOTH ', ' FROM CONCAT_WS(', ',
                    NULLIF(TRIM(COALESCE(a.street,'') || ' ' || COALESCE(a.house_number,'')), ''),
                    NULLIF(a.city, ''))), '')),
            NULL::text, NULL::float8,
            ARRAY(SELECT e.first_name::text FROM inquiry_appointment_employees iae
                  JOIN employees e ON e.id = iae.employee_id
                  WHERE iae.appointment_id = ia.id
                  ORDER BY e.first_name),
            'zusatztermin'::text
        FROM inquiry_appointments ia
        JOIN inquiries i ON i.id = ia.inquiry_id
        LEFT JOIN customers c ON c.id = i.customer_id
        LEFT JOIN addresses a ON a.id = ia.address_id
        WHERE ia.scheduled_date = $1
          AND ia.status IS DISTINCT FROM 'cancelled'
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        "#,
    )
    .bind(day)
    .fetch_all(pool)
    .await?;

    let mut out: Vec<BriefingAppointment> = cal_items
        .into_iter()
        .chain(jobs)
        .chain(extras)
        .map(
            |(id, title, category, scheduled_date, start_time, location, destination, volume_m3, crew, kind)| {
                BriefingAppointment {
                    id,
                    title,
                    category,
                    scheduled_date,
                    start_time,
                    location,
                    destination,
                    volume_m3,
                    crew,
                    kind,
                }
            },
        )
        .collect();
    // Timed first, in order; untimed last.
    out.sort_by_key(|a| (a.start_time.is_none(), a.start_time));
    Ok(out)
}

/// Today in Europe/Berlin — the server runs in UTC, and between 00:00 and 02:00
/// Berlin time the UTC date is still yesterday.
fn berlin_today() -> NaiveDate {
    Utc::now().with_timezone(&Berlin).date_naive()
}

/// Assemble the briefing for today (Europe/Berlin) from live DB data.
pub async fn assemble(pool: &PgPool) -> Result<Briefing> {
    assemble_for(pool, berlin_today()).await
}

/// Assemble the briefing as of `today`.
pub async fn assemble_for(pool: &PgPool, today: NaiveDate) -> Result<Briefing> {
    let mut b = Briefing {
        briefing_date: today,
        todays_appointments: appointments_on(pool, today).await?,
        tomorrows_appointments: appointments_on(pool, today + chrono::Duration::days(1)).await?,
        ..Default::default()
    };

    // Open invoices; overdue = past due_date, or DEFAULT_PAYMENT_DAYS after sending.
    let open: Vec<(uuid::Uuid, String, Option<String>, Option<NaiveDate>)> = sqlx::query_as(
        r#"
        SELECT v.id, v.invoice_number,
               COALESCE(NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''), c.name),
               COALESCE(v.due_date, (v.sent_at AT TIME ZONE 'Europe/Berlin')::date + $1::int)
        FROM invoices v
        LEFT JOIN inquiries i ON i.id = v.inquiry_id
        LEFT JOIN customers c ON c.id = COALESCE(v.customer_id, i.customer_id)
        WHERE v.status = 'sent'
        ORDER BY 4 ASC NULLS LAST
        "#,
    )
    .bind(DEFAULT_PAYMENT_DAYS as i32)
    .fetch_all(pool)
    .await?;
    b.open_invoice_count = open.len() as i64;
    b.overdue_invoices = open
        .into_iter()
        .filter_map(|(id, invoice_number, customer_name, due)| {
            let due_date = due?;
            (due_date < today).then_some(BriefingInvoice {
                id,
                invoice_number,
                customer_name,
                due_date,
            })
        })
        .collect();

    // Work done, no invoice yet.
    let uninvoiced: Vec<(uuid::Uuid, String, NaiveDate)> = sqlx::query_as(
        r#"
        SELECT i.id,
               COALESCE(NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                        c.name, c.email, 'Unbekannt'),
               COALESCE(i.end_date, i.scheduled_date)
        FROM inquiries i
        JOIN customers c ON c.id = i.customer_id
        WHERE i.status IN ('accepted', 'scheduled', 'completed')
          AND i.scheduled_date IS NOT NULL
          AND COALESCE(i.end_date, i.scheduled_date) < $1
          AND COALESCE(i.end_date, i.scheduled_date) >= $1 - $2::int
          AND NOT EXISTS (SELECT 1 FROM invoices v WHERE v.inquiry_id = i.id)
          -- Invoices from the Excel import hang off placeholder inquiries, not the
          -- job's own: any invoice for the same customer from a month before the
          -- job onwards counts as "invoiced".
          AND NOT EXISTS (
              SELECT 1 FROM invoices v
              LEFT JOIN inquiries vi ON vi.id = v.inquiry_id
              WHERE COALESCE(v.customer_id, vi.customer_id) = i.customer_id
                AND COALESCE(v.service_end, v.service_start, v.created_at::date)
                    >= i.scheduled_date - 30
          )
        ORDER BY 3 ASC
        "#,
    )
    .bind(today)
    .bind(UNINVOICED_DAYS as i32)
    .fetch_all(pool)
    .await?;
    b.uninvoiced_jobs = uninvoiced
        .into_iter()
        .map(|(inquiry_id, customer_name, last_day)| BriefingUninvoicedJob {
            inquiry_id,
            customer_name,
            last_day,
        })
        .collect();

    // Pipeline.
    b.new_inquiries = sqlx::query_scalar(
        r#"
        SELECT COALESCE(NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                        c.name, c.email, 'Unbekannt')
        FROM inquiries i
        JOIN customers c ON c.id = i.customer_id
        WHERE i.created_at >= NOW() - INTERVAL '24 hours'
        ORDER BY i.created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let (without_offer, not_sent, awaiting, oldest): (i64, i64, i64, Option<i32>) = sqlx::query_as(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE status IN ('pending', 'info_requested', 'estimating', 'estimated')
                             AND created_at >= NOW() - make_interval(days => $1)),
            COUNT(*) FILTER (WHERE status = 'offer_ready'
                             AND updated_at >= NOW() - make_interval(days => $1)),
            COUNT(*) FILTER (WHERE status = 'offer_sent'
                             AND COALESCE(offer_sent_at, updated_at) >= NOW() - make_interval(days => $1)),
            (MAX(CURRENT_DATE - COALESCE(offer_sent_at, updated_at)::date)
                FILTER (WHERE status = 'offer_sent'
                        AND COALESCE(offer_sent_at, updated_at) >= NOW() - make_interval(days => $1)))::int
        FROM inquiries
        "#,
    )
    .bind(PIPELINE_DAYS as i32)
    .fetch_one(pool)
    .await?;
    b.inquiries_without_offer = without_offer;
    b.offers_not_sent = not_sent;
    b.offers_awaiting_reply = awaiting;
    b.oldest_offer_wait_days = oldest.map(i64::from);

    // Same definition as the email nag in hooks/reminders.rs.
    b.unhandled_emails = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM email_messages m
        JOIN email_threads t ON t.id = m.thread_id
        WHERE m.direction = 'inbound' AND m.handled_at IS NULL AND NOT t.muted
        "#,
    )
    .fetch_one(pool)
    .await?;

    // Worker days in the past week with nothing recorded — neither by the worker
    // (employee_clock_out) nor by the office (clock_in/out, actual_hours).
    let missing: Vec<(String, String, NaiveDate)> = sqlx::query_as(
        r#"
        SELECT e.first_name || ' ' || e.last_name, job, job_date FROM (
            SELECT ie.employee_id,
                   COALESCE(NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                            c.name, 'Auftrag') AS job,
                   ie.job_date
            FROM inquiry_employees ie
            JOIN inquiries i ON i.id = ie.inquiry_id
            JOIN customers c ON c.id = i.customer_id
            WHERE ie.job_date < $1 AND ie.job_date >= $1 - $2::int
              AND i.status NOT IN ('cancelled', 'rejected', 'expired')
              AND ie.clock_in IS NULL AND ie.clock_out IS NULL
              AND ie.actual_hours IS NULL AND ie.employee_clock_out IS NULL
            UNION ALL
            SELECT cie.employee_id, ci.title, cie.job_date
            FROM calendar_item_employees cie
            JOIN calendar_items ci ON ci.id = cie.calendar_item_id
            WHERE cie.job_date < $1 AND cie.job_date >= $1 - $2::int
              AND ci.status IS DISTINCT FROM 'cancelled'
              AND cie.clock_in IS NULL AND cie.clock_out IS NULL
              AND cie.actual_hours IS NULL AND cie.employee_clock_out IS NULL
            UNION ALL
            SELECT iae.employee_id, INITCAP(ia.kind), ia.scheduled_date
            FROM inquiry_appointment_employees iae
            JOIN inquiry_appointments ia ON ia.id = iae.appointment_id
            WHERE ia.scheduled_date < $1 AND ia.scheduled_date >= $1 - $2::int
              AND ia.status IS DISTINCT FROM 'cancelled'
              AND iae.clock_in IS NULL AND iae.clock_out IS NULL
              AND iae.actual_hours IS NULL AND iae.employee_clock_out IS NULL
        ) w
        JOIN employees e ON e.id = w.employee_id
        ORDER BY job_date ASC, 1
        "#,
    )
    .bind(today)
    .bind(MISSING_HOURS_DAYS as i32)
    .fetch_all(pool)
    .await?;
    b.missing_hours = missing
        .into_iter()
        .map(|(employee, job, job_date)| BriefingMissingHours { employee, job, job_date })
        .collect();

    let vehicles: Vec<(String, String, NaiveDate)> = sqlx::query_as(
        r#"
        SELECT COALESCE(NULLIF(v.label, ''), v.kennzeichen, 'Fahrzeug'), r.label, r.due_date
        FROM vehicle_reminders r
        JOIN vehicles v ON v.id = r.vehicle_id
        WHERE r.active AND r.completed_at IS NULL
          AND r.due_date <= $1 + $2::int
        ORDER BY r.due_date ASC
        "#,
    )
    .bind(today)
    .bind(VEHICLE_LOOKAHEAD_DAYS as i32)
    .fetch_all(pool)
    .await?;
    b.vehicle_deadlines = vehicles
        .into_iter()
        .map(|(vehicle, label, due_date)| BriefingVehicleDeadline { vehicle, label, due_date })
        .collect();

    Ok(b)
}

// ── Scheduled delivery ────────────────────────────────────────────────────────

use aust_core::notifications::NotificationKind;
use chrono::Timelike;
use chrono_tz::Europe::Berlin;
use tracing::{info, warn};

use crate::events::notifier::{notify, TelegramNotifier};

/// The fixed daily slots at which the briefing is auto-posted, as
/// `(Europe/Berlin hour, slot key)`. The slot key is persisted in
/// `agent_briefing_log` and selects the greeting. Requested by Alex: 07:00 and
/// 15:00 (feedback report 68ff999e).
const BRIEFING_SLOTS: &[(u32, &str)] = &[(7, "morning"), (15, "afternoon")];

/// The evening preview slot (Europe/Berlin hour, slot key).
const EVENING_SLOT: (u32, &str) = (21, "evening");

/// How many hours after a slot's start we may still deliver it (catch-up after
/// downtime). Kept below the gap between slots so a missed briefing can never
/// bleed into the next window.
const CATCHUP_HOURS: u32 = 3;

/// Greeting prefix for each slot key.
fn slot_greeting(slot: &str) -> &'static str {
    match slot {
        "afternoon" => "🌤️ Nachmittags-Update:",
        _ => "☀️ Guten Morgen!",
    }
}

/// The owner's chat, if one is bound.
async fn owner_chat(pool: &PgPool) -> Result<Option<i64>> {
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(owner.map(|(c,)| c))
}

/// Claim `(date, slot)` in `agent_briefing_log`. Only the tick that wins the
/// insert may send, so restarts and overlapping ticks never double-post.
async fn claim_slot(pool: &PgPool, date: NaiveDate, slot: &str, chat: i64) -> Result<bool> {
    let claimed: Option<(NaiveDate,)> = sqlx::query_as(
        "INSERT INTO agent_briefing_log (slot_date, slot, chat_id) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id, slot_date, slot) DO NOTHING RETURNING slot_date",
    )
    .bind(date)
    .bind(slot)
    .bind(chat)
    .fetch_optional(pool)
    .await?;
    Ok(claimed.is_some())
}

/// Release a claim after a failed send so the next tick retries.
async fn release_slot(pool: &PgPool, date: NaiveDate, slot: &str) {
    let _ = sqlx::query("DELETE FROM agent_briefing_log WHERE slot_date = $1 AND slot = $2")
        .bind(date)
        .bind(slot)
        .execute(pool)
        .await;
}

/// Run one briefing tick: if a fixed daily slot is currently due and hasn't been
/// delivered today, assemble the briefing and post it to the owner chat.
///
/// Driven every 60s by a loop in `src/main.rs`. A muted briefing still claims its
/// slot (so the tick doesn't re-check all window long) but posts nothing.
pub async fn run_briefing_tick(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    let Some(owner_chat) = owner_chat(pool).await? else {
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

    if !claim_slot(pool, today, slot, owner_chat).await? {
        return Ok(());
    }

    // From here the slot is claimed. Any failure must RELEASE the claim so the
    // next tick retries rather than leaving the slot marked delivered.
    let deliver = async {
        let briefing = assemble_for(pool, today).await?;
        let body = briefing.to_telegram_text_with_greeting(slot_greeting(slot));
        notify(pool, notifier, owner_chat, NotificationKind::Briefing, body).await?;
        Ok::<(), crate::error::AssistantError>(())
    };

    match deliver.await {
        Ok(()) => {
            info!(slot = %slot, date = %today, "Daily briefing done");
            Ok(())
        }
        Err(e) => {
            release_slot(pool, today, slot).await;
            warn!("Daily briefing delivery failed, will retry: {e}");
            Ok(())
        }
    }
}

/// Run one evening-preview tick: at 21:00, post tomorrow's appointments that
/// start before 09:00 — if there are any — so nobody oversleeps an early job.
pub async fn run_evening_preview_tick(pool: &PgPool, notifier: &dyn TelegramNotifier) -> Result<()> {
    let Some(owner_chat) = owner_chat(pool).await? else {
        return Ok(());
    };

    let now_berlin = Utc::now().with_timezone(&Berlin);
    let hour = now_berlin.hour();
    let (slot_hour, slot) = EVENING_SLOT;
    if hour < slot_hour || hour >= slot_hour + CATCHUP_HOURS {
        return Ok(());
    }
    let today = now_berlin.date_naive();
    if !claim_slot(pool, today, slot, owner_chat).await? {
        return Ok(());
    }

    let deliver = async {
        let tomorrow = today + chrono::Duration::days(1);
        let appts = appointments_on(pool, tomorrow).await?;
        if let Some(body) = evening_preview_text(tomorrow, &appts) {
            notify(pool, notifier, owner_chat, NotificationKind::EveningPreview, body).await?;
        }
        Ok::<(), crate::error::AssistantError>(())
    };

    match deliver.await {
        Ok(()) => {
            info!(date = %today, "Evening preview done");
            Ok(())
        }
        Err(e) => {
            release_slot(pool, today, slot).await;
            warn!("Evening preview delivery failed, will retry: {e}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 9).unwrap()
    }

    fn appt(title: &str, start: Option<(u32, u32)>, crew: &[&str]) -> BriefingAppointment {
        BriefingAppointment {
            id: Uuid::now_v7(),
            title: title.to_string(),
            category: "umzug".to_string(),
            scheduled_date: Some(day()),
            start_time: start.and_then(|(h, m)| NaiveTime::from_hms_opt(h, m, 0)),
            location: Some("Hauptstr. 1, Hildesheim".to_string()),
            destination: Some("Bahnhofstr. 5, Hannover".to_string()),
            volume_m3: Some(32.0),
            crew: crew.iter().map(|s| s.to_string()).collect(),
            kind: "auftrag".to_string(),
        }
    }

    fn make_briefing() -> Briefing {
        Briefing {
            briefing_date: day(),
            todays_appointments: vec![
                appt("Umzug Müller", Some((9, 30)), &["Max", "Tim"]),
                appt("Halteverbot Schmidt", None, &[]),
            ],
            open_invoice_count: 3,
            overdue_invoices: vec![BriefingInvoice {
                id: Uuid::now_v7(),
                invoice_number: "2026-12".to_string(),
                customer_name: Some("Langer".to_string()),
                due_date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
            }],
            unhandled_emails: 2,
            ..Default::default()
        }
    }

    #[test]
    fn today_shows_time_route_volume_and_crew() {
        let text = make_briefing().to_telegram_text();
        assert!(text.contains("Dienstag 09.06.2026"));
        assert!(text.contains("09:30 Umzug Müller · 32 m³"));
        assert!(text.contains("Hauptstr. 1, Hildesheim → Bahnhofstr. 5, Hannover"));
        assert!(text.contains("👷 Max, Tim"));
        assert!(text.contains("⚠️ niemand eingeteilt"));
    }

    #[test]
    fn money_section_lists_overdue_with_days() {
        let text = make_briefing().to_telegram_text();
        assert!(text.contains("3 Rechnungen offen, davon 1 überfällig"));
        assert!(text.contains("2026-12 Langer, 8 Tage drüber"));
    }

    #[test]
    fn plain_text_has_no_markdown_asterisks() {
        // The notifier sends without parse_mode; asterisks would show literally.
        assert!(!make_briefing().to_telegram_text().contains('*'));
    }

    #[test]
    fn quiet_day_is_short() {
        let b = Briefing { briefing_date: day(), ..Default::default() };
        let text = b.to_telegram_text();
        assert!(text.contains("Heute keine Termine"));
        assert!(text.contains("Morgen (Mittwoch) frei"));
        assert!(!text.contains("💶"));
        assert!(!text.contains("📋"));
    }

    #[test]
    fn long_lists_are_capped() {
        let mut b = Briefing { briefing_date: day(), ..Default::default() };
        b.uninvoiced_jobs = (0..8)
            .map(|i| BriefingUninvoicedJob {
                inquiry_id: Uuid::now_v7(),
                customer_name: format!("Kunde {i}"),
                last_day: day(),
            })
            .collect();
        let text = b.to_telegram_text();
        assert!(text.contains("Kunde 4"));
        assert!(!text.contains("Kunde 5"));
        assert!(text.contains("… und 3 weitere"));
    }

    #[test]
    fn afternoon_greeting_replaces_morning_header() {
        let text = make_briefing().to_telegram_text_with_greeting(slot_greeting("afternoon"));
        assert!(text.contains("Nachmittags-Update"));
        assert!(!text.contains("Guten Morgen"));
        assert!(text.contains("Umzug Müller"));
    }

    #[test]
    fn evening_preview_only_lists_early_starts() {
        let appts = vec![
            appt("Umzug Früh", Some((7, 0)), &["Max"]),
            appt("Umzug Spät", Some((10, 0)), &["Tim"]),
            appt("Ohne Uhrzeit", None, &[]),
        ];
        let text = evening_preview_text(day(), &appts).unwrap();
        assert!(text.contains("07:00 Umzug Früh"));
        assert!(!text.contains("Umzug Spät"));
        assert!(!text.contains("Ohne Uhrzeit"));
    }

    #[test]
    fn evening_preview_is_silent_without_early_starts() {
        let appts = vec![appt("Umzug", Some((9, 0)), &["Max"])];
        assert!(evening_preview_text(day(), &appts).is_none());
    }

    /// Runs every briefing query against a migrated database (skipped without
    /// `DATABASE_URL`), so a typo'd column fails here, not at 07:00 in prod.
    #[tokio::test]
    async fn assemble_runs_against_the_schema() {
        let Ok(url) = std::env::var("DATABASE_URL") else { return };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else { return };
        let b = assemble(&pool).await.expect("briefing queries");
        let text = b.to_telegram_text();
        assert!(text.chars().count() < 4096, "fits one Telegram message");
    }

    #[test]
    fn morning_slot_keeps_default_greeting() {
        assert_eq!(slot_greeting("morning"), "☀️ Guten Morgen!");
        assert_eq!(slot_greeting("whatever"), "☀️ Guten Morgen!");
    }
}
