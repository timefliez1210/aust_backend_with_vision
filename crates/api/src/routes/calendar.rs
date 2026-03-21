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
}

#[derive(Debug, Serialize)]
struct ScheduleEntry {
    date: NaiveDate,
    available: bool,
    capacity: i32,
    booked: i32,
    remaining: i32,
    inquiries: Vec<ScheduleInquiry>,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct CapacityOverride {
    pub override_date: NaiveDate,
    pub capacity: i32,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn count_active_on_date(pool: &sqlx::PgPool, date: NaiveDate) -> Result<i32, sqlx::Error> {
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

async fn effective_capacity(
    pool: &sqlx::PgPool,
    date: NaiveDate,
    default: i32,
) -> Result<i32, sqlx::Error> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT capacity FROM calendar_capacity_overrides WHERE override_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(c,)| c).unwrap_or(default))
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
    // Single-day: no rows in inquiry_days, use scheduled_date / preferred_date.
    // Multi-day: one row per day from inquiry_days.
    #[derive(FromRow)]
    struct InquiryRow {
        effective_date: NaiveDate,
        inquiry_id: Uuid,
        customer_name: Option<String>,
        departure_address: Option<String>,
        arrival_address: Option<String>,
        volume_m3: Option<f64>,
        status: String,
        notes: Option<String>,
        start_time: NaiveTime,
        end_time: NaiveTime,
        employees_assigned: i64,
        employee_names: Option<String>,
        day_number: Option<i16>,
        total_days: Option<i16>,
        day_notes: Option<String>,
    }

    let inquiry_rows: Vec<InquiryRow> = sqlx::query_as(
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
    .bind(query.from)
    .bind(query.to)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Fetch offer prices for all inquiry_ids in one query
    let inquiry_ids: Vec<Uuid> = inquiry_rows.iter().map(|r| r.inquiry_id).collect();
    let mut price_map: HashMap<Uuid, i64> = HashMap::new();
    if !inquiry_ids.is_empty() {
        let price_rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT inquiry_id, price_cents FROM offers WHERE inquiry_id = ANY($1) AND status != 'rejected' ORDER BY created_at DESC",
        )
        .bind(&inquiry_ids)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        for (id, price) in price_rows {
            price_map.entry(id).or_insert(price);
        }
    }

    // Fetch capacity overrides for range
    let override_rows: Vec<(NaiveDate, i32)> = sqlx::query_as(
        "SELECT override_date, capacity FROM calendar_capacity_overrides WHERE override_date BETWEEN $1 AND $2",
    )
    .bind(query.from)
    .bind(query.to)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let override_map: HashMap<NaiveDate, i32> = override_rows.into_iter().collect();

    // Group inquiries by date
    let mut inquiry_map: HashMap<NaiveDate, Vec<ScheduleInquiry>> = HashMap::new();
    for row in inquiry_rows {
        let price = price_map.get(&row.inquiry_id).copied();
        inquiry_map.entry(row.effective_date).or_default().push(ScheduleInquiry {
            inquiry_id: row.inquiry_id,
            customer_name: row.customer_name,
            departure_address: row.departure_address,
            arrival_address: row.arrival_address,
            volume_m3: row.volume_m3,
            status: row.status,
            notes: row.notes,
            offer_price_cents: price,
            start_time: row.start_time,
            end_time: row.end_time,
            employees_assigned: row.employees_assigned,
            employee_names: row.employee_names,
            day_number: row.day_number,
            total_days: row.total_days,
            day_notes: row.day_notes,
        });
    }

    // Build one entry per day in the range
    let mut entries = Vec::new();
    let mut current = query.from;
    while current <= query.to {
        let capacity = override_map.get(&current).copied().unwrap_or(default_capacity);
        let day_inquiries = inquiry_map.remove(&current).unwrap_or_default();
        let booked = day_inquiries.len() as i32;
        let remaining = (capacity - booked).max(0);

        entries.push(ScheduleEntry {
            date: current,
            available: remaining > 0,
            capacity,
            booked,
            remaining,
            inquiries: day_inquiries,
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

    sqlx::query(
        r#"
        INSERT INTO calendar_capacity_overrides (id, override_date, capacity, created_at)
        VALUES (gen_random_uuid(), $1, $2, NOW())
        ON CONFLICT (override_date) DO UPDATE SET capacity = EXCLUDED.capacity
        "#,
    )
    .bind(date)
    .bind(request.capacity)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(CapacityOverride { override_date: date, capacity: request.capacity }))
}

// ── Day management ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DayInput {
    day_date: NaiveDate,
    day_number: i16,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PutDaysRequest {
    days: Vec<DayInput>,
}

#[derive(Debug, Serialize)]
struct DayResponse {
    day_date: NaiveDate,
    day_number: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

/// `GET /api/v1/inquiries/{id}/days` — List all scheduled days for a multi-day inquiry.
///
/// **Caller**: Calendar frontend, inquiry detail page.
/// **Why**: Returns the explicit day list so the UI can render multi-day inquiries
/// and the admin can inspect or edit the schedule.
async fn get_inquiry_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    let rows: Vec<(NaiveDate, i16, Option<String>)> = sqlx::query_as(
        "SELECT day_date, day_number, notes FROM inquiry_days WHERE inquiry_id = $1 ORDER BY day_number",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(rows.into_iter().map(|(day_date, day_number, notes)| DayResponse { day_date, day_number, notes }).collect()))
}

/// `PUT /api/v1/inquiries/{id}/days` — Replace the full day list for an inquiry.
///
/// **Caller**: Calendar frontend when admin sets or edits multi-day schedule.
/// **Why**: Full-replace semantics keep the client in control — no partial-update
/// edge cases. Empty `days` array converts the inquiry back to single-day.
///
/// # Errors
/// Returns 400 if any `day_number` values are duplicated or < 1.
async fn put_inquiry_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutDaysRequest>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    // Validate day_numbers
    let mut seen = std::collections::HashSet::new();
    for d in &body.days {
        if d.day_number < 1 {
            return Err(ApiError::BadRequest("day_number must be >= 1".into()));
        }
        if !seen.insert(d.day_number) {
            return Err(ApiError::BadRequest(format!("duplicate day_number: {}", d.day_number)));
        }
    }

    let mut tx = state.db.begin().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    sqlx::query("DELETE FROM inquiry_days WHERE inquiry_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    for d in &body.days {
        sqlx::query(
            "INSERT INTO inquiry_days (inquiry_id, day_date, day_number, notes) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(d.day_date)
        .bind(d.day_number)
        .bind(&d.notes)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    tx.commit().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(body.days.into_iter().map(|d| DayResponse { day_date: d.day_date, day_number: d.day_number, notes: d.notes }).collect()))
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
    let rows: Vec<(NaiveDate, i16, Option<String>)> = sqlx::query_as(
        "SELECT day_date, day_number, notes FROM calendar_item_days WHERE calendar_item_id = $1 ORDER BY day_number",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(rows.into_iter().map(|(day_date, day_number, notes)| DayResponse { day_date, day_number, notes }).collect()))
}

/// `PUT /api/v1/calendar-items/{id}/days` — Replace the full day list for a calendar item.
///
/// **Caller**: Calendar frontend when admin edits multi-day Termin.
/// **Why**: Full-replace semantics — empty `days` converts back to single-day.
///
/// # Errors
/// Returns 400 if any `day_number` values are duplicated or < 1.
async fn put_calendar_item_days(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<PutDaysRequest>,
) -> Result<Json<Vec<DayResponse>>, ApiError> {
    let mut seen = std::collections::HashSet::new();
    for d in &body.days {
        if d.day_number < 1 {
            return Err(ApiError::BadRequest("day_number must be >= 1".into()));
        }
        if !seen.insert(d.day_number) {
            return Err(ApiError::BadRequest(format!("duplicate day_number: {}", d.day_number)));
        }
    }

    let mut tx = state.db.begin().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    sqlx::query("DELETE FROM calendar_item_days WHERE calendar_item_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    for d in &body.days {
        sqlx::query(
            "INSERT INTO calendar_item_days (calendar_item_id, day_date, day_number, notes) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(d.day_date)
        .bind(d.day_number)
        .bind(&d.notes)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    tx.commit().await.map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(body.days.into_iter().map(|d| DayResponse { day_date: d.day_date, day_number: d.day_number, notes: d.notes }).collect()))
}
