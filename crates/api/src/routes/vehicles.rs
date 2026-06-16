//! Vehicle fleet management — admin CRUD for vehicles and their reminders.
//!
//! Mounted under `/admin/vehicles` inside the admin JWT-protected layer
//! (see `lib.rs`). Alex maintains a list of vehicles (cars, trucks,
//! transporters), each with free-form reminders (TÜV, Ölwechsel, …) that the
//! background tick in `services/vehicle_reminder_service.rs` pings to Telegram
//! as the due date approaches.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use chrono::NaiveDate;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;

use crate::repositories::vehicle_repo;
use crate::{error::ApiError, AppState};

/// Register all vehicle routes (protected under admin JWT middleware).
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_vehicles).post(create_vehicle))
        .route("/{id}", patch(update_vehicle).delete(delete_vehicle))
        .route("/{id}/reminders", post(create_reminder))
        .route(
            "/{id}/reminders/{rid}",
            patch(update_reminder).delete(delete_reminder),
        )
}

// ── Request bodies ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct VehicleBody {
    label: String,
    kennzeichen: String,
}

#[derive(Debug, Deserialize)]
struct CreateReminderBody {
    label: String,
    due_date: NaiveDate,
}

#[derive(Debug, Deserialize)]
struct UpdateReminderBody {
    label: Option<String>,
    due_date: Option<NaiveDate>,
    /// `false` marks the reminder done/dismissed and stops the Telegram nag;
    /// `true` reactivates it.
    active: Option<bool>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /admin/vehicles` — list all vehicles with their reminders nested.
async fn list_vehicles(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<Vec<vehicle_repo::VehicleWithReminders>>, ApiError> {
    Ok(Json(
        vehicle_repo::list_vehicles_with_reminders(&state.db).await?,
    ))
}

/// `POST /admin/vehicles` — create a vehicle from a free-form label.
async fn create_vehicle(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(body): Json<VehicleBody>,
) -> Result<(StatusCode, Json<vehicle_repo::VehicleRow>), ApiError> {
    let (label, kennzeichen) = validate_vehicle(&body)?;
    let row = vehicle_repo::insert_vehicle(&state.db, label, kennzeichen).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// `PATCH /admin/vehicles/{id}` — rename a vehicle.
async fn update_vehicle(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<VehicleBody>,
) -> Result<Json<vehicle_repo::VehicleRow>, ApiError> {
    let (label, kennzeichen) = validate_vehicle(&body)?;
    Ok(Json(
        vehicle_repo::update_vehicle(&state.db, id, label, kennzeichen).await?,
    ))
}

/// Validate and trim a vehicle body, returning the cleaned (label, kennzeichen).
fn validate_vehicle(body: &VehicleBody) -> Result<(&str, &str), ApiError> {
    let label = body.label.trim();
    let kennzeichen = body.kennzeichen.trim();
    if label.is_empty() {
        return Err(ApiError::Validation("Bezeichnung darf nicht leer sein".into()));
    }
    if label.chars().count() > 120 {
        return Err(ApiError::Validation("Bezeichnung zu lang".into()));
    }
    if kennzeichen.is_empty() {
        return Err(ApiError::Validation("Kennzeichen darf nicht leer sein".into()));
    }
    if kennzeichen.chars().count() > 20 {
        return Err(ApiError::Validation("Kennzeichen zu lang".into()));
    }
    Ok((label, kennzeichen))
}

/// `DELETE /admin/vehicles/{id}` — delete a vehicle and its reminders.
async fn delete_vehicle(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let rows = vehicle_repo::delete_vehicle(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Fahrzeug nicht gefunden".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /admin/vehicles/{id}/reminders` — add a reminder to a vehicle.
async fn create_reminder(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateReminderBody>,
) -> Result<(StatusCode, Json<vehicle_repo::ReminderRow>), ApiError> {
    let label = body.label.trim();
    if label.is_empty() {
        return Err(ApiError::Validation("Bezeichnung darf nicht leer sein".into()));
    }
    if label.chars().count() > 120 {
        return Err(ApiError::Validation("Bezeichnung zu lang".into()));
    }
    if !vehicle_repo::vehicle_exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Fahrzeug nicht gefunden".into()));
    }
    let row = vehicle_repo::insert_reminder(&state.db, id, label, body.due_date).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// `PATCH /admin/vehicles/{id}/reminders/{rid}` — edit / complete / dismiss a reminder.
async fn update_reminder(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, rid)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateReminderBody>,
) -> Result<Json<vehicle_repo::ReminderRow>, ApiError> {
    let label = body.label.as_deref().map(str::trim);
    if let Some(l) = label {
        if l.is_empty() {
            return Err(ApiError::Validation("Bezeichnung darf nicht leer sein".into()));
        }
        if l.chars().count() > 120 {
            return Err(ApiError::Validation("Bezeichnung zu lang".into()));
        }
    }
    Ok(Json(
        vehicle_repo::update_reminder(&state.db, id, rid, label, body.due_date, body.active)
            .await?,
    ))
}

/// `DELETE /admin/vehicles/{id}/reminders/{rid}` — remove a reminder.
async fn delete_reminder(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, rid)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let rows = vehicle_repo::delete_reminder(&state.db, id, rid).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Erinnerung nicht gefunden".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}
