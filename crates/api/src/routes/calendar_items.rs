use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
use crate::{error::ApiError, AppState};

/// Register all calendar-items routes (protected under admin JWT middleware).
///
/// **Caller**: `crates/api/src/lib.rs` — nested under `/admin/calendar-items` inside the
/// admin JWT-protected layer.
/// **Why**: Internal work items (training, maintenance, vehicle inspection, etc.) need
/// employee assignment and hours tracking just like moving inquiries.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_items).post(create_item))
        .route("/{id}", get(get_item).patch(update_item).delete(delete_item))
        .route("/{id}/employees", get(list_item_employees).post(assign_employee))
        .route(
            "/{id}/employees/{emp_id}",
            patch(update_item_employee).delete(remove_item_employee),
        )
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Query parameters for listing calendar items, with optional month filter.
#[derive(Debug, Deserialize)]
struct ListQuery {
    /// Optional month filter in `YYYY-MM` format; returns all dates if omitted.
    month: Option<String>,
}

/// A single calendar item row as returned from the database.
#[derive(Debug, Serialize, FromRow)]
struct CalendarItemRow {
    id: Uuid,
    title: String,
    description: Option<String>,
    category: String,
    location: Option<String>,
    scheduled_date: Option<NaiveDate>,
    duration_hours: f64,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[sqlx(default)]
    customer_id: Option<Uuid>,
    #[sqlx(default)]
    customer_name: Option<String>,
}

/// Employee assignment record for a calendar item.
#[derive(Debug, Serialize, FromRow)]
struct CalendarItemEmployee {
    employee_id: Uuid,
    first_name: String,
    last_name: String,
    planned_hours: f64,
    actual_hours: Option<f64>,
    notes: Option<String>,
}

/// Full calendar item detail with the list of assigned employees.
#[derive(Debug, Serialize)]
struct CalendarItemDetail {
    id: Uuid,
    title: String,
    description: Option<String>,
    category: String,
    location: Option<String>,
    scheduled_date: Option<NaiveDate>,
    duration_hours: f64,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    customer_id: Option<Uuid>,
    customer_name: Option<String>,
    employees: Vec<CalendarItemEmployee>,
}

/// Body for creating a new calendar item.
#[derive(Debug, Deserialize)]
struct CreateItemBody {
    /// Required. Short descriptive title (e.g. "Fahrerschulung").
    title: String,
    description: Option<String>,
    category: Option<String>,
    location: Option<String>,
    scheduled_date: Option<NaiveDate>,
    duration_hours: Option<f64>,
    customer_id: Option<Uuid>,
}

/// Body for partially updating an existing calendar item.
#[derive(Debug, Deserialize)]
struct UpdateItemBody {
    title: Option<String>,
    description: Option<String>,
    category: Option<String>,
    location: Option<String>,
    scheduled_date: Option<NaiveDate>,
    duration_hours: Option<f64>,
    status: Option<String>,
    customer_id: Option<Uuid>,
    /// Set to true to explicitly remove the customer assignment.
    #[serde(default)]
    remove_customer: bool,
}

/// Body for assigning an employee to a calendar item.
#[derive(Debug, Deserialize)]
struct AssignEmployeeBody {
    /// UUID of the employee to assign.
    employee_id: Uuid,
    /// Number of hours this employee is planned to work on the item.
    planned_hours: f64,
}

/// Body for updating hours/notes on an existing employee assignment.
#[derive(Debug, Deserialize)]
struct UpdateEmployeeBody {
    planned_hours: Option<f64>,
    actual_hours: Option<f64>,
    notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper: parse "YYYY-MM" into an inclusive date range (first_day..=last_day)
// ---------------------------------------------------------------------------

/// Parse a `"YYYY-MM"` string into `(month_start, exclusive_end)` NaiveDate pair.
///
/// **Caller**: `list_items` handler.
/// **Why**: Reuses the same month-filter pattern used elsewhere in admin.rs.
///
/// # Returns
/// `Some((first, end_exclusive))` where `end_exclusive` is the first day of the next month,
/// or `None` when the string is malformed.
fn parse_month_bounds(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let parts: Vec<&str> = month.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let year: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let start = NaiveDate::from_ymd_opt(year, m, 1)?;
    let end = if m == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, m + 1, 1)?
    };
    Some((start, end))
}

// ---------------------------------------------------------------------------
// Helper: fetch single item row with customer join
// ---------------------------------------------------------------------------

/// Fetch a single `CalendarItemRow` by id, LEFT JOINing the customer name.
///
/// **Caller**: `create_item`, `get_item`, `update_item`
/// **Why**: RETURNING clauses cannot do JOINs in PostgreSQL; this centralises
/// the post-write SELECT with customer data.
///
/// # Errors
/// `404 Not Found` when no item with the given UUID exists.
async fn fetch_item_row(pool: &sqlx::PgPool, id: Uuid) -> Result<CalendarItemRow, ApiError> {
    sqlx::query_as(
        r#"
        SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
               ci.scheduled_date, ci.duration_hours::float8 AS duration_hours,
               ci.status, ci.created_at, ci.updated_at,
               ci.customer_id, c.name AS customer_name
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/admin/calendar-items` — List all calendar items, optionally filtered by month.
///
/// **Caller**: Admin calendar/scheduling page.
/// **Why**: Operators need to see internal events (training, maintenance) in the same
/// calendar view as moving jobs so they can plan employee capacity.
///
/// # Parameters
/// - `month` — optional `YYYY-MM` query param; when omitted all items are returned.
///
/// # Returns
/// `200 OK` with an array of `CalendarItemRow` ordered by `scheduled_date ASC NULLS LAST`.
async fn list_items(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<CalendarItemRow>>, ApiError> {
    let rows: Vec<CalendarItemRow> = if let Some(month) = query.month {
        let (start, end) = parse_month_bounds(&month)
            .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat (erwartet: YYYY-MM)".into()))?;

        sqlx::query_as(
            r#"
            SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
                   ci.scheduled_date, ci.duration_hours::float8 AS duration_hours,
                   ci.status, ci.created_at, ci.updated_at,
                   ci.customer_id, c.name AS customer_name
            FROM calendar_items ci
            LEFT JOIN customers c ON c.id = ci.customer_id
            WHERE ci.scheduled_date >= $1 AND ci.scheduled_date < $2
            ORDER BY ci.scheduled_date ASC NULLS LAST
            "#,
        )
        .bind(start)
        .bind(end)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as(
            r#"
            SELECT ci.id, ci.title, ci.description, ci.category, ci.location,
                   ci.scheduled_date, ci.duration_hours::float8 AS duration_hours,
                   ci.status, ci.created_at, ci.updated_at,
                   ci.customer_id, c.name AS customer_name
            FROM calendar_items ci
            LEFT JOIN customers c ON c.id = ci.customer_id
            ORDER BY ci.scheduled_date ASC NULLS LAST
            "#,
        )
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(rows))
}

/// `POST /api/v1/admin/calendar-items` — Create a new calendar item.
///
/// **Caller**: Admin calendar create form.
/// **Why**: Operators need to schedule non-job events (training days, vehicle inspections,
/// team meetings) and track employee time against them.
///
/// # Returns
/// `201 Created` with the new `CalendarItemRow`.
async fn create_item(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(body): Json<CreateItemBody>,
) -> Result<(StatusCode, Json<CalendarItemRow>), ApiError> {
    if body.title.trim().is_empty() {
        return Err(ApiError::Validation("Titel darf nicht leer sein".into()));
    }

    let category = body.category.unwrap_or_else(|| "internal".to_string());
    let duration_hours = body.duration_hours.unwrap_or(0.0);

    let (new_id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO calendar_items (title, description, category, location, scheduled_date, duration_hours, customer_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
    )
    .bind(body.title.trim())
    .bind(body.description)
    .bind(category)
    .bind(body.location)
    .bind(body.scheduled_date)
    .bind(duration_hours)
    .bind(body.customer_id)
    .fetch_one(&state.db)
    .await?;

    let row = fetch_item_row(&state.db, new_id).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// `GET /api/v1/admin/calendar-items/{id}` — Fetch full detail for a single calendar item.
///
/// **Caller**: Admin calendar item detail page.
/// **Why**: Returns the item's metadata plus the full list of assigned employees so the
/// operator can see and manage staffing in one request.
///
/// # Returns
/// `200 OK` with `CalendarItemDetail` (includes `employees` array).
///
/// # Errors
/// `404 Not Found` when no item with the given UUID exists.
async fn get_item(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<CalendarItemDetail>, ApiError> {
    let item = fetch_item_row(&state.db, id).await?;
    let employees = fetch_item_employees(&state.db, id).await?;

    Ok(Json(CalendarItemDetail {
        id: item.id,
        title: item.title,
        description: item.description,
        category: item.category,
        location: item.location,
        scheduled_date: item.scheduled_date,
        duration_hours: item.duration_hours,
        status: item.status,
        created_at: item.created_at,
        updated_at: item.updated_at,
        customer_id: item.customer_id,
        customer_name: item.customer_name,
        employees,
    }))
}

/// `PATCH /api/v1/admin/calendar-items/{id}` — Partially update a calendar item.
///
/// **Caller**: Admin calendar item edit form.
/// **Why**: Operators need to adjust dates, titles, status, or location after creation.
/// Only fields that are `Some` in the request body are updated; others remain unchanged.
///
/// # Returns
/// `200 OK` with the updated `CalendarItemRow`.
///
/// # Errors
/// `404 Not Found` when no item with the given UUID exists.
async fn update_item(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateItemBody>,
) -> Result<Json<CalendarItemRow>, ApiError> {
    // Verify the item exists before building the dynamic update
    let existing: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM calendar_items WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    existing.ok_or_else(|| ApiError::NotFound("Kalendereintrag nicht gefunden".into()))?;

    // Build dynamic SET clause
    let mut sets: Vec<String> = Vec::new();
    let mut idx = 1usize;

    if body.title.is_some() {
        sets.push(format!("title = ${idx}"));
        idx += 1;
    }
    if body.description.is_some() || body.description.as_ref().map(|_| true).is_none() {
        // description can be explicitly null — always allow it when key present
    }
    if body.category.is_some() {
        sets.push(format!("category = ${idx}"));
        idx += 1;
    }
    if body.location.is_some() {
        sets.push(format!("location = ${idx}"));
        idx += 1;
    }
    if body.scheduled_date.is_some() {
        sets.push(format!("scheduled_date = ${idx}"));
        idx += 1;
    }
    if body.duration_hours.is_some() {
        sets.push(format!("duration_hours = ${idx}"));
        idx += 1;
    }
    if body.status.is_some() {
        sets.push(format!("status = ${idx}"));
        idx += 1;
    }
    if body.remove_customer {
        sets.push(format!("customer_id = NULL"));
    } else if body.customer_id.is_some() {
        sets.push(format!("customer_id = ${idx}"));
        idx += 1;
    }

    // Always update updated_at
    sets.push(format!("updated_at = ${idx}"));

    if sets.len() == 1 {
        // Only updated_at would be set — nothing actually changed
        return Ok(Json(fetch_item_row(&state.db, id).await?));
    }

    let sql = format!(
        "UPDATE calendar_items SET {} WHERE id = ${}",
        sets.join(", "),
        idx + 1,
    );

    // Bind values in the same order as the SET clause was built
    let mut q = sqlx::query(&sql);
    if let Some(v) = body.title {
        q = q.bind(v);
    }
    if let Some(v) = body.category {
        q = q.bind(v);
    }
    if let Some(v) = body.location {
        q = q.bind(v);
    }
    if let Some(v) = body.scheduled_date {
        q = q.bind(v);
    }
    if let Some(v) = body.duration_hours {
        q = q.bind(v);
    }
    if let Some(v) = body.status {
        q = q.bind(v);
    }
    if !body.remove_customer {
        if let Some(v) = body.customer_id {
            q = q.bind(v);
        }
    }
    q = q.bind(Utc::now()); // updated_at
    q = q.bind(id);         // WHERE id

    q.execute(&state.db).await?;
    Ok(Json(fetch_item_row(&state.db, id).await?))
}

/// `DELETE /api/v1/admin/calendar-items/{id}` — Delete a calendar item and all its assignments.
///
/// **Caller**: Admin calendar item detail delete button.
/// **Why**: Removes cancelled or erroneous internal events. Cascade deletes
/// `calendar_item_employees` rows automatically via the FK constraint.
///
/// # Returns
/// `204 No Content` on success.
///
/// # Errors
/// `404 Not Found` when no item with the given UUID exists.
async fn delete_item(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM calendar_items WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Kalendereintrag nicht gefunden".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/admin/calendar-items/{id}/employees` — List employees assigned to a calendar item.
///
/// **Caller**: Admin calendar item detail employee section.
/// **Why**: Shows current staffing for the item without loading the full detail response.
///
/// # Returns
/// `200 OK` with an array of `CalendarItemEmployee`.
///
/// # Errors
/// `404 Not Found` when the item does not exist.
async fn list_item_employees(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<CalendarItemEmployee>>, ApiError> {
    // Verify item exists
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM calendar_items WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    exists.ok_or_else(|| ApiError::NotFound("Kalendereintrag nicht gefunden".into()))?;

    let employees = fetch_item_employees(&state.db, id).await?;
    Ok(Json(employees))
}

/// `POST /api/v1/admin/calendar-items/{id}/employees` — Assign an employee to a calendar item.
///
/// **Caller**: Admin calendar item detail assign form.
/// **Why**: Links an employee to an internal work item with planned hours, mirroring how
/// `inquiry_employees` works for moving jobs.
///
/// # Returns
/// `201 Created` with the updated employee list for the item.
///
/// # Errors
/// `404 Not Found` when the item or employee does not exist.
/// `409 Conflict` when the employee is already assigned.
async fn assign_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<AssignEmployeeBody>,
) -> Result<(StatusCode, Json<Vec<CalendarItemEmployee>>), ApiError> {
    // Verify item exists
    let item_exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM calendar_items WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    item_exists.ok_or_else(|| ApiError::NotFound("Kalendereintrag nicht gefunden".into()))?;

    // Verify employee exists
    let emp_exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM employees WHERE id = $1")
        .bind(body.employee_id)
        .fetch_optional(&state.db)
        .await?;
    emp_exists.ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    sqlx::query(
        r#"
        INSERT INTO calendar_item_employees (calendar_item_id, employee_id, planned_hours)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(id)
    .bind(body.employee_id)
    .bind(body.planned_hours)
    .execute(&state.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("calendar_item_employees_calendar_item_id_employee_id_key") {
                return ApiError::Conflict("Mitarbeiter ist bereits zugewiesen".into());
            }
        }
        ApiError::from(e)
    })?;

    let employees = fetch_item_employees(&state.db, id).await?;
    Ok((StatusCode::CREATED, Json(employees)))
}

/// `PATCH /api/v1/admin/calendar-items/{id}/employees/{emp_id}` — Update hours or notes on an assignment.
///
/// **Caller**: Admin calendar item detail employee row edit.
/// **Why**: Operators enter actual hours after the event, or correct planned hours before it.
///
/// # Returns
/// `200 OK` with the updated `CalendarItemEmployee` row.
///
/// # Errors
/// `404 Not Found` when the assignment does not exist.
async fn update_item_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateEmployeeBody>,
) -> Result<Json<CalendarItemEmployee>, ApiError> {
    // Build dynamic SET clause
    let mut sets: Vec<String> = Vec::new();
    let mut idx = 1usize;

    if body.planned_hours.is_some() {
        sets.push(format!("planned_hours = ${idx}"));
        idx += 1;
    }
    if body.actual_hours.is_some() {
        sets.push(format!("actual_hours = ${idx}"));
        idx += 1;
    }
    if body.notes.is_some() {
        sets.push(format!("notes = ${idx}"));
        idx += 1;
    }

    if sets.is_empty() {
        // Nothing to update — fetch and return current row
        let row: Option<CalendarItemEmployee> = sqlx::query_as(
            r#"
            SELECT cie.employee_id,
                   e.first_name,
                   e.last_name,
                   cie.planned_hours::float8 AS planned_hours,
                   cie.actual_hours::float8 AS actual_hours,
                   cie.notes
            FROM calendar_item_employees cie
            JOIN employees e ON e.id = cie.employee_id
            WHERE cie.calendar_item_id = $1 AND cie.employee_id = $2
            "#,
        )
        .bind(id)
        .bind(emp_id)
        .fetch_optional(&state.db)
        .await?;
        return row
            .map(Json)
            .ok_or_else(|| ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    let sql = format!(
        r#"
        UPDATE calendar_item_employees
        SET {}
        WHERE calendar_item_id = ${} AND employee_id = ${}
        RETURNING employee_id,
                  planned_hours::float8 AS planned_hours,
                  actual_hours::float8 AS actual_hours,
                  notes
        "#,
        sets.join(", "),
        idx,
        idx + 1,
    );

    #[derive(sqlx::FromRow)]
    struct UpdatedRow {
        employee_id: Uuid,
        planned_hours: f64,
        actual_hours: Option<f64>,
        notes: Option<String>,
    }

    let mut q = sqlx::query_as::<_, UpdatedRow>(&sql);
    if let Some(v) = body.planned_hours {
        q = q.bind(v);
    }
    if let Some(v) = body.actual_hours {
        q = q.bind(v);
    }
    if let Some(v) = body.notes {
        q = q.bind(v);
    }
    q = q.bind(id);
    q = q.bind(emp_id);

    let updated = q
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Zuweisung nicht gefunden".into()))?;

    // Fetch employee name fields separately
    let name: Option<(String, String)> =
        sqlx::query_as("SELECT first_name, last_name FROM employees WHERE id = $1")
            .bind(updated.employee_id)
            .fetch_optional(&state.db)
            .await?;

    let (first_name, last_name) = name.unwrap_or_default();

    Ok(Json(CalendarItemEmployee {
        employee_id: updated.employee_id,
        first_name,
        last_name,
        planned_hours: updated.planned_hours,
        actual_hours: updated.actual_hours,
        notes: updated.notes,
    }))
}

/// `DELETE /api/v1/admin/calendar-items/{id}/employees/{emp_id}` — Remove an employee assignment.
///
/// **Caller**: Admin calendar item detail employee row remove button.
/// **Why**: Operators need to unassign employees when plans change.
///
/// # Returns
/// `204 No Content` on success.
///
/// # Errors
/// `404 Not Found` when the assignment does not exist.
async fn remove_item_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query(
        "DELETE FROM calendar_item_employees WHERE calendar_item_id = $1 AND employee_id = $2",
    )
    .bind(id)
    .bind(emp_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

/// Fetch all employee assignments for a calendar item, joined with employee names.
///
/// **Caller**: `get_item`, `list_item_employees`, `assign_employee`.
/// **Why**: Centralises the join query so the result shape is consistent across all
/// handlers that need the employee list.
///
/// # Parameters
/// - `pool` — database connection pool
/// - `item_id` — UUID of the calendar item
///
/// # Returns
/// Ordered list of `CalendarItemEmployee` (by last_name, first_name).
async fn fetch_item_employees(
    pool: &sqlx::PgPool,
    item_id: Uuid,
) -> Result<Vec<CalendarItemEmployee>, ApiError> {
    let employees: Vec<CalendarItemEmployee> = sqlx::query_as(
        r#"
        SELECT cie.employee_id,
               e.first_name,
               e.last_name,
               cie.planned_hours::float8 AS planned_hours,
               cie.actual_hours::float8  AS actual_hours,
               cie.notes
        FROM calendar_item_employees cie
        JOIN employees e ON e.id = cie.employee_id
        WHERE cie.calendar_item_id = $1
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(item_id)
    .fetch_all(pool)
    .await?;

    Ok(employees)
}
