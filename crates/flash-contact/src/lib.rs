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

/// Preferred callback time chosen by the customer — matches the UI labels exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimePreference {
    /// Call as soon as possible — no scheduled reminder.
    #[serde(rename = "gleich")]
    Gleich,
    /// Morning slot — reminder fires at 09:00 Europe/Berlin.
    #[serde(rename = "vormittag")]
    Vormittag,
    /// Afternoon slot — reminder fires at 13:00 Europe/Berlin.
    #[serde(rename = "nachmittag")]
    Nachmittag,
}

impl TimePreference {
    /// Returns the reminder hour in Europe/Berlin, or `None` for `Gleich`.
    pub fn reminder_hour(&self) -> Option<u32> {
        match self {
            TimePreference::Gleich => None,
            TimePreference::Vormittag => Some(9),
            TimePreference::Nachmittag => Some(13),
        }
    }

    /// German display label shown in Telegram.
    pub fn label(&self) -> &'static str {
        match self {
            TimePreference::Gleich => "Jetzt gleich",
            TimePreference::Vormittag => "Vormittag",
            TimePreference::Nachmittag => "Nachmittag",
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            TimePreference::Gleich => "gleich",
            TimePreference::Vormittag => "vormittag",
            TimePreference::Nachmittag => "nachmittag",
        }
    }
}

impl FromStr for TimePreference {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "gleich" => Ok(TimePreference::Gleich),
            "vormittag" => Ok(TimePreference::Vormittag),
            "nachmittag" => Ok(TimePreference::Nachmittag),
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
    /// Overrides the default reminder schedule when set by the bot after "Nochmal erinnern".
    pub next_remind_at: Option<DateTime<Utc>>,
    /// Set when Alex taps "Verwerfen" — contact is closed without a successful callback.
    pub dismissed_at: Option<DateTime<Utc>>,
}

/// Input used to create a flash contact.
#[derive(Debug, Deserialize)]
pub struct CreateFlashContact {
    pub name: String,
    pub phone: String,
    pub time_preference: TimePreference,
}

// ── Repository ─────────────────────────────────────────────────────────────

const SELECT_COLS: &str =
    "id, name, phone, time_preference, created_at, reminder_sent_at, handled_at, next_remind_at, dismissed_at";

/// Insert a new flash contact.
pub async fn insert(db: &PgPool, input: &CreateFlashContact) -> Result<FlashContact> {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let sql = format!(
        "INSERT INTO flash_contacts (id, name, phone, time_preference, created_at)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING {SELECT_COLS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(&input.name)
        .bind(&input.phone)
        .bind(input.time_preference.as_str())
        .bind(now)
        .fetch_one(db)
        .await?;
    Ok(map_row(row)?)
}

/// Fetch all contacts that need a reminder right now.
///
/// Two cases:
/// 1. First-time reminder: `reminder_sent_at IS NULL`, time_preference != 'gleich'
/// 2. Snoozed reminder: `next_remind_at` is set and has arrived
pub async fn fetch_pending_reminders(db: &PgPool) -> Result<Vec<FlashContact>> {
    let now = Utc::now();
    let sql = format!(
        "SELECT {SELECT_COLS} FROM flash_contacts
         WHERE handled_at IS NULL AND dismissed_at IS NULL
           AND (
             (reminder_sent_at IS NULL AND next_remind_at IS NULL AND time_preference != 'gleich')
             OR (next_remind_at IS NOT NULL AND next_remind_at <= $1)
           )
         ORDER BY created_at ASC"
    );
    let rows = sqlx::query(&sql).bind(now).fetch_all(db).await?;
    rows.into_iter().map(map_row).collect()
}

/// Mark the initial reminder as sent (clears next_remind_at).
pub async fn mark_reminder_sent(db: &PgPool, id: Uuid) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE flash_contacts SET reminder_sent_at = $1, next_remind_at = NULL WHERE id = $2",
    )
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark contact as handled — Alex successfully reached the customer.
/// Idempotent: re-clicking the button leaves the original `handled_at` intact.
pub async fn mark_handled(db: &PgPool, id: Uuid) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE flash_contacts SET handled_at = $1, next_remind_at = NULL \
         WHERE id = $2 AND handled_at IS NULL",
    )
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Mark contact as dismissed — Alex gives up without reaching the customer.
pub async fn mark_dismissed(db: &PgPool, id: Uuid) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE flash_contacts SET dismissed_at = $1, next_remind_at = NULL \
         WHERE id = $2 AND dismissed_at IS NULL",
    )
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Schedule the next snooze reminder and clear the sent flag so the cron fires again.
pub async fn schedule_snooze(db: &PgPool, id: Uuid, next_remind_at: DateTime<Utc>) -> Result<()> {
    sqlx::query(
        "UPDATE flash_contacts SET next_remind_at = $1, reminder_sent_at = NULL WHERE id = $2",
    )
    .bind(next_remind_at)
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
        next_remind_at: row.try_get("next_remind_at")?,
        dismissed_at: row.try_get("dismissed_at")?,
    })
}

// ── Snooze schedule ──────────────────────────────────────────────────────────

/// Compute the next snooze timestamp after Alex taps "Nochmal erinnern".
///
/// Escalation sequences (wall-clock hours in Europe/Berlin):
/// - Vormittag: 08 → 11 → 08(+1d) → 11(+1d) → …
/// - Nachmittag: 13 → 16 → 13(+1d) → 16(+1d) → …
///
/// Picks the next slot strictly after `now`.
pub fn next_snooze(pref: TimePreference, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let slots: &[u32] = match pref {
        TimePreference::Gleich => return None,
        TimePreference::Vormittag => &[8, 11],
        TimePreference::Nachmittag => &[13, 16],
    };
    let now_local = now.with_timezone(&Berlin);
    let today = now_local.date_naive();

    // Try each slot today, then tomorrow.
    for day_offset in 0u64..=1 {
        let day = today + chrono::Duration::days(day_offset as i64);
        for &hour in slots {
            let t = NaiveTime::from_hms_opt(hour, 0, 0)?;
            let local = Berlin.from_local_datetime(&day.and_time(t)).single()?;
            let utc = local.with_timezone(&Utc);
            if utc > now {
                return Some(utc);
            }
        }
    }
    // Fallback: 08:00 two days from now (shouldn't normally be reached).
    let fallback_day = today + chrono::Duration::days(2);
    let t = NaiveTime::from_hms_opt(slots[0], 0, 0)?;
    let local = Berlin.from_local_datetime(&fallback_day.and_time(t)).single()?;
    Some(local.with_timezone(&Utc))
}

// ── Time window logic ────────────────────────────────────────────────────────

/// Compute the next reminder DateTime for a flash contact.
///
/// Returns `None` when:
/// - the preference is `Gleich` (call immediately, no scheduled reminder)
/// - the contact is already handled or already reminded
///
/// Otherwise returns the next wall-clock reminder time in Europe/Berlin:
/// - if the reminder hour hasn't passed today → today at that hour
/// - if it has already passed today → tomorrow at that hour
///
/// When we are past the reminder hour but the cron hasn't fired yet,
/// `Some(today_remind_utc)` is still returned so that `now >= remind_at`
/// is true and the reminder fires on the next cron tick.
pub fn reminder_time(contact: &FlashContact, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if contact.handled_at.is_some() || contact.dismissed_at.is_some() || contact.reminder_sent_at.is_some() {
        return None;
    }

    // Snoozed contact: use the explicit next_remind_at timestamp.
    if let Some(next) = contact.next_remind_at {
        return Some(next);
    }

    // Reminder hour is a wall-clock hour in Europe/Berlin (DST-aware).
    let remind_hour = contact.time_preference.reminder_hour()?;
    let now_local = now.with_timezone(&Berlin);
    let today = now_local.date_naive();
    let remind_time = NaiveTime::from_hms_opt(remind_hour, 0, 0)?;

    let today_remind_local = Berlin
        .from_local_datetime(&today.and_time(remind_time))
        .single()?;
    let today_remind_utc = today_remind_local.with_timezone(&Utc);

    if now < today_remind_utc {
        // Reminder hour hasn't arrived yet today.
        return Some(today_remind_utc);
    }

    // We are at or past today's reminder hour — either fire now (cron missed the
    // exact tick) or roll to tomorrow if the reminder was already sent.
    // Since `reminder_sent_at` is checked above, reaching here means it hasn't
    // been sent yet, so return today's time so `now >= remind_at` fires.
    Some(today_remind_utc)
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
            next_remind_at: None,
            dismissed_at: None,
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
    fn reminder_before_hour_returns_that_hour_today() {
        // Vormittag reminds at 09:00. At 07:00 → remind_at = 09:00 today.
        let c = make_contact(TimePreference::Vormittag);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let rt = reminder_time(&c, berlin(day, 7, 0)).expect("should schedule reminder");
        assert_eq!(rt, berlin(day, 9, 0));
    }

    #[test]
    fn reminder_at_or_after_hour_fires_immediately() {
        // At 09:30 the cron tick may have missed 09:00 — still returns today's remind_at
        // so `now >= remind_at` is true and the reminder fires on the next tick.
        let c = make_contact(TimePreference::Vormittag);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let rt = reminder_time(&c, berlin(day, 9, 30)).expect("should still remind");
        assert_eq!(rt, berlin(day, 9, 0));
    }

    #[test]
    fn reminder_nachmittag_before_hour() {
        // Nachmittag reminds at 13:00.
        let c = make_contact(TimePreference::Nachmittag);
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let rt = reminder_time(&c, berlin(day, 11, 0)).expect("should schedule reminder");
        assert_eq!(rt, berlin(day, 13, 0));
    }

    #[test]
    fn reminder_gleich_is_none() {
        let c = make_contact(TimePreference::Gleich);
        assert!(reminder_time(&c, Utc::now()).is_none());
    }

    #[test]
    fn reminder_handled_is_none() {
        let mut c = make_contact(TimePreference::Vormittag);
        c.handled_at = Some(Utc::now());
        let day = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        assert!(reminder_time(&c, berlin(day, 7, 0)).is_none());
    }
}
