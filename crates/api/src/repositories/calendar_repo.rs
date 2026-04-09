//! Calendar repository — centralised queries for calendar, inquiry_days, and calendar_item_days.

use chrono::{NaiveDate, NaiveTime};
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

/// A single row from `inquiry_days` including optional per-day times.
#[derive(Debug, FromRow)]
pub(crate) struct InquiryDayRow {
    pub id: Uuid,
    pub day_date: NaiveDate,
    pub day_number: i16,
    pub notes: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
}

/// A single row from `calendar_item_days` including optional per-day times.
#[derive(Debug, FromRow)]
pub(crate) struct CalendarItemDayRow {
    pub id: Uuid,
    pub day_date: NaiveDate,
    pub day_number: i16,
    pub notes: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
}

/// A per-day employee assignment row (used for both inquiry days and calendar item days).
///
/// The `day_id` field holds the UUID of the parent `inquiry_days` or
/// `calendar_item_days` row so callers can group by day.
#[derive(Debug, FromRow)]
pub(crate) struct DayEmployeeRow {
    pub day_id: Uuid,
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub planned_hours: Option<f64>,
    pub notes: Option<String>,
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
        WHERE scheduled_date = $1
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
///
/// Single-day branch uses inquiry-level `start_time`/`end_time` and
/// `inquiry_employees` for the employee count.
///
/// Multi-day branch uses per-day `start_time`/`end_time` (falling back to
/// parent via COALESCE) and `inquiry_day_employees` for the employee count and
/// names, giving per-day staffing visibility in the calendar view.
pub(crate) async fn fetch_schedule_inquiries(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleInquiryRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        -- Single-day branch: inquiry has no rows in inquiry_days
        SELECT
            i.scheduled_date AS effective_date,
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
          AND i.scheduled_date BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY i.id, c.id, ao.id, ad.id

        UNION ALL

        -- Multi-day branch: one row per day from inquiry_days.
        -- Times: per-day if set, else parent inquiry times (COALESCE).
        -- Employees: per-day inquiry_day_employees (not inquiry_employees).
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
            COALESCE(id2.start_time, i.start_time) AS start_time,
            COALESCE(id2.end_time,   i.end_time)   AS end_time,
            COUNT(ide.id) AS employees_assigned,
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
        LEFT JOIN inquiry_day_employees ide ON ide.inquiry_day_id = id2.id
        LEFT JOIN employees e ON ide.employee_id = e.id
        WHERE id2.day_date BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY i.id, c.id, ao.id, ad.id, id2.id, id2.day_date, id2.day_number,
                 id2.notes, id2.start_time, id2.end_time, total.total_days

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

// ── Inquiry days ─────────────────────────────────────────────────────────────

/// Fetch `inquiry_days` for an inquiry ordered by `day_number`.
///
/// **Caller**: `calendar::get_inquiry_days`
/// **Why**: Returns the multi-day schedule (with per-day times) for one inquiry.
pub(crate) async fn fetch_inquiry_days(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<InquiryDayRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, day_date, day_number, notes, start_time, end_time \
         FROM inquiry_days WHERE inquiry_id = $1 ORDER BY day_number",
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Fetch all per-day employee assignments for every day of a given inquiry.
///
/// **Caller**: `calendar::get_inquiry_days`
/// **Why**: Lets the handler merge employees into each `InquiryDayRow` without N+1 queries.
///
/// Returns rows keyed by `day_id` (the `inquiry_days.id`), joined with employee names.
pub(crate) async fn fetch_inquiry_day_employees(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<DayEmployeeRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ide.inquiry_day_id AS day_id,
               ide.employee_id,
               e.first_name,
               e.last_name,
               ide.planned_hours::float8 AS planned_hours,
               ide.notes
        FROM inquiry_day_employees ide
        JOIN inquiry_days id2 ON ide.inquiry_day_id = id2.id
        JOIN employees e ON ide.employee_id = e.id
        WHERE id2.inquiry_id = $1
        ORDER BY id2.day_number, e.last_name, e.first_name
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Delete all `inquiry_days` for an inquiry (within a transaction).
///
/// **Caller**: `calendar::put_inquiry_days`
/// **Why**: Full-replace semantics — delete all before re-inserting.
///          Cascade on the FK also deletes `inquiry_day_employees`.
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

/// Insert a single `inquiry_day` (within a transaction).
///
/// **Caller**: `calendar::put_inquiry_days`
/// **Why**: Inserts one day in the multi-day schedule. Returns the new row's UUID
///          so the caller can insert `inquiry_day_employees` against it.
pub(crate) async fn insert_inquiry_day(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inquiry_id: Uuid,
    day_date: NaiveDate,
    day_number: i16,
    notes: Option<&str>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
) -> Result<Uuid, sqlx::Error> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO inquiry_days (inquiry_id, day_date, day_number, notes, start_time, end_time) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(inquiry_id)
    .bind(day_date)
    .bind(day_number)
    .bind(notes)
    .bind(start_time)
    .bind(end_time)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Insert one per-day employee assignment for an inquiry day (within a transaction).
///
/// **Caller**: `calendar::put_inquiry_days`
/// **Why**: Assigns an employee with optional planned hours to a specific day.
pub(crate) async fn insert_inquiry_day_employee(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inquiry_day_id: Uuid,
    employee_id: Uuid,
    planned_hours: Option<f64>,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inquiry_day_employees (inquiry_day_id, employee_id, planned_hours, notes) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (inquiry_day_id, employee_id) DO NOTHING",
    )
    .bind(inquiry_day_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(notes)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ── Calendar item days ────────────────────────────────────────────────────────

/// Fetch `calendar_item_days` for a calendar item ordered by `day_number`.
///
/// **Caller**: `calendar::get_calendar_item_days`
/// **Why**: Returns the multi-day schedule (with per-day times) for one Termin.
pub(crate) async fn fetch_calendar_item_days(
    pool: &PgPool,
    calendar_item_id: Uuid,
) -> Result<Vec<CalendarItemDayRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, day_date, day_number, notes, start_time, end_time \
         FROM calendar_item_days WHERE calendar_item_id = $1 ORDER BY day_number",
    )
    .bind(calendar_item_id)
    .fetch_all(pool)
    .await
}

/// Fetch all per-day employee assignments for every day of a given calendar item.
///
/// **Caller**: `calendar::get_calendar_item_days`
/// **Why**: Lets the handler merge employees into each `CalendarItemDayRow` without N+1 queries.
pub(crate) async fn fetch_calendar_item_day_employees(
    pool: &PgPool,
    calendar_item_id: Uuid,
) -> Result<Vec<DayEmployeeRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT cide.calendar_item_day_id AS day_id,
               cide.employee_id,
               e.first_name,
               e.last_name,
               cide.planned_hours::float8 AS planned_hours,
               cide.notes
        FROM calendar_item_day_employees cide
        JOIN calendar_item_days cid ON cide.calendar_item_day_id = cid.id
        JOIN employees e ON cide.employee_id = e.id
        WHERE cid.calendar_item_id = $1
        ORDER BY cid.day_number, e.last_name, e.first_name
        "#,
    )
    .bind(calendar_item_id)
    .fetch_all(pool)
    .await
}

/// Delete all `calendar_item_days` for a calendar item (within a transaction).
///
/// **Caller**: `calendar::put_calendar_item_days`
/// **Why**: Full-replace semantics. Cascade also deletes `calendar_item_day_employees`.
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

/// Insert a single `calendar_item_day` (within a transaction).
///
/// **Caller**: `calendar::put_calendar_item_days`
/// **Why**: Inserts one day in the multi-day Termin schedule. Returns the new row's UUID
///          so the caller can insert `calendar_item_day_employees` against it.
pub(crate) async fn insert_calendar_item_day(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_item_id: Uuid,
    day_date: NaiveDate,
    day_number: i16,
    notes: Option<&str>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
) -> Result<Uuid, sqlx::Error> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO calendar_item_days (calendar_item_id, day_date, day_number, notes, start_time, end_time) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(calendar_item_id)
    .bind(day_date)
    .bind(day_number)
    .bind(notes)
    .bind(start_time)
    .bind(end_time)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Insert one per-day employee assignment for a calendar item day (within a transaction).
///
/// **Caller**: `calendar::put_calendar_item_days`
/// **Why**: Assigns an employee with optional planned hours to a specific Termin day.
pub(crate) async fn insert_calendar_item_day_employee(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_item_day_id: Uuid,
    employee_id: Uuid,
    planned_hours: Option<f64>,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO calendar_item_day_employees (calendar_item_day_id, employee_id, planned_hours, notes) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (calendar_item_day_id, employee_id) DO NOTHING",
    )
    .bind(calendar_item_day_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(notes)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ── Calendar item schedule queries ────────────────────────────────────────────

/// Schedule calendar item row — per-day projection used by `get_schedule`.
///
/// Mirrors `ScheduleInquiryRow` for calendar items (Termine).
#[derive(Debug, FromRow)]
pub(crate) struct ScheduleCalendarItemRow {
    pub effective_date: NaiveDate,
    pub calendar_item_id: Uuid,
    pub title: String,
    pub category: String,
    pub location: Option<String>,
    pub start_time: chrono::NaiveTime,
    pub end_time: Option<chrono::NaiveTime>,
    pub employees_assigned: i64,
    pub employee_names: Option<String>,
    pub day_number: Option<i16>,
    pub total_days: Option<i16>,
    pub day_notes: Option<String>,
}

/// Fetch calendar items as per-day schedule rows within a date range.
///
/// **Caller**: `calendar::get_schedule`
/// **Why**: Returns one row per calendar-item-day for multi-day items, or one row
///          per single-day item. Mirrors `fetch_schedule_inquiries` for Termine.
///
/// Single-day branch uses `calendar_item_employees` for the employee count
/// (only fires for items with no `calendar_item_days` rows — i.e. unassigned).
/// Multi-day branch uses `calendar_item_day_employees` for per-day staffing.
pub(crate) async fn fetch_schedule_calendar_items(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleCalendarItemRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        -- Single-day branch: calendar item has no rows in calendar_item_days
        SELECT
            ci.scheduled_date AS effective_date,
            ci.id AS calendar_item_id,
            ci.title,
            ci.category,
            ci.location,
            ci.start_time,
            ci.end_time,
            COUNT(cie.id) AS employees_assigned,
            NULLIF(STRING_AGG(
                e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', ' ORDER BY e.last_name, e.first_name
            ), '') AS employee_names,
            NULL::smallint AS day_number,
            NULL::smallint AS total_days,
            NULL::text AS day_notes
        FROM calendar_items ci
        LEFT JOIN calendar_item_employees cie ON cie.calendar_item_id = ci.id
        LEFT JOIN employees e ON cie.employee_id = e.id
        WHERE NOT EXISTS (SELECT 1 FROM calendar_item_days WHERE calendar_item_id = ci.id)
          AND ci.scheduled_date BETWEEN $1 AND $2
          AND ci.status NOT IN ('cancelled')
        GROUP BY ci.id

        UNION ALL

        -- Multi-day branch: one row per day from calendar_item_days.
        SELECT
            cday.day_date AS effective_date,
            ci.id AS calendar_item_id,
            ci.title,
            ci.category,
            ci.location,
            COALESCE(cday.start_time, ci.start_time) AS start_time,
            COALESCE(cday.end_time,   ci.end_time)     AS end_time,
            COUNT(cdde.id) AS employees_assigned,
            NULLIF(STRING_AGG(
                e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', ' ORDER BY e.last_name, e.first_name
            ), '') AS employee_names,
            cday.day_number,
            total.total_days,
            cday.notes AS day_notes
        FROM calendar_items ci
        JOIN calendar_item_days cday ON cday.calendar_item_id = ci.id
        JOIN (
            SELECT calendar_item_id, COUNT(*)::smallint AS total_days
            FROM calendar_item_days
            GROUP BY calendar_item_id
        ) total ON total.calendar_item_id = ci.id
        LEFT JOIN calendar_item_day_employees cdde ON cdde.calendar_item_day_id = cday.id
        LEFT JOIN employees e ON cdde.employee_id = e.id
        WHERE cday.day_date BETWEEN $1 AND $2
          AND ci.status NOT IN ('cancelled')
        GROUP BY ci.id, cday.id, cday.day_date, cday.day_number,
                 cday.start_time, cday.end_time, cday.notes, total.total_days

        ORDER BY effective_date
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}
