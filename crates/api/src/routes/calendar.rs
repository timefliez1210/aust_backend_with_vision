use axum::{
    extract::{Path, Query, State},
    routing::{get, put},
    Extension, Json, Router,
};
use chrono::{Datelike, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
use crate::repositories::{calendar_item_repo, calendar_repo, inquiry_repo};
use crate::{ApiError, AppState};

/// Register the calendar routes.
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Exposes availability checking, schedule overview, and daily capacity
/// overrides. Data comes directly from `inquiries` — no separate bookings table.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/availability", get(check_availability))
        .route("/schedule", get(get_schedule))
        .route("/capacity/{date}", put(set_capacity))
}

/// Sub-router for `/{id}/days` merged into the protected inquiries router.
///
/// **Caller**: `inquiries::router()`
/// **Why**: Day management lives logically next to its parent resource but
/// the handlers share calendar module types and DB helpers.
pub fn inquiry_days_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{id}/days", get(get_inquiry_days).put(put_inquiry_days))
}

/// Sub-router for `/{id}/days` merged into the calendar_items router.
///
/// **Caller**: `calendar_items::router()`
/// **Why**: Mirrors the inquiry days sub-router for calendar items.
pub fn calendar_item_days_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{id}/days", get(get_calendar_item_days).put(put_calendar_item_days))
}

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct DateAvailability {
    date: NaiveDate,
    available: bool,
    capacity: i32,
    booked: i32,
    remaining: i32,
}

#[derive(Debug, Serialize)]
struct AvailabilityResult {
    requested_date: NaiveDate,
    requested_date_available: bool,
    requested_date_info: DateAvailability,
    alternatives: Vec<DateAvailability>,
}

#[derive(Debug, Serialize)]
struct ScheduleInquiry {
    inquiry_id: Uuid,
    customer_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    customer_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    company_name: Option<String>,
    departure_address: Option<String>,
    arrival_address: Option<String>,
    volume_m3: Option<f64>,
    status: String,
    notes: Option<String>,
    offer_price_cents: Option<i64>,
    start_time: NaiveTime,
    end_time: NaiveTime,
    employees_assigned: i64,
    employee_names: Option<String>,
    /// Which day number this entry represents (None for single-day inquiries).
    #[serde(skip_serializing_if = "Option::is_none")]
    day_number: Option<i16>,
    /// Total number of days for this inquiry (None for single-day).
    #[serde(skip_serializing_if = "Option::is_none")]
    total_days: Option<i16>,
    /// Per-day notes from `inquiry_days` (None for single-day).
    #[serde(skip_serializing_if = "Option::is_none")]
    day_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_type: Option<String>,
    scheduled_date: NaiveDate,
}

#[derive(Debug, Serialize)]
struct ScheduleCalendarItem {
    calendar_item_id: Uuid,
    title: String,
    category: String,
    location: Option<String>,
    start_time: NaiveTime,
    end_time: Option<NaiveTime>,
    employees_assigned: i64,
    employee_names: Option<String>,
    /// Which day number this entry represents (None for single-day items).
    #[serde(skip_serializing_if = "Option::is_none")]
    day_number: Option<i16>,
    /// Total number of days for this item (None for single-day).
    #[serde(skip_serializing_if = "Option::is_none")]
    total_days: Option<i16>,
    /// Per-day notes from `calendar_item_days` (None for single-day).
    #[serde(skip_serializing_if = "Option::is_none")]
    day_notes: Option<String>,
    scheduled_date: NaiveDate,
}

#[derive(Debug, Serialize)]
struct ScheduleEntry {
    date: NaiveDate,
    available: bool,
    capacity: i32,
    booked: i32,
    remaining: i32,
    inquiries: Vec<ScheduleInquiry>,
    calendar_items: Vec<ScheduleCalendarItem>,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct CapacityOverride {
    pub override_date: NaiveDate,
    pub capacity: i32,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn count_active_on_date(pool: &sqlx::PgPool, date: NaiveDate) -> Result<i32, sqlx::Error> {
    Ok(calendar_repo::count_active_on_date(pool, date).await? as i32)
}

async fn effective_capacity(
    pool: &sqlx::PgPool,
    date: NaiveDate,
    default: i32,
) -> Result<i32, sqlx::Error> {
    Ok(calendar_repo::fetch_capacity_override(pool, date).await?.unwrap_or(default))
}

async fn build_date_availability(
    pool: &sqlx::PgPool,
    date: NaiveDate,
    default_capacity: i32,
) -> Result<DateAvailability, sqlx::Error> {
    let capacity = effective_capacity(pool, date, default_capacity).await?;
    let booked = count_active_on_date(pool, date).await?;
    let remaining = (capacity - booked).max(0);
    Ok(DateAvailability { date, available: remaining > 0, capacity, booked, remaining })
}

async fn find_nearest_available(
    pool: &sqlx::PgPool,
    around: NaiveDate,
    default_capacity: i32,
    count: usize,
    search_window_days: i64,
) -> Result<Vec<DateAvailability>, sqlx::Error> {
    let today = chrono::Utc::now().date_naive();
    let mut results = Vec::new();
    let mut offset = 1i64;

    while results.len() < count && offset <= search_window_days {
        let future = around + chrono::Days::new(offset as u64);
        if future.weekday() != chrono::Weekday::Sun {
            let avail = build_date_availability(pool, future, default_capacity).await?;
            if avail.available {
                results.push(avail);
                if results.len() >= count {
                    break;
                }
            }
        }
        let past = around - chrono::Days::new(offset as u64);
        if past >= today && past.weekday() != chrono::Weekday::Sun {
            let avail = build_date_availability(pool, past, default_capacity).await?;
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

// ── Handlers ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    date: NaiveDate,
}

/// `GET /api/v1/calendar/availability?date=YYYY-MM-DD` — Check availability for a date.
///
/// Counts active inquiries on that date and compares against effective capacity
/// (override or default). Returns alternatives when the date is full.
async fn check_availability(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AvailabilityQuery>,
) -> Result<Json<AvailabilityResult>, ApiError> {
    let default_capacity = state.config.calendar.default_capacity;
    let alternatives_count = state.config.calendar.alternatives_count;
    let search_window = state.config.calendar.search_window_days;

    let info = build_date_availability(&state.db, query.date, default_capacity)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let available = info.available;
    let alternatives = if !available {
        find_nearest_available(&state.db, query.date, default_capacity, alternatives_count, search_window)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
    } else {
        Vec::new()
    };

    Ok(Json(AvailabilityResult {
        requested_date: query.date,
        requested_date_available: available,
        requested_date_info: info,
        alternatives,
    }))
}

#[derive(Debug, Deserialize)]
struct ScheduleQuery {
    from: NaiveDate,
    to: NaiveDate,
}

/// `GET /api/v1/calendar/schedule?from=YYYY-MM-DD&to=YYYY-MM-DD` — Fetch schedule for a date range.
///
/// Returns one entry per day with active inquiries and availability info.
/// Inquiries are joined with customers and addresses for display. Max 90-day range.
async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScheduleQuery>,
) -> Result<Json<Vec<ScheduleEntry>>, ApiError> {
    if query.from > query.to {
        return Err(ApiError::BadRequest("'from' must be before or equal to 'to'".into()));
    }
    if (query.to - query.from).num_days() > 90 {
        return Err(ApiError::BadRequest("Date range must not exceed 90 days".into()));
    }

    let default_capacity = state.config.calendar.default_capacity;

    // Fetch all active inquiries in range — single-day and multi-day via UNION ALL.
    let inquiry_rows = calendar_repo::fetch_schedule_inquiries(&state.db, query.from, query.to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Fetch offer prices for all inquiry_ids in one query
    let inquiry_ids: Vec<Uuid> = inquiry_rows.iter().map(|r| r.inquiry_id).collect();
    let mut price_map: HashMap<Uuid, i64> = HashMap::new();
    if !inquiry_ids.is_empty() {
        let price_rows = calendar_repo::fetch_offer_prices(&state.db, &inquiry_ids)
            .await
            .unwrap_or_default();
        for (id, price) in price_rows {
            price_map.entry(id).or_insert(price);
        }
    }

    // Fetch capacity overrides for range
    let override_rows = calendar_repo::fetch_capacity_overrides_range(&state.db, query.from, query.to)
        .await
        .unwrap_or_default();
    let override_map: HashMap<NaiveDate, i32> = override_rows.into_iter().collect();

    // Group inquiries by date
    let mut inquiry_map: HashMap<NaiveDate, Vec<ScheduleInquiry>> = HashMap::new();
    for r in inquiry_rows {
        let price = price_map.get(&r.inquiry_id).copied();
        inquiry_map.entry(r.effective_date).or_default().push(ScheduleInquiry {
            inquiry_id: r.inquiry_id,
            customer_name: r.customer_name, customer_type: r.customer_type, company_name: r.company_name,
            departure_address: r.departure_address,
            arrival_address: r.arrival_address,
            volume_m3: r.volume_m3,
            status: r.status,
            notes: r.notes,
            offer_price_cents: price,
            start_time: r.start_time,
            end_time: r.end_time,
            employees_assigned: r.employees_assigned,
            employee_names: r.employee_names,
            day_number: r.day_number,
            total_days: r.total_days,
            day_notes: r.day_notes,
            service_type: r.service_type,
            scheduled_date: r.scheduled_date,
        });
    }

    // Fetch calendar item schedule rows (per-day, same model as inquiries)
    let cal_item_rows = calendar_repo::fetch_schedule_calendar_items(&state.db, query.from, query.to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Group calendar items by date
    let mut cal_item_map: HashMap<NaiveDate, Vec<ScheduleCalendarItem>> = HashMap::new();
    for r in cal_item_rows {
        cal_item_map.entry(r.effective_date).or_default().push(ScheduleCalendarItem {
            calendar_item_id: r.calendar_item_id,
            title: r.title,
            category: r.category,
            location: r.location,
            start_time: r.start_time,
            end_time: r.end_time,
            employees_assigned: r.employees_assigned,
            employee_names: r.employee_names,
            day_number: r.day_number,
            total_days: r.total_days,
            day_notes: r.day_notes,
            scheduled_date: r.scheduled_date,
        });
    }

    // Build one entry per day in the range
    let mut entries = Vec::new();
    let mut current = query.from;
    while current <= query.to {
        let capacity = override_map.get(&current).copied().unwrap_or(default_capacity);
        let day_inquiries = inquiry_map.remove(&current).unwrap_or_default();
        let day_cal_items = cal_item_map.remove(&current).unwrap_or_default();
        let booked = day_inquiries.len() as i32;
        let remaining = (capacity - booked).max(0);

        entries.push(ScheduleEntry {
            date: current,
            available: remaining > 0,
            capacity,
            booked,
            remaining,
            inquiries: day_inquiries,
            calendar_items: day_cal_items,
        });
        current = current.succ_opt().unwrap();
    }

    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
struct SetCapacityRequest {
    capacity: i32,
}

/// `PUT /api/v1/calendar/capacity/{date}` — Override daily capacity for a specific date.
///
/// Setting capacity to 0 blocks the date. Higher than default allows extra bookings.
async fn set_capacity(
    State(state): State<Arc<AppState>>,
    Path(date): Path<NaiveDate>,
    Json(request): Json<SetCapacityRequest>,
) -> Result<Json<CapacityOverride>, ApiError> {
    if request.capacity < 0 {
        return Err(ApiError::BadRequest("Capacity must be >= 0".into()));
    }

    calendar_repo::upsert_capacity(&state.db, date, request.capacity)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(CapacityOverride { override_date: date, capacity: request.capacity }))
}

// ── Day management ────────────────────────────────────────────────────────────

/// One employee assignment within a day, as received from the client.
#[derive(Debug, Deserialize)]
struct DayEmployeeInput {
    employee_id: Uuid,
    #[serde(default)]
    planned_hours: Option<f64>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    start_time: Option<NaiveTime>,
    #[serde(default)]
    end_time: Option<NaiveTime>,
}

/// One employee assignment within a day, as returned to the client.
#[derive(Debug, Serialize, Clone)]
struct DayEmployeeResponse {
    employee_id: Uuid,
    first_name: String,
    last_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    planned_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<NaiveTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<NaiveTime>,
}

/// Input body for one day within a PUT /days request.
#[derive(Debug, Deserialize)]
struct DayInput {
    day_date: NaiveDate,
    day_number: i16,
    #[serde(default)]
    notes: Option<String>,
    /// Per-day start time; overrides the parent's start_time when set.
    #[serde(default)]
    start_time: Option<NaiveTime>,
    /// Per-day end time; overrides the parent's end_time when set.
    #[serde(default)]
    end_time: Option<NaiveTime>,
    /// Employee assignments for this specific day.
    #[serde(default)]
    employees: Vec<DayEmployeeInput>,
}

/// Response shape for one day, returned by GET and PUT /days.
#[derive(Debug, Serialize)]
struct DayResponse {
    day_date: NaiveDate,
    day_number: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<NaiveTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<NaiveTime>,
    /// Employees assigned to this specific day (empty array = none assigned).
    employees: Vec<DayEmployeeResponse>,
}

/// `GET /api/v1/inquiries/{id}/days` — List all scheduled days for a multi-day inquiry.
///
/// **Caller**: Calendar frontend, inquiry detail page.
/// **Why**: Returns the explicit day list with per-day times and employees so the UI
/// can render multi-day inquiries and the admin can inspect or edit the schedule.
async fn get_inquiry_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    let days = calendar_repo::fetch_inquiry_days(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let emp_rows = calendar_repo::fetch_inquiry_day_employees(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(build_day_responses(days.into_iter().map(|r| (r.id, r.day_date, r.day_number, r.notes, r.start_time, r.end_time)).collect(), emp_rows)))
}

/// `PUT /api/v1/inquiries/{id}/days` — Replace the full day list for an inquiry.
///
/// **Caller**: Calendar frontend when admin sets or edits multi-day schedule.
/// **Why**: Full-replace semantics keep the client in control — no partial-update
/// edge cases. Empty `days` array converts the inquiry back to single-day.
/// Each day may carry its own `start_time`, `end_time`, and `employees` list.
///
/// # Errors
/// Returns 400 if any `day_number` values are duplicated or < 1.
async fn put_inquiry_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutDaysRequest>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    validate_day_numbers(&body.days)?;

    let mut tx = state.db.begin().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    calendar_repo::delete_inquiry_days(&mut tx, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut inserted: Vec<(Uuid, NaiveDate, i16, Option<String>, Option<NaiveTime>, Option<NaiveTime>)> = Vec::new();
    let mut emp_map: HashMap<Uuid, Vec<DayEmployeeResponse>> = HashMap::new();

    for d in &body.days {
        let day_id = calendar_repo::insert_inquiry_day(
            &mut tx, id, d.day_date, d.day_number, d.notes.as_deref(), d.start_time, d.end_time,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        for emp in &d.employees {
            calendar_repo::insert_inquiry_day_employee(
                &mut tx, day_id, emp.employee_id, emp.planned_hours, emp.notes.as_deref(),
                emp.start_time, emp.end_time,
            )
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        inserted.push((day_id, d.day_date, d.day_number, d.notes.clone(), d.start_time, d.end_time));
        emp_map.insert(day_id, Vec::new()); // placeholder; names fetched below
    }

    tx.commit().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    // Sync flat inquiry_employees table from day-level data
    // (keeps the flat table in sync for the offer builder and other flat-table reads)
    if let Err(e) = inquiry_repo::sync_flat_inquiry_employees(&state.db, id).await {
        tracing::error!(inquiry_id = %id, error = %e, "Failed to sync flat inquiry_employees after put_inquiry_days");
        // Day-level data is correct; flat table may be stale until next sync
    }

    // Re-fetch employee names for the response (we only have IDs from the request)
    let emp_rows = calendar_repo::fetch_inquiry_day_employees(&state.db, id)
        .await
        .unwrap_or_default();

    Ok(Json(build_day_responses(inserted, emp_rows)))
}

/// `GET /api/v1/calendar-items/{id}/days` — List all scheduled days for a multi-day calendar item.
///
/// **Caller**: Calendar frontend, Termine detail view.
/// **Why**: Mirrors the inquiry days endpoint for calendar items (Termine).
async fn get_calendar_item_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    let days = calendar_repo::fetch_calendar_item_days(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let emp_rows = calendar_repo::fetch_calendar_item_day_employees(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(build_day_responses(days.into_iter().map(|r| (r.id, r.day_date, r.day_number, r.notes, r.start_time, r.end_time)).collect(), emp_rows)))
}

/// `PUT /api/v1/calendar-items/{id}/days` — Replace the full day list for a calendar item.
///
/// **Caller**: Calendar frontend when admin edits multi-day Termin.
/// **Why**: Full-replace semantics — empty `days` converts back to single-day.
/// Each day may carry its own `start_time`, `end_time`, and `employees` list.
///
/// # Errors
/// Returns 400 if any `day_number` values are duplicated or < 1.
async fn put_calendar_item_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutDaysRequest>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    validate_day_numbers(&body.days)?;

    let mut tx = state.db.begin().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    calendar_repo::delete_calendar_item_days(&mut tx, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut inserted: Vec<(Uuid, NaiveDate, i16, Option<String>, Option<NaiveTime>, Option<NaiveTime>)> = Vec::new();

    for d in &body.days {
        let day_id = calendar_repo::insert_calendar_item_day(
            &mut tx, id, d.day_date, d.day_number, d.notes.as_deref(), d.start_time, d.end_time,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        for emp in &d.employees {
            calendar_repo::insert_calendar_item_day_employee(
                &mut tx, day_id, emp.employee_id, emp.planned_hours, emp.notes.as_deref(),
                emp.start_time, emp.end_time,
            )
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        }

        inserted.push((day_id, d.day_date, d.day_number, d.notes.clone(), d.start_time, d.end_time));
    }

    tx.commit().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    // Sync flat calendar_item_employees table from day-level data
    if let Err(e) = calendar_item_repo::sync_flat_calendar_item_employees(&state.db, id).await {
        tracing::error!(calendar_item_id = %id, error = %e, "Failed to sync flat calendar_item_employees after put_calendar_item_days");
    }

    let emp_rows = calendar_repo::fetch_calendar_item_day_employees(&state.db, id)
        .await
        .unwrap_or_default();

    Ok(Json(build_day_responses(inserted, emp_rows)))
}

// ── Shared helpers ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PutDaysRequest {
    days: Vec<DayInput>,
}

/// Validate that all `day_number` values are >= 1 and unique.
///
/// **Caller**: `put_inquiry_days`, `put_calendar_item_days`
/// **Why**: Enforces the constraint before touching the database.
fn validate_day_numbers(days: &[DayInput]) -> Result<(), ApiError> {
    let mut seen = std::collections::HashSet::new();
    for d in days {
        if d.day_number < 1 {
            return Err(ApiError::BadRequest("day_number must be >= 1".into()));
        }
        if !seen.insert(d.day_number) {
            return Err(ApiError::BadRequest(format!("duplicate day_number: {}", d.day_number)));
        }
    }
    Ok(())
}

/// Build a `Vec<DayResponse>` by merging day rows with employee rows.
///
/// **Caller**: `get_inquiry_days`, `put_inquiry_days`, `get_calendar_item_days`, `put_calendar_item_days`
/// **Why**: Centralises the merge logic — both GET and PUT return the same shape.
///
/// # Parameters
/// - `days` — `(id, day_date, day_number, notes, start_time, end_time)` tuples ordered by day_number
/// - `emp_rows` — flat list of employee rows from across all days (keyed by `day_id`)
fn build_day_responses(
    days: Vec<(Uuid, NaiveDate, i16, Option<String>, Option<NaiveTime>, Option<NaiveTime>)>,
    emp_rows: Vec<calendar_repo::DayEmployeeRow>,
) -> Vec<DayResponse> {
    // Group employees by day_id
    let mut emp_by_day: HashMap<Uuid, Vec<DayEmployeeResponse>> = HashMap::new();
    for e in emp_rows {
        emp_by_day.entry(e.day_id).or_default().push(DayEmployeeResponse {
            employee_id: e.employee_id,
            first_name: e.first_name,
            last_name: e.last_name,
            planned_hours: e.planned_hours,
            notes: e.notes,
            start_time: e.start_time,
            end_time: e.end_time,
        });
    }

    days.into_iter().map(|(id, day_date, day_number, notes, start_time, end_time)| {
        DayResponse {
            day_date,
            day_number,
            notes,
            start_time,
            end_time,
            employees: emp_by_day.remove(&id).unwrap_or_default(),
        }
    }).collect()
}
