use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use aust_calendar::{
    AvailabilityResult, Booking, CapacityOverride, NewBooking,
};
use crate::{ApiError, AppState};

/// Register the calendar and booking routes.
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Exposes availability checking, schedule overview, booking CRUD, and daily
/// capacity overrides for the moving company calendar.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/availability", get(check_availability))
        .route("/schedule", get(get_schedule))
        .route("/bookings", post(create_booking))
        .route("/bookings/{id}", get(get_booking).patch(update_booking).delete(delete_booking_handler))
        .route("/capacity/{date}", put(set_capacity))
}

#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    date: NaiveDate,
}

/// `GET /api/v1/calendar/availability?date=YYYY-MM-DD` — Check availability for a specific date.
///
/// **Caller**: Axum router / customer-facing booking calendar and admin scheduling view.
/// **Why**: Returns whether the requested date is available and, if not, suggests nearby
/// alternative dates (count configured in `Config.calendar.alternatives_count`).
///
/// # Parameters
/// - `state` — shared AppState (calendar service)
/// - `query` — `date` query parameter (NaiveDate, format YYYY-MM-DD)
///
/// # Returns
/// `200 OK` with `AvailabilityResult` (available flag + alternative dates).
///
/// # Errors
/// - `500` on calendar service failures
async fn check_availability(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AvailabilityQuery>,
) -> Result<Json<AvailabilityResult>, ApiError> {
    let result = state
        .calendar
        .check_availability(query.date)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct ScheduleQuery {
    from: NaiveDate,
    to: NaiveDate,
}

/// Enriched booking with offer price for frontend display.
#[derive(Debug, Serialize)]
struct EnrichedBooking {
    #[serde(flatten)]
    booking: Booking,
    offer_price_cents: Option<i64>,
}

#[derive(Debug, Serialize)]
struct EnrichedScheduleEntry {
    date: NaiveDate,
    availability: aust_calendar::DateAvailability,
    bookings: Vec<EnrichedBooking>,
}

/// `GET /api/v1/calendar/schedule?from=YYYY-MM-DD&to=YYYY-MM-DD` — Fetch the schedule for a date range.
///
/// **Caller**: Axum router / admin dashboard calendar view.
/// **Why**: Returns each date in the range with its availability status and all bookings
/// for that day. Also enriches bookings with the latest non-rejected offer's netto price
/// so the calendar view can display the job value without extra requests. Maximum range is
/// 90 days.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, calendar service)
/// - `query` — `from` and `to` NaiveDate query parameters
///
/// # Returns
/// `200 OK` with `Vec<EnrichedScheduleEntry>` (date, availability, enriched bookings).
///
/// # Errors
/// - `400` if `from > to` or the range exceeds 90 days
/// - `500` on calendar service failures
async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScheduleQuery>,
) -> Result<Json<Vec<EnrichedScheduleEntry>>, ApiError> {
    if query.from > query.to {
        return Err(ApiError::BadRequest(
            "'from' must be before or equal to 'to'".to_string(),
        ));
    }

    let max_days = 90;
    if (query.to - query.from).num_days() > max_days {
        return Err(ApiError::BadRequest(format!(
            "Date range must not exceed {max_days} days"
        )));
    }

    let schedule = state
        .calendar
        .get_schedule(query.from, query.to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Collect all inquiry_ids from bookings to fetch offer prices in one query
    let inquiry_ids: Vec<Uuid> = schedule
        .iter()
        .flat_map(|entry| &entry.bookings)
        .filter_map(|b| b.inquiry_id)
        .collect();

    let mut price_map: HashMap<Uuid, i64> = HashMap::new();
    if !inquiry_ids.is_empty() {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT inquiry_id, price_cents FROM offers WHERE inquiry_id = ANY($1) AND status != 'rejected' ORDER BY created_at DESC",
        )
        .bind(&inquiry_ids)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        for (qid, price) in rows {
            price_map.entry(qid).or_insert(price);
        }
    }

    let enriched: Vec<EnrichedScheduleEntry> = schedule
        .into_iter()
        .map(|entry| EnrichedScheduleEntry {
            date: entry.date,
            availability: entry.availability,
            bookings: entry
                .bookings
                .into_iter()
                .map(|b| {
                    let price = b.inquiry_id.and_then(|qid| price_map.get(&qid).copied());
                    EnrichedBooking {
                        booking: b,
                        offer_price_cents: price,
                    }
                })
                .collect(),
        })
        .collect();

    Ok(Json(enriched))
}

/// `POST /api/v1/calendar/bookings` — Create a new moving booking.
///
/// **Caller**: Axum router / admin dashboard booking creation.
/// **Why**: Delegates to `CalendarService.create_booking`, which checks capacity before
/// inserting. Returns a `FullyBooked` error (mapped to 400) if the date is at capacity.
///
/// # Parameters
/// - `state` — shared AppState (calendar service)
/// - `request` — `NewBooking` JSON body (date, optional inquiry_id, notes)
///
/// # Returns
/// `200 OK` with the created `Booking` JSON.
///
/// # Errors
/// - `400` if the selected date is fully booked
/// - `500` on DB failures
async fn create_booking(
    State(state): State<Arc<AppState>>,
    Json(request): Json<NewBooking>,
) -> Result<Json<Booking>, ApiError> {
    let booking = state
        .calendar
        .create_booking(request)
        .await
        .map_err(|e| match &e {
            aust_calendar::CalendarError::FullyBooked(_) => {
                ApiError::BadRequest(e.to_string())
            }
            _ => ApiError::Internal(e.to_string()),
        })?;

    Ok(Json(booking))
}

/// `GET /api/v1/calendar/bookings/{id}` — Retrieve a single booking by ID.
///
/// **Caller**: Axum router / admin dashboard booking detail.
///
/// # Parameters
/// - `state` — shared AppState (calendar service)
/// - `id` — booking UUID path parameter
///
/// # Returns
/// `200 OK` with `Booking` JSON.
///
/// # Errors
/// - `404` if booking not found
async fn get_booking(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Booking>, ApiError> {
    let booking = state
        .calendar
        .get_booking(id)
        .await
        .map_err(|e| match &e {
            aust_calendar::CalendarError::NotFound(_) => ApiError::NotFound(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        })?;

    Ok(Json(booking))
}

#[derive(Debug, Deserialize)]
struct UpdateBookingRequest {
    status: String,
}

/// `PATCH /api/v1/calendar/bookings/{id}` — Update booking status (confirm or cancel).
///
/// **Caller**: Axum router / admin dashboard booking management.
/// **Why**: Only "confirmed" and "cancelled" status transitions are accepted. When a
/// booking with a linked `inquiry_id` is confirmed or cancelled, `status_sync` cascades the
/// change to the quote status so the pipeline state stays consistent.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, calendar service)
/// - `id` — booking UUID path parameter
/// - `request` — JSON body with `status` field ("confirmed" or "cancelled")
///
/// # Returns
/// `200 OK` with updated `Booking` JSON.
///
/// # Errors
/// - `400` if status is not "confirmed" or "cancelled"
/// - `404` if booking not found
async fn update_booking(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateBookingRequest>,
) -> Result<Json<Booking>, ApiError> {
    let booking = match request.status.as_str() {
        "cancelled" => state.calendar.cancel_booking(id).await,
        "confirmed" => state.calendar.confirm_booking(id).await,
        other => {
            return Err(ApiError::BadRequest(format!(
                "Invalid status transition: '{other}'. Use 'confirmed' or 'cancelled'."
            )));
        }
    }
    .map_err(|e| match &e {
        aust_calendar::CalendarError::NotFound(_) => ApiError::NotFound(e.to_string()),
        _ => ApiError::Internal(e.to_string()),
    })?;

    // Sync linked quote status when booking has a inquiry_id
    if let Some(inquiry_id) = booking.inquiry_id {
        match booking.status.as_str() {
            "confirmed" => {
                crate::services::status_sync::sync_booking_confirmed(&state.db, inquiry_id).await.ok();
            }
            "cancelled" => {
                crate::services::status_sync::sync_booking_cancelled(&state.db, inquiry_id).await.ok();
            }
            _ => {}
        }
    }

    Ok(Json(booking))
}

/// `DELETE /api/v1/calendar/bookings/{id}` — Hard-delete a booking record.
///
/// **Caller**: Axum router / admin dashboard "Buchung löschen" action.
/// **Why**: Permanently removes the booking. Does not update the linked quote status —
/// use `update_booking` with "cancelled" first if status sync is needed.
///
/// # Parameters
/// - `state` — shared AppState (calendar service)
/// - `id` — booking UUID path parameter
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if booking not found
async fn delete_booking_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .calendar
        .delete_booking(id)
        .await
        .map_err(|e| match &e {
            aust_calendar::CalendarError::NotFound(_) => ApiError::NotFound(e.to_string()),
            _ => ApiError::Internal(e.to_string()),
        })?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `PUT /api/v1/calendar/capacity/{date}` — Override the daily booking capacity for a specific date.
///
/// **Caller**: Axum router / admin dashboard capacity management.
/// **Why**: The default daily capacity is set in `Config.calendar.default_capacity`. This
/// endpoint upserts a `calendar_capacity_overrides` row so a particular day can have a
/// different limit (e.g. set to 0 for holidays or set higher for extra crew days).
///
/// # Parameters
/// - `state` — shared AppState (calendar service)
/// - `date` — NaiveDate path parameter (YYYY-MM-DD)
/// - `request` — JSON body with `capacity` (non-negative integer)
///
/// # Returns
/// `200 OK` with `CapacityOverride` JSON.
///
/// # Errors
/// - `400` if `capacity < 0`
/// - `500` on DB failures
async fn set_capacity(
    State(state): State<Arc<AppState>>,
    Path(date): Path<NaiveDate>,
    Json(request): Json<SetCapacityRequest>,
) -> Result<Json<CapacityOverride>, ApiError> {
    if request.capacity < 0 {
        return Err(ApiError::BadRequest(
            "Capacity must be >= 0".to_string(),
        ));
    }

    let result = state
        .calendar
        .set_capacity(date, request.capacity)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct SetCapacityRequest {
    capacity: i32,
}
