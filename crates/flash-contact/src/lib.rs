//! Flash contact — ultra-quick customer callback request backend.
//!
//! Customers enter name, phone, and a preferred time window on the landing page.
//! The backend stores the request, sends an immediate Telegram ping, and
//! schedules a reminder when the requested time window actually begins.

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum FlashContactError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unknown time_preference in database: {0}")]
    UnknownTimePreference(String),
}

pub type Result<T> = std::result::Result<T, FlashContactError>;

// ── Model ───────────────────────────────────────────────────────────────────

/// Preferred callback time window chosen by the customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimePreference {
    #[serde(rename = "any_time")]
    AnyTime,
    #[serde(rename = "08-10")]
    MorningEarly,
    #[serde(rename = "10-12")]
    MorningLate,
    #[serde(rename = "14-16")]
    AfternoonEarly,
    #[serde(rename = "16-18")]
    AfternoonLate,
}

impl TimePreference {
    /// Returns `(start_hour, end_hour)` or `None` for `AnyTime`.
    pub fn window(&self) -> Option<(u32, u32)> {
        match self {
            TimePreference::AnyTime => None,
            TimePreference::MorningEarly => Some((8, 10)),
            TimePreference::MorningLate => Some((10, 12)),
            TimePreference::AfternoonEarly => Some((14, 16)),
            TimePreference::AfternoonLate => Some((16, 18)),
        }
    }

    /// German display label.
    pub fn label(&self) -> &'static str {
        match self {
            TimePreference::AnyTime => "Jederzeit",
            TimePreference::MorningEarly => "08:00 – 10:00",
            TimePreference::MorningLate => "10:00 – 12:00",
            TimePreference::AfternoonEarly => "14:00 – 16:00",
            TimePreference::AfternoonLate => "16:00 – 18:00",
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            TimePreference::AnyTime => "any_time",
            TimePreference::MorningEarly => "08-10",
            TimePreference::MorningLate => "10-12",
            TimePreference::AfternoonEarly => "14-16",
            TimePreference::AfternoonLate => "16-18",
        }
    }
}

impl FromStr for TimePreference {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "any_time" => Ok(TimePreference::AnyTime),
            "08-10" => Ok(TimePreference::MorningEarly),
            "10-12" => Ok(TimePreference::MorningLate),
            "14-16" => Ok(TimePreference::AfternoonEarly),
            "16-18" => Ok(TimePreference::AfternoonLate),
            _ => Err(format!("unknown time_preference: {s}")),
        }
    }
}

/// A stored flash contact request.
#[derive(Debug, Clone)]
pub struct FlashContact {
    pub id: Uuid,
    pub name: String,
    pub phone: String,
    pub time_preference: TimePreference,
    pub created_at: DateTime<Utc>,
    pub reminder_sent_at: Option<DateTime<Utc>>,
    pub handled_at: Option<DateTime<Utc>>,
}

/// Input used to create a flash contact.
#[derive(Debug, Deserialize)]
pub struct CreateFlashContact {
    pub name: String,
    pub phone: String,
    pub time_preference: TimePreference,
}

// ── Repository ─────────────────────────────────────────────────────────────

/// Insert a new flash contact.
pub async fn insert(db: &PgPool, input: &CreateFlashContact) -> Result<FlashContact> {
    let id = Uuid::now_v7();
    let now = Utc::now();

    let row = sqlx::query(
        r#"
        INSERT INTO flash_contacts (id, name, phone, time_preference, created_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, name, phone, time_preference, created_at, reminder_sent_at, handled_at
        "#
    )
    .bind(id)
    .bind(&input.name)
    .bind(&input.phone)
    .bind(input.time_preference.as_str())
    .bind(now)
    .fetch_one(db)
    .await?;

    Ok(map_row(row)?)
}

/// Fetch all flash contacts that still need a reminder check.
pub async fn fetch_pending_reminders(db: &PgPool) -> Result<Vec<FlashContact>> {
    let rows = sqlx::query(
        r#"
        SELECT id, name, phone, time_preference, created_at, reminder_sent_at, handled_at
        FROM flash_contacts
        WHERE handled_at IS NULL
          AND reminder_sent_at IS NULL
          AND time_preference != 'any_time'
        ORDER BY created_at ASC
        "#
    )
    .fetch_all(db)
    .await?;

    rows.into_iter().map(map_row).collect()
}

/// Mark reminder as sent.
pub async fn mark_reminder_sent(db: &PgPool, id: Uuid) -> Result<()> {
    let now = Utc::now();
    sqlx::query("UPDATE flash_contacts SET reminder_sent_at = $1 WHERE id = $2")
        .bind(now)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Mark contact as handled (e.g. Alex called back).
pub async fn mark_handled(db: &PgPool, id: Uuid) -> Result<()> {
    let now = Utc::now();
    sqlx::query("UPDATE flash_contacts SET handled_at = $1 WHERE id = $2")
        .bind(now)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

fn map_row(row: sqlx::postgres::PgRow) -> Result<FlashContact> {
    let time_pref_str: String = row.try_get("time_preference")?;
    let time_preference = time_pref_str
        .parse()
        .map_err(|_| FlashContactError::UnknownTimePreference(time_pref_str))?;

    Ok(FlashContact {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        phone: row.try_get("phone")?,
        time_preference,
        created_at: row.try_get("created_at")?,
        reminder_sent_at: row.try_get("reminder_sent_at")?,
        handled_at: row.try_get("handled_at")?,
    })
}

// ── Time window logic ────────────────────────────────────────────────────────

/// Compute the next reminder DateTime for a flash contact.
///
/// Returns `None` when:
/// - the preference is `AnyTime`
/// - the contact is already handled or reminded
/// - the current time is already inside the selected window
pub fn reminder_time(contact: &FlashContact, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if contact.handled_at.is_some() || contact.reminder_sent_at.is_some() {
        return None;
    }

    let (start_hour, end_hour) = contact.time_preference.window()?;
    // Time windows are wall-clock hours in Alex's timezone (Europe/Berlin),
    // not UTC — DST handling matters.
    let now_local = now.with_timezone(&Berlin);
    let today = now_local.date_naive();
    let start_time = NaiveTime::from_hms_opt(start_hour, 0, 0)?;
    let end_time = NaiveTime::from_hms_opt(end_hour, 0, 0)?;

    let today_start_local = Berlin
        .from_local_datetime(&today.and_time(start_time))
        .single()?;
    let today_start_utc = today_start_local.with_timezone(&Utc);

    if now < today_start_utc {
        return Some(today_start_utc);
    }

    let today_end_local = Berlin
        .from_local_datetime(&today.and_time(end_time))
        .single()?;
    let today_end_utc = today_end_local.with_timezone(&Utc);

    if now <= today_end_utc {
        // Inside the window — immediate notification was enough.
        return None;
    }

    // Window already closed for today → next occurrence is tomorrow at start hour.
    let tomorrow = today.succ_opt()?;
    let tomorrow_start_local = Berlin
        .from_local_datetime(&tomorrow.and_time(start_time))
        .single()?;
    Some(tomorrow_start_local.with_timezone(&Utc))
}

// ── Telegram formatting ────────────────────────────────────────────────────

/// Plain-text message for the immediate Telegram notification.
pub fn format_immediate_message(contact: &FlashContact) -> String {
    format!(
        "⚡ FLASH-Kontakt\n\n\
         Name: {}\n\
         Telefon: {}\n\
         Rückruf: {}\n\n\
         Bitte so schnell wie möglich zurückrufen!",
        contact.name, contact.phone, contact.time_preference.label()
    )
}

/// Plain-text message for the delayed time-window reminder.
pub fn format_reminder_message(contact: &FlashContact) -> String {
    format!(
        "⏰ FLASH-Erinnerung\n\n\
         Name: {}\n\
         Telefon: {}\n\
         Rückrufzeit: {} — jetzt ist die Zeit!\n\n\
         Bitte anrufen!",
        contact.name, contact.phone, contact.time_preference.label()
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn make_contact(preference: TimePreference) -> FlashContact {
        FlashContact {
            id: Uuid::nil(),
            name: "Test".into(),
            phone: "123".into(),
            time_preference: preference,
            created_at: Utc::now(),
            reminder_sent_at: None,
            handled_at: None,
        }
    }

    /// Build a UTC DateTime from a Europe/Berlin wall-clock time.
    fn berlin(d: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
        Berlin
            .from_local_datetime(&d.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap()))
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn reminder_before_window_starts_today() {
        let c = make_contact(TimePreference::MorningEarly);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let now = berlin(day, 6, 0);
        let rt = reminder_time(&c, now).expect("should remind today");
        assert_eq!(rt, berlin(day, 8, 0));
    }

    #[test]
    fn reminder_inside_window_returns_none() {
        let c = make_contact(TimePreference::MorningEarly);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        assert!(reminder_time(&c, berlin(day, 9, 0)).is_none());
    }

    #[test]
    fn reminder_after_window_rolls_to_tomorrow() {
        let c = make_contact(TimePreference::MorningEarly);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let tomorrow = day.succ_opt().unwrap();
        let rt = reminder_time(&c, berlin(day, 11, 0)).expect("should remind tomorrow");
        assert_eq!(rt, berlin(tomorrow, 8, 0));
    }

    #[test]
    fn reminder_any_time_is_none() {
        let c = make_contact(TimePreference::AnyTime);
        assert!(reminder_time(&c, Utc::now()).is_none());
    }

    #[test]
    fn reminder_handled_is_none() {
        let mut c = make_contact(TimePreference::MorningEarly);
        c.handled_at = Some(Utc::now());
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        assert!(reminder_time(&c, berlin(day, 6, 0)).is_none());
    }
}
