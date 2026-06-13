//! Calendar repository — queries for schedule, availability, and employee assignments.

use chrono::{NaiveDate, NaiveTime};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ── Row types ────────────────────────────────────────────────────────────────

/// Schedule inquiry row — one row per (inquiry × job_date) in the window.
#[derive(Debug, FromRow)]
pub(crate) struct ScheduleInquiryRow {
    pub effective_date: NaiveDate,
    pub inquiry_id: Uuid,
    pub customer_name: Option<String>,
    #[sqlx(default)]
    pub customer_email: Option<String>,
    #[sqlx(default)]
    pub customer_phone: Option<String>,
    #[sqlx(default)]
    pub customer_type: Option<String>,
    #[sqlx(default)]
    pub company_name: Option<String>,
    pub departure_address: Option<String>,
    pub arrival_address: Option<String>,
    pub volume_m3: Option<f64>,
    pub status: String,
    pub notes: Option<String>,
    pub employee_notes: Option<String>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub employees_assigned: i64,
    pub employee_names: Option<String>,
    pub day_number: i32,
    pub total_days: i32,
    #[sqlx(default)]
    pub service_type: Option<String>,
    pub scheduled_date: NaiveDate,
}

/// Schedule calendar item row — one row per (item × job_date) in the window.
#[derive(Debug, FromRow)]
pub(crate) struct ScheduleCalendarItemRow {
    pub effective_date: NaiveDate,
    pub calendar_item_id: Uuid,
    pub title: String,
    pub category: String,
    pub location: Option<String>,
    pub start_time: NaiveTime,
    pub end_time: Option<NaiveTime>,
    pub employees_assigned: i64,
    pub employee_names: Option<String>,
    pub day_number: i32,
    pub total_days: i32,
    pub scheduled_date: NaiveDate,
    pub description: Option<String>,
}

/// One employee assignment row returned by `fetch_inquiry_employees` /
/// `fetch_calendar_item_employees`.
#[derive(Debug, FromRow, serde::Serialize)]
pub(crate) struct EmployeeAssignmentRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub job_date: NaiveDate,
    pub notes: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub transport_mode: Option<String>,
    pub travel_costs_cents: Option<i64>,
    pub accommodation_cents: Option<i64>,
    pub misc_costs_cents: Option<i64>,
    pub meal_deduction: Option<String>,
}

/// Input for one employee assignment (used by put_inquiry_employees / put_calendar_item_employees).
pub(crate) struct EmployeeAssignmentInput {
    pub employee_id: Uuid,
    pub job_date: NaiveDate,
    pub notes: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub transport_mode: Option<String>,
    pub travel_costs_cents: Option<i64>,
    pub accommodation_cents: Option<i64>,
    pub misc_costs_cents: Option<i64>,
    pub meal_deduction: Option<String>,
}

// ── Availability ──────────────────────────────────────────────────────────────

/// Count active inquiries and calendar items that span a given date.
///
/// **Caller**: `calendar::count_active_on_date`
pub(crate) async fn count_active_on_date(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        r#"
        SELECT (
            (SELECT COUNT(*) FROM inquiries
             WHERE $1 BETWEEN scheduled_date AND COALESCE(end_date, scheduled_date)
               AND status NOT IN ('cancelled', 'rejected', 'expired'))
            +
            (SELECT COUNT(*) FROM calendar_items
             WHERE $1 BETWEEN scheduled_date AND COALESCE(end_date, scheduled_date)
               AND status NOT IN ('cancelled'))
        )
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

// ── Schedule queries ──────────────────────────────────────────────────────────

/// Fetch all schedule inquiries in a date range.
///
/// **Caller**: `calendar::get_schedule`
///
/// One row per (inquiry × day) using `generate_series` to expand multi-day
/// inquiries. Employee assignments are joined by job_date.
pub(crate) async fn fetch_schedule_inquiries(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleInquiryRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            gs.day::date                                              AS effective_date,
            i.id                                                      AS inquiry_id,
            COALESCE(
                NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                c.name, c.email
            )                                                         AS customer_name,
            c.email                                                   AS customer_email,
            c.phone                                                   AS customer_phone,
            c.customer_type,
            c.company_name,
            CASE WHEN ao.id IS NOT NULL THEN ao.street || ', ' || ao.city END AS departure_address,
            CASE WHEN ad.id IS NOT NULL THEN ad.street || ', ' || ad.city END AS arrival_address,
            i.estimated_volume_m3                                     AS volume_m3,
            i.status,
            i.service_type,
            i.notes,
            i.employee_notes,
            COALESCE(i.start_time, '08:00'::time)                    AS start_time,
            COALESCE(i.end_time,   '17:00'::time)                    AS end_time,
            COUNT(ie.employee_id)                                     AS employees_assigned,
            NULLIF(STRING_AGG(
                DISTINCT e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', '
            ), '')                                                    AS employee_names,
            (gs.day::date - i.scheduled_date)::int + 1               AS day_number,
            (COALESCE(i.end_date, i.scheduled_date) - i.scheduled_date)::int + 1 AS total_days,
            i.scheduled_date
        FROM inquiries i
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses ao ON i.origin_address_id      = ao.id
        LEFT JOIN addresses ad ON i.destination_address_id = ad.id
        CROSS JOIN LATERAL generate_series(
            i.scheduled_date,
            COALESCE(i.end_date, i.scheduled_date),
            '1 day'::interval
        ) AS gs(day)
        LEFT JOIN inquiry_employees ie ON ie.inquiry_id = i.id AND ie.job_date = gs.day::date
        LEFT JOIN employees e ON ie.employee_id = e.id
        WHERE gs.day::date BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY gs.day, i.id, c.id, ao.id, ad.id
        ORDER BY gs.day
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
pub(crate) async fn fetch_offer_prices(
    pool: &PgPool,
    inquiry_ids: &[Uuid],
) -> Result<Vec<(Uuid, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT inquiry_id, price_cents FROM offers \
         WHERE inquiry_id = ANY($1) AND status != 'rejected' \
         ORDER BY created_at DESC",
    )
    .bind(inquiry_ids)
    .fetch_all(pool)
    .await
}

/// Fetch capacity overrides for a date range.
///
/// **Caller**: `calendar::get_schedule`
pub(crate) async fn fetch_capacity_overrides_range(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<(NaiveDate, i32)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT override_date, capacity FROM calendar_capacity_overrides \
         WHERE override_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

/// Upsert a capacity override for a specific date.
///
/// **Caller**: `calendar::set_capacity`
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

/// Fetch calendar items as per-day schedule rows within a date range.
///
/// **Caller**: `calendar::get_schedule`
pub(crate) async fn fetch_schedule_calendar_items(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleCalendarItemRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            gs.day::date                                                AS effective_date,
            ci.id                                                       AS calendar_item_id,
            ci.title,
            ci.category,
            ci.location,
            ci.description,
            COALESCE(ci.start_time, '08:00'::time)                     AS start_time,
            ci.end_time,
            COUNT(cie.employee_id)                                      AS employees_assigned,
            NULLIF(STRING_AGG(
                DISTINCT e.first_name || ' ' || LEFT(e.last_name, 1) || '.',
                ', '
            ), '')                                                      AS employee_names,
            (gs.day::date - ci.scheduled_date)::int + 1                AS day_number,
            (COALESCE(ci.end_date, ci.scheduled_date) - ci.scheduled_date)::int + 1 AS total_days,
            ci.scheduled_date
        FROM calendar_items ci
        CROSS JOIN LATERAL generate_series(
            ci.scheduled_date,
            COALESCE(ci.end_date, ci.scheduled_date),
            '1 day'::interval
        ) AS gs(day)
        LEFT JOIN calendar_item_employees cie ON cie.calendar_item_id = ci.id AND cie.job_date = gs.day::date
        LEFT JOIN employees e ON cie.employee_id = e.id
        WHERE gs.day::date BETWEEN $1 AND $2
          AND ci.status NOT IN ('cancelled')
        GROUP BY gs.day, ci.id
        ORDER BY gs.day
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

// ── Employee assignment CRUD ──────────────────────────────────────────────────

/// Fetch all employee assignments for an inquiry, ordered by job_date then name.
///
/// **Caller**: `calendar::get_inquiry_employees`
pub(crate) async fn fetch_inquiry_employees(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<EmployeeAssignmentRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ie.employee_id, e.first_name, e.last_name,
               ie.job_date,
               ie.notes,
               ie.start_time, ie.end_time, ie.clock_in, ie.clock_out,
               COALESCE(ie.break_minutes, 0) AS break_minutes,
               ie.actual_hours::float8 AS actual_hours,
               ie.transport_mode,
               ie.travel_costs_cents,
               ie.accommodation_cents,
               ie.misc_costs_cents,
               ie.meal_deduction
        FROM inquiry_employees ie
        JOIN employees e ON ie.employee_id = e.id
        WHERE ie.inquiry_id = $1
        ORDER BY ie.job_date, e.last_name, e.first_name
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Replace the SET of employee assignments for an inquiry, preserving stored
/// time/hour/cost values on rows that survive the replacement.
///
/// **Caller**: `calendar::put_inquiry_employees`
///
/// **Why not DELETE-all + INSERT**: the inquiry page auto-fires this PUT right
/// after field-level saves, with UI default times in the payload. Full-replace
/// silently wiped just-entered clock times (2026-06-10 incident: 20 saved
/// edits on one job destroyed). Existing non-null values now win over the
/// incoming payload; payload values only fill gaps and create new rows.
pub(crate) async fn put_inquiry_employees(
    pool: &PgPool,
    inquiry_id: Uuid,
    assignments: &[EmployeeAssignmentInput],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    // Delete only assignments that were removed from the list.
    let keep_employee_ids: Vec<Uuid> = assignments.iter().map(|a| a.employee_id).collect();
    let keep_job_dates: Vec<NaiveDate> = assignments.iter().map(|a| a.job_date).collect();
    sqlx::query(
        r#"
        DELETE FROM inquiry_employees ie
        WHERE ie.inquiry_id = $1
          AND NOT EXISTS (
            SELECT 1
            FROM unnest($2::uuid[], $3::date[]) AS keep(employee_id, job_date)
            WHERE keep.employee_id = ie.employee_id AND keep.job_date = ie.job_date
          )
        "#,
    )
    .bind(inquiry_id)
    .bind(&keep_employee_ids)
    .bind(&keep_job_dates)
    .execute(&mut *tx)
    .await?;

    for a in assignments {
        sqlx::query(
            r#"
            INSERT INTO inquiry_employees
                (id, inquiry_id, employee_id, job_date, notes,
                 start_time, end_time, clock_in, clock_out, break_minutes, actual_hours,
                 transport_mode, travel_costs_cents, accommodation_cents, misc_costs_cents, meal_deduction)
            VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            ON CONFLICT (inquiry_id, employee_id, job_date) DO UPDATE SET
                notes               = COALESCE(EXCLUDED.notes, inquiry_employees.notes),
                start_time          = COALESCE(inquiry_employees.start_time, EXCLUDED.start_time),
                end_time            = COALESCE(inquiry_employees.end_time, EXCLUDED.end_time),
                clock_in            = COALESCE(inquiry_employees.clock_in, EXCLUDED.clock_in),
                clock_out           = COALESCE(inquiry_employees.clock_out, EXCLUDED.clock_out),
                actual_hours        = COALESCE(inquiry_employees.actual_hours, EXCLUDED.actual_hours),
                transport_mode      = COALESCE(inquiry_employees.transport_mode, EXCLUDED.transport_mode),
                travel_costs_cents  = COALESCE(inquiry_employees.travel_costs_cents, EXCLUDED.travel_costs_cents),
                accommodation_cents = COALESCE(inquiry_employees.accommodation_cents, EXCLUDED.accommodation_cents),
                misc_costs_cents    = COALESCE(inquiry_employees.misc_costs_cents, EXCLUDED.misc_costs_cents),
                meal_deduction      = COALESCE(inquiry_employees.meal_deduction, EXCLUDED.meal_deduction),
                updated_at          = now()
            "#,
        )
        .bind(inquiry_id)
        .bind(a.employee_id)
        .bind(a.job_date)
        .bind(&a.notes)
        .bind(a.start_time)
        .bind(a.end_time)
        .bind(a.clock_in)
        .bind(a.clock_out)
        .bind(a.break_minutes)
        .bind(a.actual_hours)
        .bind(&a.transport_mode)
        .bind(a.travel_costs_cents)
        .bind(a.accommodation_cents)
        .bind(a.misc_costs_cents)
        .bind(&a.meal_deduction)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Fetch all employee assignments for a calendar item, ordered by job_date then name.
///
/// **Caller**: `calendar::get_calendar_item_employees`
pub(crate) async fn fetch_calendar_item_employees(
    pool: &PgPool,
    calendar_item_id: Uuid,
) -> Result<Vec<EmployeeAssignmentRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT cie.employee_id, e.first_name, e.last_name,
               cie.job_date,
               cie.notes,
               cie.start_time, cie.end_time, cie.clock_in, cie.clock_out,
               COALESCE(cie.break_minutes, 0) AS break_minutes,
               cie.actual_hours::float8 AS actual_hours,
               cie.transport_mode,
               cie.travel_costs_cents,
               cie.accommodation_cents,
               cie.misc_costs_cents,
               cie.meal_deduction
        FROM calendar_item_employees cie
        JOIN employees e ON cie.employee_id = e.id
        WHERE cie.calendar_item_id = $1
        ORDER BY cie.job_date, e.last_name, e.first_name
        "#,
    )
    .bind(calendar_item_id)
    .fetch_all(pool)
    .await
}

/// Replace the SET of employee assignments for a calendar item, preserving
/// stored time/hour/cost values on rows that survive the replacement.
/// Same rationale as [`put_inquiry_employees`] — full-replace wiped entered hours.
///
/// **Caller**: `calendar::put_calendar_item_employees`
pub(crate) async fn put_calendar_item_employees(
    pool: &PgPool,
    calendar_item_id: Uuid,
    assignments: &[EmployeeAssignmentInput],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let keep_employee_ids: Vec<Uuid> = assignments.iter().map(|a| a.employee_id).collect();
    let keep_job_dates: Vec<NaiveDate> = assignments.iter().map(|a| a.job_date).collect();
    sqlx::query(
        r#"
        DELETE FROM calendar_item_employees cie
        WHERE cie.calendar_item_id = $1
          AND NOT EXISTS (
            SELECT 1
            FROM unnest($2::uuid[], $3::date[]) AS keep(employee_id, job_date)
            WHERE keep.employee_id = cie.employee_id AND keep.job_date = cie.job_date
          )
        "#,
    )
    .bind(calendar_item_id)
    .bind(&keep_employee_ids)
    .bind(&keep_job_dates)
    .execute(&mut *tx)
    .await?;

    for a in assignments {
        sqlx::query(
            r#"
            INSERT INTO calendar_item_employees
                (id, calendar_item_id, employee_id, job_date, notes,
                 start_time, end_time, clock_in, clock_out, break_minutes, actual_hours,
                 transport_mode, travel_costs_cents, accommodation_cents, misc_costs_cents, meal_deduction)
            VALUES (gen_random_uuid(), $1, $2, $3, $15, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT (calendar_item_id, employee_id, job_date) DO UPDATE SET
                notes               = COALESCE(EXCLUDED.notes, calendar_item_employees.notes),
                start_time          = COALESCE(calendar_item_employees.start_time, EXCLUDED.start_time),
                end_time            = COALESCE(calendar_item_employees.end_time, EXCLUDED.end_time),
                clock_in            = COALESCE(calendar_item_employees.clock_in, EXCLUDED.clock_in),
                clock_out           = COALESCE(calendar_item_employees.clock_out, EXCLUDED.clock_out),
                actual_hours        = COALESCE(calendar_item_employees.actual_hours, EXCLUDED.actual_hours),
                transport_mode      = COALESCE(calendar_item_employees.transport_mode, EXCLUDED.transport_mode),
                travel_costs_cents  = COALESCE(calendar_item_employees.travel_costs_cents, EXCLUDED.travel_costs_cents),
                accommodation_cents = COALESCE(calendar_item_employees.accommodation_cents, EXCLUDED.accommodation_cents),
                misc_costs_cents    = COALESCE(calendar_item_employees.misc_costs_cents, EXCLUDED.misc_costs_cents),
                meal_deduction      = COALESCE(calendar_item_employees.meal_deduction, EXCLUDED.meal_deduction)
            "#,
        )
        .bind(calendar_item_id)
        .bind(a.employee_id)
        .bind(a.job_date)
        .bind(a.start_time)
        .bind(a.end_time)
        .bind(a.clock_in)
        .bind(a.clock_out)
        .bind(a.break_minutes)
        .bind(a.actual_hours)
        .bind(&a.transport_mode)
        .bind(a.travel_costs_cents)
        .bind(a.accommodation_cents)
        .bind(a.misc_costs_cents)
        .bind(&a.meal_deduction)
        .bind(&a.notes)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}
