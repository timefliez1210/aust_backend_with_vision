//! Calendar item repository — centralised queries for the `calendar_items` and
//! `calendar_item_employees` tables.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// A single calendar item row as returned from the database.
#[derive(Debug, serde::Serialize, FromRow)]
pub(crate) struct CalendarItemRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub category: String,
    pub location: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    pub start_time: NaiveTime,
    pub end_time: Option<NaiveTime>,
    pub duration_hours: f64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[sqlx(default)]
    pub customer_id: Option<Uuid>,
    #[sqlx(default)]
    pub customer_name: Option<String>,
    #[sqlx(default)]
    pub customer_type: Option<String>,
    #[sqlx(default)]
    pub company_name: Option<String>,
    #[sqlx(default)]
    pub employee_notes: Option<String>,
    #[sqlx(default)]
    pub end_date: Option<NaiveDate>,
    #[sqlx(default)]
    pub has_pauschale: bool,
}

/// Employee assignment record for a calendar item.
#[derive(Debug, serde::Serialize, FromRow)]
pub(crate) struct CalendarItemEmployee {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
    pub transport_mode: Option<String>,
    pub travel_costs_cents: Option<i64>,
    pub accommodation_cents: Option<i64>,
    pub misc_costs_cents: Option<i64>,
    pub meal_deduction: Option<String>,
}

/// Fetch a single calendar item row by id, LEFT JOINing the customer name.
///
/// **Caller**: `create_item`, `get_item`, `update_item`
/// **Why**: RETURNING clauses cannot do JOINs in PostgreSQL; this centralises
/// the post-write SELECT with customer data.
///
/// # Errors
/// `404 Not Found` when no item with the given UUID exists.
pub(crate) async fn fetch_item_row(pool: &PgPool, id: Uuid) -> Result<CalendarItemRow, ApiError> {
    sqlx::query_as(
        r#"
        SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
               ci.scheduled_date, ci.start_time, ci.end_time,
               ci.duration_hours::float8 AS duration_hours,
               ci.status, ci.created_at, ci.updated_at,
               ci.customer_id, c.name AS customer_name, c.customer_type, c.company_name,
               ci.employee_notes, ci.end_date, ci.has_pauschale
        FROM calendar_items ci
        LEFT JOIN customers c ON c.id = ci.customer_id
        WHERE ci.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Kalendereintrag nicht gefunden".into()))
}

/// List calendar items filtered by month range.
///
/// **Caller**: `list_items` handler
/// **Why**: Month-filtered calendar view for the admin dashboard.
pub(crate) async fn list_items_by_month(
    pool: &PgPool,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<CalendarItemRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
               ci.scheduled_date, ci.start_time, ci.end_time,
               ci.duration_hours::float8 AS duration_hours,
               ci.status, ci.created_at, ci.updated_at,
               ci.customer_id, c.name AS customer_name, c.customer_type, c.company_name,
               ci.employee_notes, ci.end_date, ci.has_pauschale
        FROM calendar_items ci
        LEFT JOIN customers c ON c.id = ci.customer_id
        WHERE ci.scheduled_date >= $1 AND ci.scheduled_date < $2
        ORDER BY ci.scheduled_date ASC NULLS LAST
        "#,
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await
}

/// List all calendar items (no month filter).
///
/// **Caller**: `list_items` handler
/// **Why**: Unfiltered calendar view for the admin dashboard.
pub(crate) async fn list_items_all(pool: &PgPool) -> Result<Vec<CalendarItemRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
               ci.scheduled_date, ci.start_time, ci.end_time,
               ci.duration_hours::float8 AS duration_hours,
               ci.status, ci.created_at, ci.updated_at,
               ci.customer_id, c.name AS customer_name, c.customer_type, c.company_name,
               ci.employee_notes, ci.end_date, ci.has_pauschale
        FROM calendar_items ci
        LEFT JOIN customers c ON c.id = ci.customer_id
        ORDER BY ci.scheduled_date ASC NULLS LAST
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Insert a new calendar item and return its ID.
///
/// **Caller**: `create_item` handler
/// **Why**: Creates a new internal event record.
pub(crate) async fn insert_item(
    pool: &PgPool,
    title: &str,
    description: Option<&str>,
    category: &str,
    location: Option<&str>,
    scheduled_date: Option<NaiveDate>,
    start_time: NaiveTime,
    end_time: Option<NaiveTime>,
    duration_hours: f64,
    customer_id: Option<Uuid>,
    end_date: Option<NaiveDate>,
) -> Result<Uuid, sqlx::Error> {
    let (new_id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO calendar_items (title, description, category, location, scheduled_date, start_time, end_time, duration_hours, customer_id, end_date)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING id
        "#,
    )
    .bind(title)
    .bind(description)
    .bind(category)
    .bind(location)
    .bind(scheduled_date)
    .bind(start_time)
    .bind(end_time)
    .bind(duration_hours)
    .bind(customer_id)
    .bind(end_date)
    .fetch_one(pool)
    .await?;
    Ok(new_id)
}

/// Check whether a calendar item with the given ID exists.
///
/// **Caller**: `update_item`, `list_item_employees`, `assign_employee`
/// **Why**: Validates item_id before proceeding.
pub(crate) async fn item_exists(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM calendar_items WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Delete a calendar item by ID.
///
/// **Caller**: `delete_item` handler
/// **Why**: Removes cancelled or erroneous internal events.
///
/// # Returns
/// Number of rows deleted (0 or 1).
pub(crate) async fn delete_item(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM calendar_items WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Check whether an employee with the given ID exists.
///
/// **Caller**: `assign_employee` handler
/// **Why**: Validates employee_id before assigning.
pub(crate) async fn employee_exists(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM employees WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Insert calendar item employee assignment rows — one row per day in scheduled_date..=end_date.
///
/// **Caller**: `assign_employee` handler
/// **Why**: Links an employee to an internal work item for every day of its date range.
pub(crate) async fn insert_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO calendar_item_employees (id, calendar_item_id, employee_id, job_date, start_time, end_time)
        SELECT gen_random_uuid(), $1, $2,
               d::date,
               COALESCE(start_time, '08:00'::time),
               COALESCE(end_time,   '17:00'::time)
        FROM calendar_items,
             generate_series(
                 COALESCE(scheduled_date, created_at::date),
                 COALESCE(end_date, scheduled_date, created_at::date),
                 '1 day'::interval
             ) AS d
        WHERE id = $1
        ON CONFLICT (calendar_item_id, employee_id, job_date) DO NOTHING
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a calendar item employee assignment (hours, clock times, notes).
///
/// **Caller**: `update_item_employee` handler
/// **Why**: Operators enter actual hours or correct planned hours.
///
/// Updates the row matching (calendar_item_id, employee_id, job_date). When job_date
/// is None, falls back to the item's scheduled_date.
pub(crate) async fn update_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
    clock_in: Option<NaiveTime>,
    clock_out: Option<NaiveTime>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours_override: Option<f64>,
    notes: Option<&str>,
    day_date: Option<chrono::NaiveDate>,
    transport_mode: Option<&str>,
    travel_costs_cents: Option<i64>,
    accommodation_cents: Option<i64>,
    misc_costs_cents: Option<i64>,
    meal_deduction: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let break_min_f = break_minutes.unwrap_or(0) as f64;
    let computed_actual_hours: Option<f64> = if let Some(ah) = actual_hours_override {
        Some(ah)
    } else if let (Some(ci), Some(co)) = (clock_in, clock_out) {
        let duration_secs = (co - ci).num_seconds() as f64;
        Some(duration_secs / 3600.0 - break_min_f / 60.0)
    } else {
        None
    };

    let result = sqlx::query(
        r#"
        UPDATE calendar_item_employees SET
            clock_in            = COALESCE($3, clock_in),
            clock_out           = COALESCE($4, clock_out),
            start_time          = COALESCE($5, start_time),
            end_time            = COALESCE($6, end_time),
            break_minutes       = COALESCE($7, break_minutes),
            actual_hours        = $8,
            notes               = COALESCE($9, notes),
            transport_mode      = COALESCE($11, transport_mode),
            travel_costs_cents  = COALESCE($12, travel_costs_cents),
            accommodation_cents = COALESCE($13, accommodation_cents),
            misc_costs_cents    = COALESCE($14, misc_costs_cents),
            meal_deduction      = COALESCE($15, meal_deduction)
        WHERE calendar_item_id = $1
          AND employee_id = $2
          AND job_date = COALESCE($10, (SELECT COALESCE(scheduled_date, created_at::date) FROM calendar_items WHERE id = $1))
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(computed_actual_hours)
    .bind(notes)
    .bind(day_date)
    .bind(transport_mode)
    .bind(travel_costs_cents)
    .bind(accommodation_cents)
    .bind(misc_costs_cents)
    .bind(meal_deduction)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Fetch a single employee assignment for a calendar item.
///
/// **Caller**: `update_item_employee` handler
/// **Why**: Returns updated assignment data after an update.
pub(crate) async fn fetch_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
) -> Result<Option<CalendarItemEmployee>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT cie.employee_id,
               e.first_name,
               e.last_name,
               MIN(cie.clock_in)  AS clock_in,
               MAX(cie.clock_out) AS clock_out,
               MIN(cie.start_time) AS start_time,
               MAX(cie.end_time)   AS end_time,
               COALESCE(MAX(cie.break_minutes), 0)::int AS break_minutes,
               SUM(COALESCE(cie.actual_hours,
                            CASE WHEN cie.clock_out IS NOT NULL AND cie.clock_in IS NOT NULL
                                 THEN (EXTRACT(EPOCH FROM (cie.clock_out - cie.clock_in)) / 3600.0)
                                 ELSE NULL END))::float8 AS actual_hours,
               STRING_AGG(cie.notes, '; ' ORDER BY cie.job_date) AS notes,
               MAX(cie.transport_mode)      AS transport_mode,
               SUM(cie.travel_costs_cents)  AS travel_costs_cents,
               SUM(cie.accommodation_cents) AS accommodation_cents,
               SUM(cie.misc_costs_cents)    AS misc_costs_cents,
               MAX(cie.meal_deduction)      AS meal_deduction
        FROM calendar_item_employees cie
        JOIN employees e ON e.id = cie.employee_id
        WHERE cie.calendar_item_id = $1 AND cie.employee_id = $2
        GROUP BY cie.employee_id, e.first_name, e.last_name
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .fetch_optional(pool)
    .await
}

/// Delete a calendar item employee assignment.
///
/// **Caller**: `remove_item_employee` handler
/// **Why**: Unassigns an employee from an internal work item.
///          Deletes from both day-level and flat tables for transition compatibility.
///
pub(crate) async fn delete_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM calendar_item_employees WHERE calendar_item_id = $1 AND employee_id = $2",
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Fetch all employee assignments for a calendar item, joined with employee names.
///
/// **Caller**: `get_item`, `list_item_employees`, `assign_employee`
/// **Why**: Centralises the join query so the result shape is consistent across all
/// handlers that need the employee list.
///
/// # Returns
/// Ordered list of `CalendarItemEmployee` (by last_name, first_name).
pub(crate) async fn fetch_item_employees(
    pool: &PgPool,
    item_id: Uuid,
) -> Result<Vec<CalendarItemEmployee>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT cie.employee_id,
               e.first_name,
               e.last_name,
               MIN(cie.clock_in)  AS clock_in,
               MAX(cie.clock_out) AS clock_out,
               MIN(cie.start_time) AS start_time,
               MAX(cie.end_time)   AS end_time,
               COALESCE(MAX(cie.break_minutes), 0)::int AS break_minutes,
               SUM(COALESCE(cie.actual_hours,
                            CASE WHEN cie.clock_out IS NOT NULL AND cie.clock_in IS NOT NULL
                                 THEN (EXTRACT(EPOCH FROM (cie.clock_out - cie.clock_in)) / 3600.0)
                                 ELSE NULL END))::float8 AS actual_hours,
               STRING_AGG(cie.notes, '; ' ORDER BY cie.job_date) AS notes,
               MAX(cie.transport_mode)      AS transport_mode,
               SUM(cie.travel_costs_cents)  AS travel_costs_cents,
               SUM(cie.accommodation_cents) AS accommodation_cents,
               SUM(cie.misc_costs_cents)    AS misc_costs_cents,
               MAX(cie.meal_deduction)      AS meal_deduction
        FROM calendar_item_employees cie
        JOIN employees e ON e.id = cie.employee_id
        WHERE cie.calendar_item_id = $1
        GROUP BY cie.employee_id, e.first_name, e.last_name
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(item_id)
    .fetch_all(pool)
    .await
}

