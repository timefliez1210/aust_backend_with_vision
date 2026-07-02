//! CRUD for lightweight appointments linked to an inquiry (e.g. a Besichtigung
//! on its own, possibly non-consecutive, date). Mounted under the inquiries
//! router at `/api/v1/inquiries/{id}/appointments`.
//!
//! These are NOT crew/hours tracked — see `inquiry_appointment_repo`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch},
    Json, Router,
};
use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

use crate::repositories::{inquiry_appointment_repo as appt_repo, inquiry_repo};
use crate::repositories::inquiry_appointment_repo::AppointmentInput;
use crate::services::inquiry_builder::appointment_snapshot;
use crate::{ApiError, AppState};
use aust_core::models::AppointmentSnapshot;

const ALLOWED_STATUS: [&str; 3] = ["scheduled", "done", "cancelled"];

/// Appointment routes, merged into the inquiries router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/{id}/appointments",
            get(list_appointments).post(create_appointment),
        )
        .route(
            "/{id}/appointments/{appt_id}",
            patch(update_appointment).delete(delete_appointment),
        )
}

/// `GET /api/v1/inquiries/{id}/appointments` — list an inquiry's appointments.
async fn list_appointments(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AppointmentSnapshot>>, ApiError> {
    let rows = appt_repo::list_for_inquiry(&state.db, id).await?;
    Ok(Json(rows.into_iter().map(appointment_snapshot).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateAppointmentRequest {
    kind: Option<String>,
    scheduled_date: NaiveDate,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    assignee_id: Option<Uuid>,
    location: Option<String>,
    notes: Option<String>,
    status: Option<String>,
}

/// `POST /api/v1/inquiries/{id}/appointments` — create an appointment.
async fn create_appointment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateAppointmentRequest>,
) -> Result<(StatusCode, Json<AppointmentSnapshot>), ApiError> {
    if !inquiry_repo::exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Anfrage nicht gefunden.".into()));
    }
    if let Some(status) = body.status.as_deref() {
        validate_status(status)?;
    }
    validate_assignee(&state, body.assignee_id).await?;

    let input = AppointmentInput {
        kind: body.kind.as_deref(),
        scheduled_date: Some(body.scheduled_date),
        start_time: Some(body.start_time),
        end_time: Some(body.end_time),
        assignee_id: Some(body.assignee_id),
        location: Some(body.location.as_deref()),
        notes: Some(body.notes.as_deref()),
        status: body.status.as_deref(),
    };
    let new_id = appt_repo::create(&state.db, id, &input).await?;

    let row = appt_repo::fetch_one(&state.db, id, new_id)
        .await?
        .ok_or_else(|| ApiError::Internal("Termin nach dem Anlegen nicht gefunden.".into()))?;
    Ok((StatusCode::CREATED, Json(appointment_snapshot(row))))
}

/// `PATCH /api/v1/inquiries/{id}/appointments/{appt_id}` — partial update.
///
/// Uses raw JSON so a nullable field can be distinguished as *absent* (leave
/// unchanged) vs *explicit null* (clear it) — plain serde can't tell them apart.
async fn update_appointment(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> Result<Json<AppointmentSnapshot>, ApiError> {
    // Owned values kept alive so AppointmentInput can borrow &str from them.
    let kind = body.get("kind").and_then(Value::as_str).map(str::to_string);
    let status = body.get("status").and_then(Value::as_str).map(str::to_string);
    if let Some(s) = status.as_deref() {
        validate_status(s)?;
    }
    let scheduled_date = match body.get("scheduled_date") {
        Some(v) if !v.is_null() => Some(parse_date(v)?),
        _ => None,
    };
    let start_time = opt_time_field(&body, "start_time")?;
    let end_time = opt_time_field(&body, "end_time")?;
    let assignee_id = opt_uuid_field(&body, "assignee_id")?;
    let location = opt_str_field(&body, "location");
    let notes = opt_str_field(&body, "notes");

    if let Some(Some(assignee)) = assignee_id {
        validate_assignee(&state, Some(assignee)).await?;
    }

    let input = AppointmentInput {
        kind: kind.as_deref(),
        scheduled_date,
        start_time,
        end_time,
        assignee_id,
        location: location.as_ref().map(|o| o.as_deref()),
        notes: notes.as_ref().map(|o| o.as_deref()),
        status: status.as_deref(),
    };
    let affected = appt_repo::update(&state.db, id, appt_id, &input).await?;
    if affected == 0 {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }

    let row = appt_repo::fetch_one(&state.db, id, appt_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Termin nicht gefunden.".into()))?;
    Ok(Json(appointment_snapshot(row)))
}

/// `DELETE /api/v1/inquiries/{id}/appointments/{appt_id}` — remove an appointment.
async fn delete_appointment(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let affected = appt_repo::delete(&state.db, id, appt_id).await?;
    if affected == 0 {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn validate_status(status: &str) -> Result<(), ApiError> {
    if ALLOWED_STATUS.contains(&status) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Ungültiger Status. Erlaubt: scheduled, done, cancelled.".into(),
        ))
    }
}

async fn validate_assignee(state: &AppState, assignee_id: Option<Uuid>) -> Result<(), ApiError> {
    if let Some(emp_id) = assignee_id
        && inquiry_repo::check_employee_active(&state.db, emp_id).await?.is_none()
    {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden.".into()));
    }
    Ok(())
}

fn parse_date(v: &Value) -> Result<NaiveDate, ApiError> {
    v.as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .ok_or_else(|| ApiError::BadRequest("Ungültiges Datum (erwartet YYYY-MM-DD).".into()))
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
        .ok()
}

/// `None` = key absent; `Some(None)` = explicit null (clear); `Some(Some(v))` = set.
fn opt_str_field(body: &Value, key: &str) -> Option<Option<String>> {
    match body.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => None,
    }
}

fn opt_time_field(body: &Value, key: &str) -> Result<Option<Option<NaiveTime>>, ApiError> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => parse_time(s)
            .map(|t| Some(Some(t)))
            .ok_or_else(|| ApiError::BadRequest("Ungültige Uhrzeit (erwartet HH:MM).".into())),
        Some(_) => Ok(None),
    }
}

fn opt_uuid_field(body: &Value, key: &str) -> Result<Option<Option<Uuid>>, ApiError> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => Uuid::parse_str(s)
            .map(|u| Some(Some(u)))
            .map_err(|_| ApiError::BadRequest("Ungültige Mitarbeiter-ID.".into())),
        Some(_) => Ok(None),
    }
}
