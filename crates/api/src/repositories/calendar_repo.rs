//! Calendar repository — centralised queries for calendar, inquiry_days, and calendar_item_days.

use chrono::NaiveDate;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ── Row types ────────────────────────────────────────────────────────────────

/// Schedule inquiry row — full projection used by `get_schedule`.
#[derive(Debug, FromRow)]
pub(crate) struct ScheduleInquiryRow {
    pub effective_date: NaiveDate,
    pub inquiry_id: Uuid,
    pub customer_name: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
    pub status: String,
    pub notes: Option<String>,
    pub start_time: chrono::NaiveTime,
    pub end_time: chrono::NaiveTime,
    pub employees_assigned: i64,
    pub employee_names: Option<String>,
    pub day_number: Option<i16>,
    pub total_days: Option<i16>,
    pub day_notes: Option<String>,
}

// ── Queries ──────────────────────────────────────────────────────────────────

/// Count active (non-cancelled/rejected/expired) inquiries on a given date.
///
/// **Caller**: `calendar::count_active_on_date`
/// **Why**: Used to compute availability for a date.
pub(crate) async fn count_active_on_date(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<i64, sqlx::Error> {
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
    Ok(count)
}

/// Fetch capacity override for a specific date.
///
/// **Caller**: `calendar::effective_capacity`
/// **Why**: Returns the custom capacity if one exists.
pub(crate) async fn fetch_capacity_override(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<Option<i32>, sqlx::Error> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT capacity FROM calendar_capacity_overrides WHERE override_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(c,)| c))
}

/// Fetch all schedule inquiries (single-day + multi-day) in a date range.
///
/// **Caller**: `calendar::get_schedule`
/// **Why**: Returns one row per inquiry-day for schedule display.
pub(crate) async fn fetch_schedule_inquiries(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleInquiryRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        -- Single-day branch: inquiry has no rows in inquiry_days
        SELECT
            COALESCE(i.scheduled_date, i.preferred_date::date) AS effective_date,
            i.id AS inquiry_id,
            COALESCE(
                NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                c.name, c.email
            ) AS customer_name,
            CASE WHEN ao.id IS NOT NULL THEN ao.street || ', ' || ao.city END AS departure_address,
            CASE WHEN ad.id IS NOT NULL THEN ad.street || ', ' || ad.city END AS arrival_address,
            i.estimated_volume_m3 AS volume_m3,
            i.status,
            i.notes,
            i.start_time,
            i.end_time,
            COUNT(ie.id) AS employees_assigned,
            NULLIF(STRING_AGG(
                e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', ' ORDER BY e.last_name, e.first_name
            ), '') AS employee_names,
            NULL::smallint AS day_number,
            NULL::smallint AS total_days,
            NULL::text AS day_notes
        FROM inquiries i
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses ao ON i.origin_address_id = ao.id
        LEFT JOIN addresses ad ON i.destination_address_id = ad.id
        LEFT JOIN inquiry_employees ie ON ie.inquiry_id = i.id
        LEFT JOIN employees e ON ie.employee_id = e.id
        WHERE NOT EXISTS (SELECT 1 FROM inquiry_days WHERE inquiry_id = i.id)
          AND COALESCE(i.scheduled_date, i.preferred_date::date) BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY i.id, c.id, ao.id, ad.id

        UNION ALL

        -- Multi-day branch: one row per day from inquiry_days
        SELECT
            id2.day_date AS effective_date,
            i.id AS inquiry_id,
            COALESCE(
                NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                c.name, c.email
            ) AS customer_name,
            CASE WHEN ao.id IS NOT NULL THEN ao.street || ', ' || ao.city END AS departure_address,
            CASE WHEN ad.id IS NOT NULL THEN ad.street || ', ' || ad.city END AS arrival_address,
            i.estimated_volume_m3 AS volume_m3,
            i.status,
            i.notes,
            i.start_time,
            i.end_time,
            COUNT(ie.id) AS employees_assigned,
            NULLIF(STRING_AGG(
                e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', ' ORDER BY e.last_name, e.first_name
            ), '') AS employee_names,
            id2.day_number,
            total.total_days,
            id2.notes AS day_notes
        FROM inquiries i
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses ao ON i.origin_address_id = ao.id
        LEFT JOIN addresses ad ON i.destination_address_id = ad.id
        JOIN inquiry_days id2 ON id2.inquiry_id = i.id
        JOIN (
            SELECT inquiry_id, COUNT(*)::smallint AS total_days
            FROM inquiry_days
            GROUP BY inquiry_id
        ) total ON total.inquiry_id = i.id
        LEFT JOIN inquiry_employees ie ON ie.inquiry_id = i.id
        LEFT JOIN employees e ON ie.employee_id = e.id
        WHERE id2.day_date BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY i.id, c.id, ao.id, ad.id, id2.day_date, id2.day_number, id2.notes, total.total_days

        ORDER BY effective_date
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

/// Fetch offer prices for a set of inquiry IDs.
///
/// **Caller**: `calendar::get_schedule`
/// **Why**: Builds a price map for schedule display.
pub(crate) async fn fetch_offer_prices(
    pool: &PgPool,
    inquiry_ids: &[Uuid],
) -> Result<Vec<(Uuid, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT inquiry_id, price_cents FROM offers WHERE inquiry_id = ANY($1) AND status != 'rejected' ORDER BY created_at DESC",
    )
    .bind(inquiry_ids)
    .fetch_all(pool)
    .await
}

/// Fetch capacity overrides for a date range.
///
/// **Caller**: `calendar::get_schedule`
/// **Why**: Pre-loads overrides to avoid per-day queries.
pub(crate) async fn fetch_capacity_overrides_range(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<(NaiveDate, i32)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT override_date, capacity FROM calendar_capacity_overrides WHERE override_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

/// Upsert a capacity override for a specific date.
///
/// **Caller**: `calendar::set_capacity`
/// **Why**: Creates or updates the capacity override.
pub(crate) async fn upsert_capacity(
    pool: &PgPool,
    date: NaiveDate,
    capacity: i32,
) -> Result<(), sqlx::Error> {
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

/// Fetch inquiry_days for an inquiry ordered by day_number.
///
/// **Caller**: `calendar::get_inquiry_days`
/// **Why**: Returns the multi-day schedule for an inquiry.
pub(crate) async fn fetch_inquiry_days(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<(NaiveDate, i16, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT day_date, day_number, notes FROM inquiry_days WHERE inquiry_id = $1 ORDER BY day_number",
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Delete all inquiry_days for an inquiry (within a transaction).
///
/// **Caller**: `calendar::put_inquiry_days`
/// **Why**: Full-replace semantics — delete all before re-inserting.
pub(crate) async fn delete_inquiry_days(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM inquiry_days WHERE inquiry_id = $1")
        .bind(inquiry_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Insert a single inquiry_day (within a transaction).
///
/// **Caller**: `calendar::put_inquiry_days`
/// **Why**: Inserts one day in the multi-day schedule.
pub(crate) async fn insert_inquiry_day(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inquiry_id: Uuid,
    day_date: NaiveDate,
    day_number: i16,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inquiry_days (inquiry_id, day_date, day_number, notes) VALUES ($1, $2, $3, $4)",
    )
    .bind(inquiry_id)
    .bind(day_date)
    .bind(day_number)
    .bind(notes)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Fetch calendar_item_days for a calendar item ordered by day_number.
///
/// **Caller**: `calendar::get_calendar_item_days`
/// **Why**: Returns the multi-day schedule for a calendar item (Termin).
pub(crate) async fn fetch_calendar_item_days(
    pool: &PgPool,
    calendar_item_id: Uuid,
) -> Result<Vec<(NaiveDate, i16, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT day_date, day_number, notes FROM calendar_item_days WHERE calendar_item_id = $1 ORDER BY day_number",
    )
    .bind(calendar_item_id)
    .fetch_all(pool)
    .await
}

/// Delete all calendar_item_days for a calendar item (within a transaction).
///
/// **Caller**: `calendar::put_calendar_item_days`
/// **Why**: Full-replace semantics.
pub(crate) async fn delete_calendar_item_days(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_item_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM calendar_item_days WHERE calendar_item_id = $1")
        .bind(calendar_item_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Insert a single calendar_item_day (within a transaction).
///
/// **Caller**: `calendar::put_calendar_item_days`
/// **Why**: Inserts one day in the multi-day Termin schedule.
pub(crate) async fn insert_calendar_item_day(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_item_id: Uuid,
    day_date: NaiveDate,
    day_number: i16,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO calendar_item_days (calendar_item_id, day_date, day_number, notes) VALUES ($1, $2, $3, $4)",
    )
    .bind(calendar_item_id)
    .bind(day_date)
    .bind(day_number)
    .bind(notes)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
