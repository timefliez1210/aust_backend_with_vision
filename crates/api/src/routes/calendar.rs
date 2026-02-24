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
use chrono::Utc;
use crate::{ApiError, AppState};

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

    // Collect all quote_ids from bookings to fetch offer prices in one query
    let quote_ids: Vec<Uuid> = schedule
        .iter()
        .flat_map(|entry| &entry.bookings)
        .filter_map(|b| b.quote_id)
        .collect();

    let mut price_map: HashMap<Uuid, i64> = HashMap::new();
    if !quote_ids.is_empty() {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT quote_id, price_cents FROM offers WHERE quote_id = ANY($1) AND status != 'rejected' ORDER BY created_at DESC",
        )
        .bind(&quote_ids)
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
                    let price = b.quote_id.and_then(|qid| price_map.get(&qid).copied());
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

    // Sync linked quote status when booking has a quote_id
    if let Some(quote_id) = booking.quote_id {
        let now = Utc::now();
        match booking.status.as_str() {
            "confirmed" => {
                // Booking confirmed → quote accepted, offer accepted
                sqlx::query("UPDATE quotes SET status = 'accepted', updated_at = $1 WHERE id = $2 AND status IN ('offer_generated', 'offer_sent')")
                    .bind(now)
                    .bind(quote_id)
                    .execute(&state.db)
                    .await
                    .ok();
                sqlx::query("UPDATE offers SET status = 'accepted' WHERE quote_id = $1 AND status IN ('draft', 'sent')")
                    .bind(quote_id)
                    .execute(&state.db)
                    .await
                    .ok();
            }
            "cancelled" => {
                // Booking cancelled → quote rejected
                sqlx::query("UPDATE quotes SET status = 'rejected', updated_at = $1 WHERE id = $2 AND status IN ('offer_generated', 'offer_sent', 'accepted')")
                    .bind(now)
                    .bind(quote_id)
                    .execute(&state.db)
                    .await
                    .ok();
            }
            _ => {}
        }
    }

    Ok(Json(booking))
}

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
