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
}

/// Employee assignment record for a calendar item.
#[derive(Debug, serde::Serialize, FromRow)]
pub(crate) struct CalendarItemEmployee {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub planned_hours: Option<f64>,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
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
               ci.employee_notes
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
               ci.employee_notes
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
               ci.employee_notes
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
) -> Result<Uuid, sqlx::Error> {
    let (new_id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO calendar_items (title, description, category, location, scheduled_date, start_time, end_time, duration_hours, customer_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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

/// Insert a calendar item employee assignment.
///
/// **Caller**: `assign_employee` handler
/// **Why**: Links an employee to an internal work item with planned hours.
///          Auto-creates a day-1 calendar_item_days row if one doesn't exist,
///          then inserts into calendar_item_day_employees.
pub(crate) async fn insert_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
    planned_hours: f64,
) -> Result<(), sqlx::Error> {
    // Ensure a day-1 calendar_item_days row exists
    let day_id: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_item_days WHERE calendar_item_id = $1 AND day_number = 1",
    )
    .bind(calendar_item_id)
    .fetch_optional(pool)
    .await?;

    let calendar_item_day_id = if let Some((existing_id,)) = day_id {
        existing_id
    } else {
        let row: (Uuid,) = sqlx::query_as(
            r#"
            INSERT INTO calendar_item_days (calendar_item_id, day_date, day_number, start_time, end_time)
            SELECT $1, COALESCE(scheduled_date, created_at::date), 1, start_time, end_time
            FROM calendar_items WHERE id = $1
            RETURNING id
            "#,
        )
        .bind(calendar_item_id)
        .fetch_one(pool)
        .await?;
        row.0
    };

    // Insert into day-level table
    sqlx::query(
        r#"
        INSERT INTO calendar_item_day_employees (calendar_item_day_id, employee_id, planned_hours)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(calendar_item_day_id)
    .bind(employee_id)
    .bind(planned_hours)
    .execute(pool)
    .await?;

    // Also insert into flat table for backwards compat
    let _ = sqlx::query(
        r#"
        INSERT INTO calendar_item_employees (calendar_item_id, employee_id, planned_hours)
        VALUES ($1, $2, $3)
        ON CONFLICT (calendar_item_id, employee_id) DO NOTHING
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .bind(planned_hours)
    .execute(pool)
    .await;

    Ok(())
}

/// Update a calendar item employee assignment (hours, clock times, notes).
///
/// **Caller**: `update_item_employee` handler
/// **Why**: Operators enter actual hours or correct planned hours.
///          Updates both day-level and flat tables for transition compatibility.
///
/// # Returns
/// Number of rows affected in the day-level table (0 if assignment not found).
pub(crate) async fn update_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
    planned_hours: Option<f64>,
    clock_in: Option<NaiveTime>,
    clock_out: Option<NaiveTime>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours_override: Option<f64>,
    notes: Option<&str>,
    day_date: Option<chrono::NaiveDate>,
) -> Result<u64, sqlx::Error> {
    // Derive actual_hours in Rust
    let break_min_f = break_minutes.unwrap_or(0) as f64;
    let computed_actual_hours: Option<f64> = if let Some(ah) = actual_hours_override {
        Some(ah)
    } else if let (Some(ci), Some(co)) = (clock_in, clock_out) {
        let duration_secs = (co - ci).num_seconds() as f64;
        Some(duration_secs / 3600.0 - break_min_f / 60.0)
    } else {
        None
    };

    // Update day-level table
    let result = sqlx::query(
        r#"
        UPDATE calendar_item_day_employees SET
            clock_in      = COALESCE($4, calendar_item_day_employees.clock_in),
            clock_out     = COALESCE($5, calendar_item_day_employees.clock_out),
            start_time    = COALESCE($6, calendar_item_day_employees.start_time),
            end_time      = COALESCE($7, calendar_item_day_employees.end_time),
            break_minutes = COALESCE($8, calendar_item_day_employees.break_minutes),
            actual_hours  = $9,
            planned_hours = CASE
                WHEN $9 IS NOT NULL THEN $9
                WHEN COALESCE($4, calendar_item_day_employees.clock_in) IS NOT NULL AND COALESCE($5, calendar_item_day_employees.clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, calendar_item_day_employees.clock_out) - COALESCE($4, calendar_item_day_employees.clock_in))) / 3600.0)
                ELSE COALESCE($3, calendar_item_day_employees.planned_hours)
            END,
            notes = COALESCE($10, calendar_item_day_employees.notes)
        FROM calendar_item_days cday
        WHERE calendar_item_day_id = cday.id
          AND cday.calendar_item_id = $1
          AND employee_id = $2
          AND ($11::date IS NULL OR cday.day_date = $11)
          AND ($11::date IS NOT NULL OR cday.day_number = 1)
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(computed_actual_hours)
    .bind(notes)
    .bind(day_date)
    .execute(pool)
    .await?;

    // Also update flat table for backwards compat — only when editing the single-day default.
    // Per-day edits would otherwise overwrite the aggregate with a single day's values.
    if day_date.is_none() {
    let _ = sqlx::query(
        r#"
        UPDATE calendar_item_employees SET
            clock_in      = COALESCE($4, clock_in),
            clock_out     = COALESCE($5, clock_out),
            start_time    = COALESCE($6, start_time),
            end_time      = COALESCE($7, end_time),
            break_minutes = COALESCE($8, break_minutes),
            actual_hours  = $9,
            planned_hours = CASE
                WHEN $9 IS NOT NULL THEN $9
                WHEN COALESCE($4, clock_in) IS NOT NULL AND COALESCE($5, clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, clock_out) - COALESCE($4, clock_in))) / 3600.0)
                ELSE COALESCE($3, planned_hours)
            END,
            notes = COALESCE($10, notes)
        WHERE calendar_item_id = $1 AND employee_id = $2
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(computed_actual_hours)
    .bind(notes)
    .execute(pool)
    .await;
    }

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
        SELECT cdde.employee_id,
               e.first_name,
               e.last_name,
               SUM(cdde.planned_hours)::float8 AS planned_hours,
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.clock_in END) AS clock_in,
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.clock_out END) AS clock_out,
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.start_time END) AS start_time,
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.end_time END) AS end_time,
               COALESCE(MAX(CASE WHEN cday.day_number = 1 THEN cdde.break_minutes END), 0)::int AS break_minutes,
               SUM(COALESCE(cdde.actual_hours,
                            CASE WHEN cdde.clock_out IS NOT NULL AND cdde.clock_in IS NOT NULL
                                 THEN (EXTRACT(EPOCH FROM (cdde.clock_out - cdde.clock_in)) / 3600.0)
                                 ELSE NULL END))::float8 AS actual_hours,
               STRING_AGG(cdde.notes, '; ' ORDER BY cday.day_number) AS notes
        FROM calendar_item_day_employees cdde
        JOIN calendar_item_days cday ON cdde.calendar_item_day_id = cday.id
        JOIN employees e ON e.id = cdde.employee_id
        WHERE cday.calendar_item_id = $1 AND cdde.employee_id = $2
        GROUP BY cdde.employee_id, e.first_name, e.last_name
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
/// # Returns
/// Number of rows deleted from the day-level table (0 or more).
pub(crate) async fn delete_item_employee(
    pool: &PgPool,
    calendar_item_id: Uuid,
    employee_id: Uuid,
) -> Result<u64, sqlx::Error> {
    // Delete from day-level table (may affect multiple days)
    let result = sqlx::query(
        r#"
        DELETE FROM calendar_item_day_employees
        WHERE employee_id = $2
          AND calendar_item_day_id IN (
              SELECT id FROM calendar_item_days WHERE calendar_item_id = $1
          )
        "#,
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .execute(pool)
    .await?;

    // Also delete from flat table
    let _ = sqlx::query(
        "DELETE FROM calendar_item_employees WHERE calendar_item_id = $1 AND employee_id = $2",
    )
    .bind(calendar_item_id)
    .bind(employee_id)
    .execute(pool)
    .await;

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
        SELECT cdde.employee_id,
               e.first_name,
               e.last_name,
               SUM(cdde.planned_hours)::float8 AS planned_hours,
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.clock_in END) AS clock_in,
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.clock_out END) AS clock_out,
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.start_time END) AS start_time,
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.end_time END) AS end_time,
               COALESCE(MAX(CASE WHEN cday.day_number = 1 THEN cdde.break_minutes END), 0)::int AS break_minutes,
               SUM(COALESCE(cdde.actual_hours,
                            CASE WHEN cdde.clock_out IS NOT NULL AND cdde.clock_in IS NOT NULL
                                 THEN (EXTRACT(EPOCH FROM (cdde.clock_out - cdde.clock_in)) / 3600.0)
                                 ELSE NULL END))::float8 AS actual_hours,
               STRING_AGG(cdde.notes, '; ' ORDER BY cday.day_number) AS notes
        FROM calendar_item_day_employees cdde
        JOIN calendar_item_days cday ON cdde.calendar_item_day_id = cday.id
        JOIN employees e ON e.id = cdde.employee_id
        WHERE cday.calendar_item_id = $1
        GROUP BY cdde.employee_id, e.first_name, e.last_name
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(item_id)
    .fetch_all(pool)
    .await
}

// ── Flat-table sync (transition compatibility) ───────────────────────────────

/// After day-level writes, sync the flat `calendar_item_employees` table so
/// that any remaining flat-table reads return correct data.
///
/// **Caller**: `put_calendar_item_days` handler, after committing day-level data.
/// **Why**: The multi-day editor writes only to `calendar_item_day_employees`. This
///          function rebuilds the flat table from day-level data.
pub(crate) async fn sync_flat_calendar_item_employees(
    pool: &PgPool,
    calendar_item_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Delete all existing flat-table rows for this calendar item
    sqlx::query("DELETE FROM calendar_item_employees WHERE calendar_item_id = $1")
        .bind(calendar_item_id)
        .execute(pool)
        .await?;

    // Re-insert from day-level data, aggregating per employee
    sqlx::query(
        r#"
        INSERT INTO calendar_item_employees (calendar_item_id, employee_id, planned_hours, clock_in, clock_out, start_time, end_time, break_minutes, actual_hours, notes, created_at)
        SELECT cday.calendar_item_id,
               cdde.employee_id,
               SUM(cdde.planned_hours),
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.clock_in END),
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.clock_out END),
               MIN(CASE WHEN cday.day_number = 1 THEN cdde.start_time END),
               MAX(CASE WHEN cday.day_number = 1 THEN cdde.end_time END),
               COALESCE(MAX(CASE WHEN cday.day_number = 1 THEN cdde.break_minutes END), 0),
               MAX(cdde.actual_hours),
               STRING_AGG(cdde.notes, '; ' ORDER BY cday.day_number),
               NOW()
        FROM calendar_item_day_employees cdde
        JOIN calendar_item_days cday ON cdde.calendar_item_day_id = cday.id
        WHERE cday.calendar_item_id = $1
        GROUP BY cday.calendar_item_id, cdde.employee_id
        "#,
    )
    .bind(calendar_item_id)
    .execute(pool)
    .await?;

    Ok(())
}
