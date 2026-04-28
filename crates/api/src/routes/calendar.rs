use axum::{
    extract::{Path, Query, State},
    routing::{get, put},
    Json, Router,
};
use chrono::{Datelike, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::repositories::calendar_repo;
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/availability", get(check_availability))
        .route("/schedule", get(get_schedule))
        .route("/capacity/{date}", put(set_capacity))
}


// ── Response types ────────────────────────────────────────────────────────────

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
    employee_notes: Option<String>,
    offer_price_cents: Option<i64>,
    start_time: NaiveTime,
    end_time: NaiveTime,
    employees_assigned: i64,
    employee_names: Option<String>,
    day_number: i32,
    total_days: i32,
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
    day_number: i32,
    total_days: i32,
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

#[derive(Debug, Serialize)]
pub struct EmployeeAssignmentResponse {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub job_date: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<NaiveTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<NaiveTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_in: Option<NaiveTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_out: Option<NaiveTime>,
    pub break_minutes: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_hours: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct EmployeeAssignmentBody {
    pub employee_id: Uuid,
    pub job_date: NaiveDate,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub start_time: Option<NaiveTime>,
    #[serde(default)]
    pub end_time: Option<NaiveTime>,
    #[serde(default)]
    pub clock_in: Option<NaiveTime>,
    #[serde(default)]
    pub clock_out: Option<NaiveTime>,
    #[serde(default)]
    pub break_minutes: i32,
    #[serde(default)]
    pub actual_hours: Option<f64>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn row_to_assignment_response(r: calendar_repo::EmployeeAssignmentRow) -> EmployeeAssignmentResponse {
    EmployeeAssignmentResponse {
        employee_id: r.employee_id,
        first_name: r.first_name,
        last_name: r.last_name,
        job_date: r.job_date,
        notes: r.notes,
        start_time: r.start_time,
        end_time: r.end_time,
        clock_in: r.clock_in,
        clock_out: r.clock_out,
        break_minutes: r.break_minutes,
        actual_hours: r.actual_hours,
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    date: NaiveDate,
}

/// `GET /api/v1/calendar/availability?date=YYYY-MM-DD`
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

/// `GET /api/v1/calendar/schedule?from=YYYY-MM-DD&to=YYYY-MM-DD`
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

    let inquiry_rows = calendar_repo::fetch_schedule_inquiries(&state.db, query.from, query.to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

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

    let override_rows = calendar_repo::fetch_capacity_overrides_range(&state.db, query.from, query.to)
        .await
        .unwrap_or_default();
    let override_map: HashMap<NaiveDate, i32> = override_rows.into_iter().collect();

    let mut inquiry_map: HashMap<NaiveDate, Vec<ScheduleInquiry>> = HashMap::new();
    for r in inquiry_rows {
        let price = price_map.get(&r.inquiry_id).copied();
        inquiry_map.entry(r.effective_date).or_default().push(ScheduleInquiry {
            inquiry_id: r.inquiry_id,
            customer_name: r.customer_name,
            customer_type: r.customer_type,
            company_name: r.company_name,
            departure_address: r.departure_address,
            arrival_address: r.arrival_address,
            volume_m3: r.volume_m3,
            status: r.status,
            notes: r.notes,
            employee_notes: r.employee_notes,
            offer_price_cents: price,
            start_time: r.start_time,
            end_time: r.end_time,
            employees_assigned: r.employees_assigned,
            employee_names: r.employee_names,
            day_number: r.day_number,
            total_days: r.total_days,
            service_type: r.service_type,
            scheduled_date: r.scheduled_date,
        });
    }

    let cal_item_rows = calendar_repo::fetch_schedule_calendar_items(&state.db, query.from, query.to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

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
            scheduled_date: r.scheduled_date,
        });
    }

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

/// `PUT /api/v1/calendar/capacity/{date}`
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

