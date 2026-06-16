//! Vehicle repository — queries for the `vehicles` and `vehicle_reminders`
//! tables.
//!
//! Powers the admin fleet page (CRUD) and the background reminder tick
//! (`services/vehicle_reminder_service.rs`), which pings Alex's Telegram chat as
//! a reminder's due date approaches.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// A vehicle (car, truck, transporter) with a free-form label.
#[derive(Debug, serde::Serialize, FromRow)]
pub(crate) struct VehicleRow {
    pub id: Uuid,
    pub label: String,
    pub kennzeichen: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single reminder attached to a vehicle.
#[derive(Debug, serde::Serialize, FromRow)]
pub(crate) struct ReminderRow {
    pub id: Uuid,
    pub vehicle_id: Uuid,
    pub label: String,
    pub due_date: NaiveDate,
    pub active: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_pinged_on: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A vehicle bundled with its reminders — the shape the admin page consumes.
#[derive(Debug, serde::Serialize)]
pub(crate) struct VehicleWithReminders {
    #[serde(flatten)]
    pub vehicle: VehicleRow,
    pub reminders: Vec<ReminderRow>,
}

/// An active reminder joined with its vehicle label, for the ping tick.
#[derive(Debug, FromRow)]
pub(crate) struct PingableReminder {
    pub id: Uuid,
    pub vehicle_label: String,
    pub reminder_label: String,
    pub due_date: NaiveDate,
    pub last_pinged_on: Option<NaiveDate>,
}

// ── Vehicle CRUD ────────────────────────────────────────────────────────────

/// List every vehicle with its reminders attached.
///
/// **Caller**: `GET /admin/vehicles`.
/// **Why**: The fleet page renders each vehicle as a card with its reminder list,
/// so one round-trip returning the nested shape avoids N+1 calls from the client.
pub(crate) async fn list_vehicles_with_reminders(
    pool: &PgPool,
) -> Result<Vec<VehicleWithReminders>, ApiError> {
    let vehicles: Vec<VehicleRow> = sqlx::query_as(
        "SELECT id, label, kennzeichen, created_at, updated_at FROM vehicles ORDER BY label ASC",
    )
    .fetch_all(pool)
    .await?;

    // Reminders for active vehicles first (active=true), then by due date so the
    // most urgent shows on top within each vehicle.
    let reminders: Vec<ReminderRow> = sqlx::query_as(
        r#"
        SELECT id, vehicle_id, label, due_date, active, completed_at,
               last_pinged_on, created_at, updated_at
        FROM vehicle_reminders
        ORDER BY active DESC, due_date ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(vehicles
        .into_iter()
        .map(|vehicle| {
            let reminders = reminders
                .iter()
                .filter(|r| r.vehicle_id == vehicle.id)
                .map(|r| ReminderRow {
                    id: r.id,
                    vehicle_id: r.vehicle_id,
                    label: r.label.clone(),
                    due_date: r.due_date,
                    active: r.active,
                    completed_at: r.completed_at,
                    last_pinged_on: r.last_pinged_on,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })
                .collect();
            VehicleWithReminders { vehicle, reminders }
        })
        .collect())
}

/// Insert a new vehicle and return the created row.
pub(crate) async fn insert_vehicle(
    pool: &PgPool,
    label: &str,
    kennzeichen: &str,
) -> Result<VehicleRow, ApiError> {
    sqlx::query_as(
        r#"
        INSERT INTO vehicles (label, kennzeichen) VALUES ($1, $2)
        RETURNING id, label, kennzeichen, created_at, updated_at
        "#,
    )
    .bind(label)
    .bind(kennzeichen)
    .fetch_one(pool)
    .await
    .map_err(ApiError::from)
}

/// Update a vehicle's label and license plate. Returns the updated row, or
/// `404` when it does not exist.
pub(crate) async fn update_vehicle(
    pool: &PgPool,
    id: Uuid,
    label: &str,
    kennzeichen: &str,
) -> Result<VehicleRow, ApiError> {
    sqlx::query_as(
        r#"
        UPDATE vehicles SET label = $1, kennzeichen = $2, updated_at = NOW()
        WHERE id = $3
        RETURNING id, label, kennzeichen, created_at, updated_at
        "#,
    )
    .bind(label)
    .bind(kennzeichen)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Fahrzeug nicht gefunden".into()))
}

/// Returns true when a vehicle with the given id exists.
pub(crate) async fn vehicle_exists(pool: &PgPool, id: Uuid) -> Result<bool, ApiError> {
    let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM vehicles WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(exists.is_some())
}

/// Delete a vehicle (and its reminders, via ON DELETE CASCADE). Returns rows affected.
pub(crate) async fn delete_vehicle(pool: &PgPool, id: Uuid) -> Result<u64, ApiError> {
    let res = sqlx::query("DELETE FROM vehicles WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

// ── Reminder CRUD ───────────────────────────────────────────────────────────

/// Insert a reminder for a vehicle and return the created row.
pub(crate) async fn insert_reminder(
    pool: &PgPool,
    vehicle_id: Uuid,
    label: &str,
    due_date: NaiveDate,
) -> Result<ReminderRow, ApiError> {
    sqlx::query_as(
        r#"
        INSERT INTO vehicle_reminders (vehicle_id, label, due_date)
        VALUES ($1, $2, $3)
        RETURNING id, vehicle_id, label, due_date, active, completed_at,
                  last_pinged_on, created_at, updated_at
        "#,
    )
    .bind(vehicle_id)
    .bind(label)
    .bind(due_date)
    .fetch_one(pool)
    .await
    .map_err(ApiError::from)
}

/// Partially update a reminder (label / due_date / active).
///
/// Setting `active = Some(false)` marks it done/dismissed: `completed_at` is
/// stamped and the ping tick stops nagging. Setting it back to `true` clears
/// `completed_at` and resumes the cadence. Changing `due_date` clears
/// `last_pinged_on` so the new schedule is evaluated fresh.
///
/// Returns the updated row, or `404` when (vehicle_id, id) does not match.
pub(crate) async fn update_reminder(
    pool: &PgPool,
    vehicle_id: Uuid,
    id: Uuid,
    label: Option<&str>,
    due_date: Option<NaiveDate>,
    active: Option<bool>,
) -> Result<ReminderRow, ApiError> {
    sqlx::query_as(
        r#"
        UPDATE vehicle_reminders SET
            label          = COALESCE($3, label),
            due_date       = COALESCE($4, due_date),
            active         = COALESCE($5, active),
            completed_at   = CASE
                                WHEN $5 IS NULL THEN completed_at
                                WHEN $5 = TRUE  THEN NULL
                                ELSE NOW()
                             END,
            last_pinged_on = CASE WHEN $4 IS NULL THEN last_pinged_on ELSE NULL END,
            updated_at     = NOW()
        WHERE id = $1 AND vehicle_id = $2
        RETURNING id, vehicle_id, label, due_date, active, completed_at,
                  last_pinged_on, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(vehicle_id)
    .bind(label)
    .bind(due_date)
    .bind(active)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Erinnerung nicht gefunden".into()))
}

/// Delete a single reminder scoped to its vehicle. Returns rows affected.
pub(crate) async fn delete_reminder(
    pool: &PgPool,
    vehicle_id: Uuid,
    id: Uuid,
) -> Result<u64, ApiError> {
    let res = sqlx::query("DELETE FROM vehicle_reminders WHERE id = $1 AND vehicle_id = $2")
        .bind(id)
        .bind(vehicle_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

// ── Ping tick helpers ───────────────────────────────────────────────────────

/// Fetch all active reminders joined with their vehicle label.
///
/// **Caller**: `vehicle_reminder_service::run_reminder_check`.
/// **Why**: The cadence decision (which day to ping) is wall-clock-sensitive and
/// computed in Rust against the Europe/Berlin date, so we pull all active rows
/// and filter there rather than encoding the date math in SQL.
pub(crate) async fn fetch_active_reminders(
    pool: &PgPool,
) -> Result<Vec<PingableReminder>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT vr.id,
               v.label  AS vehicle_label,
               vr.label AS reminder_label,
               vr.due_date,
               vr.last_pinged_on
        FROM vehicle_reminders vr
        JOIN vehicles v ON v.id = vr.vehicle_id
        WHERE vr.active
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Stamp `last_pinged_on` so the 60-second tick fires at most once per day.
pub(crate) async fn mark_pinged(
    pool: &PgPool,
    id: Uuid,
    on: NaiveDate,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE vehicle_reminders SET last_pinged_on = $1 WHERE id = $2")
        .bind(on)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
