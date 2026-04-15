use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
use crate::repositories::calendar_item_repo;
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
        .merge(super::calendar::calendar_item_days_router())
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

/// Full calendar item detail with the list of assigned employees.
#[derive(Debug, Serialize)]
struct CalendarItemDetail {
    id: Uuid,
    title: String,
    description: Option<String>,
    category: String,
    location: Option<String>,
    scheduled_date: Option<NaiveDate>,
    start_time: NaiveTime,
    end_time: Option<NaiveTime>,
    duration_hours: f64,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    customer_id: Option<Uuid>,
    customer_name: Option<String>,
    employees: Vec<calendar_item_repo::CalendarItemEmployee>,
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
    /// Required. Start time in HH:MM:SS format (e.g. "09:00:00").
    start_time: NaiveTime,
    /// Optional end time in HH:MM:SS format.
    end_time: Option<NaiveTime>,
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
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
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
    clock_in: Option<NaiveTime>,
    clock_out: Option<NaiveTime>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    break_minutes: Option<i32>,
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
) -> Result<Json<Vec<calendar_item_repo::CalendarItemRow>>, ApiError> {
    let rows = if let Some(month) = query.month {
        let (start, end) = parse_month_bounds(&month)
            .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat (erwartet: YYYY-MM)".into()))?;

        calendar_item_repo::list_items_by_month(&state.db, start, end).await?
    } else {
        calendar_item_repo::list_items_all(&state.db).await?
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
) -> Result<(StatusCode, Json<calendar_item_repo::CalendarItemRow>), ApiError> {
    if body.title.trim().is_empty() {
        return Err(ApiError::Validation("Titel darf nicht leer sein".into()));
    }

    let category = body.category.unwrap_or_else(|| "intern".to_string());
    let duration_hours = body.duration_hours.unwrap_or(0.0);

    let new_id = calendar_item_repo::insert_item(
        &state.db,
        body.title.trim(),
        body.description.as_deref(),
        &category,
        body.location.as_deref(),
        body.scheduled_date,
        body.start_time,
        body.end_time,
        duration_hours,
        body.customer_id,
    ).await?;

    let row = calendar_item_repo::fetch_item_row(&state.db, new_id).await?;
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
    let item = calendar_item_repo::fetch_item_row(&state.db, id).await?;
    let employees = calendar_item_repo::fetch_item_employees(&state.db, id).await?;

    Ok(Json(CalendarItemDetail {
        id: item.id,
        title: item.title,
        description: item.description,
        category: item.category,
        location: item.location,
        scheduled_date: item.scheduled_date,
        start_time: item.start_time,
        end_time: item.end_time,
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
) -> Result<Json<calendar_item_repo::CalendarItemRow>, ApiError> {
    // Verify the item exists before building the dynamic update
    if !calendar_item_repo::item_exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Kalendereintrag nicht gefunden".into()));
    }

    // Build dynamic SET clause — kept inline because SQL is dynamically constructed
    let mut sets: Vec<String> = Vec::new();
    let mut idx = 1usize;

    if body.title.is_some() {
        sets.push(format!("title = ${idx}"));
        idx += 1;
    }
    if body.description.is_some() {
        sets.push(format!("description = ${idx}"));
        idx += 1;
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
    if body.start_time.is_some() {
        sets.push(format!("start_time = ${idx}"));
        idx += 1;
    }
    if body.end_time.is_some() {
        sets.push(format!("end_time = ${idx}"));
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
        return Ok(Json(calendar_item_repo::fetch_item_row(&state.db, id).await?));
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
    if let Some(v) = body.description {
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
    if let Some(v) = body.start_time {
        q = q.bind(v);
    }
    if let Some(v) = body.end_time {
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
    Ok(Json(calendar_item_repo::fetch_item_row(&state.db, id).await?))
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
    let rows = calendar_item_repo::delete_item(&state.db, id).await?;
    if rows == 0 {
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
) -> Result<Json<Vec<calendar_item_repo::CalendarItemEmployee>>, ApiError> {
    // Verify item exists
    if !calendar_item_repo::item_exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Kalendereintrag nicht gefunden".into()));
    }

    let employees = calendar_item_repo::fetch_item_employees(&state.db, id).await?;
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
) -> Result<(StatusCode, Json<Vec<calendar_item_repo::CalendarItemEmployee>>), ApiError> {
    // Verify item exists
    if !calendar_item_repo::item_exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Kalendereintrag nicht gefunden".into()));
    }

    // Verify employee exists
    if !calendar_item_repo::employee_exists(&state.db, body.employee_id).await? {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into()));
    }

    calendar_item_repo::insert_item_employee(&state.db, id, body.employee_id, body.planned_hours)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("calendar_item_employees_calendar_item_id_employee_id_key") {
                    return ApiError::Conflict("Mitarbeiter ist bereits zugewiesen".into());
                }
            }
            ApiError::from(e)
        })?;

    let employees = calendar_item_repo::fetch_item_employees(&state.db, id).await?;
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
) -> Result<Json<calendar_item_repo::CalendarItemEmployee>, ApiError> {
    let rows = calendar_item_repo::update_item_employee(
        &state.db,
        id,
        emp_id,
        body.planned_hours,
        body.clock_in,
        body.clock_out,
        body.start_time,
        body.end_time,
        body.break_minutes,
        body.actual_hours,
        body.notes.as_deref(),
    ).await?;

    if rows == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    calendar_item_repo::fetch_item_employee(&state.db, id, emp_id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("Zuweisung nicht gefunden".into()))
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
    let rows = calendar_item_repo::delete_item_employee(&state.db, id, emp_id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}
