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
use crate::services::inquiry_builder::appointment_snapshot_full;
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
        .route(
            "/{id}/appointments/{appt_id}/employees",
            get(list_crew).post(assign_crew).put(replace_crew),
        )
        .route(
            "/{id}/appointments/{appt_id}/employees/{emp_id}",
            patch(update_crew).delete(remove_crew),
        )
}

/// `GET /api/v1/inquiries/{id}/appointments` — list an inquiry's appointments.
async fn list_appointments(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AppointmentSnapshot>>, ApiError> {
    let rows = appt_repo::list_for_inquiry(&state.db, id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(appointment_snapshot_full(&state.db, row).await?);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct CreateAppointmentRequest {
    kind: Option<String>,
    scheduled_date: NaiveDate,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    assignee_id: Option<Uuid>,
    location: Option<String>,
    description: Option<String>,
    address_id: Option<Uuid>,
    notes: Option<String>,
    employee_notes: Option<String>,
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
        description: Some(body.description.as_deref()),
        address_id: Some(body.address_id),
        notes: Some(body.notes.as_deref()),
        employee_notes: Some(body.employee_notes.as_deref()),
        status: body.status.as_deref(),
    };
    let new_id = appt_repo::create(&state.db, id, &input).await?;

    let row = appt_repo::fetch_one(&state.db, id, new_id)
        .await?
        .ok_or_else(|| ApiError::Internal("Termin nach dem Anlegen nicht gefunden.".into()))?;
    Ok((StatusCode::CREATED, Json(appointment_snapshot_full(&state.db, row).await?)))
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
    let description = opt_str_field(&body, "description");
    let address_id = opt_uuid_field(&body, "address_id")?;
    let notes = opt_str_field(&body, "notes");
    let employee_notes = opt_str_field(&body, "employee_notes");

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
        description: description.as_ref().map(|o| o.as_deref()),
        address_id,
        notes: notes.as_ref().map(|o| o.as_deref()),
        employee_notes: employee_notes.as_ref().map(|o| o.as_deref()),
        status: status.as_deref(),
    };
    let affected = appt_repo::update(&state.db, id, appt_id, &input).await?;
    if affected == 0 {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }

    let row = appt_repo::fetch_one(&state.db, id, appt_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Termin nicht gefunden.".into()))?;
    Ok(Json(appointment_snapshot_full(&state.db, row).await?))
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

// ── Crew (paid Zusatztermine) ────────────────────────────────────────────────

/// Load the full appointment snapshot scoped to its inquiry (404 if missing).
async fn load_appointment(
    state: &AppState,
    inquiry_id: Uuid,
    appt_id: Uuid,
) -> Result<AppointmentSnapshot, ApiError> {
    let row = appt_repo::fetch_one(&state.db, inquiry_id, appt_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Termin nicht gefunden.".into()))?;
    appointment_snapshot_full(&state.db, row).await
}

#[derive(Debug, Deserialize)]
struct AssignCrewBody {
    employee_id: Uuid,
}

/// `GET /{id}/appointments/{appt_id}/employees` — the appointment's crew.
async fn list_crew(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<aust_core::models::EmployeeAssignmentSnapshot>>, ApiError> {
    let appt = load_appointment(&state, id, appt_id).await?;
    Ok(Json(appt.employees))
}

/// `POST /{id}/appointments/{appt_id}/employees` — assign an employee to the entry.
async fn assign_crew(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<AssignCrewBody>,
) -> Result<(StatusCode, Json<AppointmentSnapshot>), ApiError> {
    // Scope the appointment to its inquiry, then validate the employee.
    if appt_repo::fetch_one(&state.db, id, appt_id).await?.is_none() {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }
    if inquiry_repo::check_employee_active(&state.db, body.employee_id).await?.is_none() {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden.".into()));
    }
    appt_repo::insert_appointment_employee(&state.db, appt_id, body.employee_id).await?;
    Ok((StatusCode::CREATED, Json(load_appointment(&state, id, appt_id).await?)))
}

/// Body for updating hours/notes/expenses on a crew assignment.
/// Time fields use the lenient parser (accepts "7:30", "07:30", "7.30").
#[derive(Debug, Deserialize)]
struct UpdateCrewBody {
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    clock_in: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    clock_out: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    start_time: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    end_time: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours: Option<f64>,
    notes: Option<String>,
    transport_mode: Option<String>,
    travel_costs_cents: Option<i64>,
    accommodation_cents: Option<i64>,
    misc_costs_cents: Option<i64>,
    meal_deduction: Option<String>,
}

/// `PATCH /{id}/appointments/{appt_id}/employees/{emp_id}` — update a crew assignment.
async fn update_crew(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id, emp_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(body): Json<UpdateCrewBody>,
) -> Result<Json<AppointmentSnapshot>, ApiError> {
    if appt_repo::fetch_one(&state.db, id, appt_id).await?.is_none() {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }
    let rows = appt_repo::update_appointment_employee(
        &state.db,
        appt_id,
        emp_id,
        body.clock_in,
        body.clock_out,
        body.start_time,
        body.end_time,
        body.break_minutes,
        body.actual_hours,
        body.notes.as_deref(),
        body.transport_mode.as_deref(),
        body.travel_costs_cents,
        body.accommodation_cents,
        body.misc_costs_cents,
        body.meal_deduction.as_deref(),
    )
    .await?;
    if rows == 0 {
        return Err(ApiError::NotFound(
            "Mitarbeiter ist nicht zugewiesen — bitte erst zuweisen, dann Zeiten eintragen.".into(),
        ));
    }
    Ok(Json(load_appointment(&state, id, appt_id).await?))
}

/// `DELETE /{id}/appointments/{appt_id}/employees/{emp_id}` — unassign an employee.
async fn remove_crew(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id, emp_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    if appt_repo::fetch_one(&state.db, id, appt_id).await?.is_none() {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }
    let rows = appt_repo::delete_appointment_employee(&state.db, appt_id, emp_id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden.".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// One crew row for the full-replace (`PUT`) body.
#[derive(Debug, Deserialize)]
struct BulkCrewBody {
    employee_id: Uuid,
    notes: Option<String>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    start_time: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    end_time: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    clock_in: Option<NaiveTime>,
    #[serde(default, deserialize_with = "aust_core::models::deserialize_lenient_time")]
    clock_out: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours: Option<f64>,
    transport_mode: Option<String>,
    travel_costs_cents: Option<i64>,
    accommodation_cents: Option<i64>,
    misc_costs_cents: Option<i64>,
    meal_deduction: Option<String>,
}

/// `PUT /{id}/appointments/{appt_id}/employees` — full-replace the crew.
async fn replace_crew(
    State(state): State<Arc<AppState>>,
    Path((id, appt_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Vec<BulkCrewBody>>,
) -> Result<Json<AppointmentSnapshot>, ApiError> {
    if appt_repo::fetch_one(&state.db, id, appt_id).await?.is_none() {
        return Err(ApiError::NotFound("Termin nicht gefunden.".into()));
    }
    for b in &body {
        if inquiry_repo::check_employee_active(&state.db, b.employee_id).await?.is_none() {
            return Err(ApiError::NotFound("Mitarbeiter nicht gefunden.".into()));
        }
    }
    let inputs: Vec<appt_repo::AppointmentEmployeeInput> = body
        .into_iter()
        .map(|b| appt_repo::AppointmentEmployeeInput {
            employee_id: b.employee_id,
            notes: b.notes,
            start_time: b.start_time,
            end_time: b.end_time,
            clock_in: b.clock_in,
            clock_out: b.clock_out,
            break_minutes: b.break_minutes.unwrap_or(0),
            actual_hours: b.actual_hours,
            transport_mode: b.transport_mode,
            travel_costs_cents: b.travel_costs_cents,
            accommodation_cents: b.accommodation_cents,
            misc_costs_cents: b.misc_costs_cents,
            meal_deduction: b.meal_deduction,
        })
        .collect();
    appt_repo::put_appointment_employees(&state.db, appt_id, &inputs).await?;
    Ok(Json(load_appointment(&state, id, appt_id).await?))
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
