use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use chrono::NaiveDate;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use aust_calendar::{
    AvailabilityResult, Booking, CapacityOverride, NewBooking, ScheduleEntry,
};
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/availability", get(check_availability))
        .route("/schedule", get(get_schedule))
        .route("/bookings", post(create_booking))
        .route("/bookings/{id}", get(get_booking).patch(update_booking))
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

async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScheduleQuery>,
) -> Result<Json<Vec<ScheduleEntry>>, ApiError> {
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

    Ok(Json(schedule))
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

    Ok(Json(booking))
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
