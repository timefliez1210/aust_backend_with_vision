//! Inline calendar availability logic for the email agent.
//!
//! Replaces the former `aust-calendar` crate dependency. Queries `inquiries` and
//! `calendar_capacity_overrides` directly — no separate `calendar_bookings` table.

use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Availability snapshot for a single calendar date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateAvailability {
    pub date: NaiveDate,
    pub available: bool,
    pub capacity: i32,
    pub booked: i32,
    pub remaining: i32,
}

/// Result returned by `check_availability`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityResult {
    pub requested_date: NaiveDate,
    pub requested_date_available: bool,
    pub requested_date_info: DateAvailability,
    /// Nearest available alternatives (only populated when requested date is full).
    pub alternatives: Vec<DateAvailability>,
}

/// One inquiry shown in the Telegram calendar view.
#[derive(Debug, Clone)]
pub struct ScheduleInquiry {
    pub inquiry_id: Uuid,
    pub customer_name: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
}

/// One day in the Telegram schedule view.
#[derive(Debug, Clone)]
pub struct ScheduleEntry {
    pub date: NaiveDate,
    pub availability: DateAvailability,
    pub inquiries: Vec<ScheduleInquiry>,
}

/// Count active inquiries scheduled on `date`.
///
/// "Active" = status not in (cancelled, rejected, expired).
/// Uses `COALESCE(scheduled_date, preferred_date::date)` as the effective date.
async fn count_active(pool: &PgPool, date: NaiveDate) -> Result<i32, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM inquiries
        WHERE COALESCE(scheduled_date, preferred_date::date) = $1
          AND status NOT IN ('cancelled', 'rejected', 'expired')
        "#,
    )
    .bind(date)
    .fetch_one(pool)
    .await?;
    Ok(count as i32)
}

/// Get the effective capacity for `date` (override if one exists, else default).
async fn effective_capacity(
    pool: &PgPool,
    date: NaiveDate,
    default_capacity: i32,
) -> Result<i32, sqlx::Error> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT capacity FROM calendar_capacity_overrides WHERE override_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(c,)| c).unwrap_or(default_capacity))
}

/// Build a `DateAvailability` for a single date.
async fn date_availability(
    pool: &PgPool,
    date: NaiveDate,
    default_capacity: i32,
) -> Result<DateAvailability, sqlx::Error> {
    let capacity = effective_capacity(pool, date, default_capacity).await?;
    let booked = count_active(pool, date).await?;
    let remaining = (capacity - booked).max(0);
    Ok(DateAvailability {
        date,
        available: remaining > 0,
        capacity,
        booked,
        remaining,
    })
}

/// Check availability for a date and suggest alternatives if it is full.
///
/// **Caller**: `EmailProcessor::process_incoming_email` — before drafting a
/// reply so the LLM can mention whether the preferred date is available.
pub async fn check_availability(
    pool: &PgPool,
    date: NaiveDate,
    default_capacity: i32,
    alternatives_count: usize,
    search_window_days: i64,
) -> Result<AvailabilityResult, sqlx::Error> {
    let info = date_availability(pool, date, default_capacity).await?;
    let available = info.available;

    let alternatives = if !available {
        find_nearest_available(pool, date, default_capacity, alternatives_count, search_window_days)
            .await?
    } else {
        Vec::new()
    };

    Ok(AvailabilityResult {
        requested_date: date,
        requested_date_available: available,
        requested_date_info: info,
        alternatives,
    })
}

/// Find up to `count` nearest available dates around `around`, skipping Sundays and past dates.
async fn find_nearest_available(
    pool: &PgPool,
    around: NaiveDate,
    default_capacity: i32,
    count: usize,
    search_window_days: i64,
) -> Result<Vec<DateAvailability>, sqlx::Error> {
    let today = Utc::now().date_naive();
    let mut results = Vec::new();
    let mut offset = 1i64;

    while results.len() < count && offset <= search_window_days {
        let future = around + chrono::Days::new(offset as u64);
        if future.weekday() != chrono::Weekday::Sun {
            let avail = date_availability(pool, future, default_capacity).await?;
            if avail.available {
                results.push(avail);
                if results.len() >= count {
                    break;
                }
            }
        }

        let past = around - chrono::Days::new(offset as u64);
        if past >= today && past.weekday() != chrono::Weekday::Sun {
            let avail = date_availability(pool, past, default_capacity).await?;
            if avail.available {
                results.push(avail);
            }
        }

        offset += 1;
    }

    results.sort_by_key(|a| (a.date - around).num_days().unsigned_abs());
    results.truncate(count);
    Ok(results)
}

/// Fetch the full schedule for a date range for Telegram display.
///
/// Returns one `ScheduleEntry` per day, including days with no inquiries.
/// **Caller**: Telegram `/kalender` and `/termine` commands.
pub async fn get_schedule(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
    default_capacity: i32,
) -> Result<Vec<ScheduleEntry>, sqlx::Error> {
    // Fetch all active inquiries in range
    let rows: Vec<(NaiveDate, Uuid, Option<String>, Option<String>, Option<String>, Option<f64>)> =
        sqlx::query_as(
            r#"
            SELECT
                COALESCE(i.scheduled_date, i.preferred_date::date) AS effective_date,
                i.id,
                COALESCE(
                    NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                    c.name,
                    c.email
                ) AS customer_name,
                CASE WHEN ao.id IS NOT NULL THEN ao.street || ', ' || ao.city END AS departure_address,
                CASE WHEN ad.id IS NOT NULL THEN ad.street || ', ' || ad.city END AS arrival_address,
                i.estimated_volume_m3
            FROM inquiries i
            JOIN customers c ON i.customer_id = c.id
            LEFT JOIN addresses ao ON i.origin_address_id = ao.id
            LEFT JOIN addresses ad ON i.destination_address_id = ad.id
            WHERE COALESCE(i.scheduled_date, i.preferred_date::date) BETWEEN $1 AND $2
              AND i.status NOT IN ('cancelled', 'rejected', 'expired')
            ORDER BY effective_date
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?;

    // Fetch capacity overrides for the range
    let overrides: Vec<(NaiveDate, i32)> = sqlx::query_as(
        "SELECT override_date, capacity FROM calendar_capacity_overrides WHERE override_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;

    use std::collections::HashMap;
    let override_map: HashMap<NaiveDate, i32> = overrides.into_iter().collect();
    let mut inquiry_map: HashMap<NaiveDate, Vec<ScheduleInquiry>> = HashMap::new();
    for (date, id, name, dep, arr, vol) in rows {
        inquiry_map.entry(date).or_default().push(ScheduleInquiry {
            inquiry_id: id,
            customer_name: name,
            departure_address: dep,
            arrival_address: arr,
            volume_m3: vol,
        });
    }

    let mut entries = Vec::new();
    let mut current = from;
    while current <= to {
        let capacity = override_map.get(&current).copied().unwrap_or(default_capacity);
        let day_inquiries = inquiry_map.remove(&current).unwrap_or_default();
        let booked = day_inquiries.len() as i32;
        let remaining = (capacity - booked).max(0);

        entries.push(ScheduleEntry {
            date: current,
            availability: DateAvailability {
                date: current,
                available: remaining > 0,
                capacity,
                booked,
                remaining,
            },
            inquiries: day_inquiries,
        });
        current = current.succ_opt().unwrap();
    }

    Ok(entries)
}

/// Upsert the daily capacity override for `date`.
///
/// **Caller**: Telegram `/kapazitaet YYYY-MM-DD N` command.
pub async fn set_capacity(pool: &PgPool, date: NaiveDate, capacity: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO calendar_capacity_overrides (id, override_date, capacity, created_at)
        VALUES (gen_random_uuid(), $1, $2, NOW())
        ON CONFLICT (override_date) DO UPDATE SET capacity = EXCLUDED.capacity
        "#,
    )
    .bind(date)
    .bind(capacity)
    .execute(pool)
    .await?;
    Ok(())
}
