//! Repository for `inquiry_appointments` — lightweight, possibly non-consecutive
//! appointments linked to an inquiry (e.g. a Besichtigung before the move).
//!
//! Not crew/hours tracked: at most one optional assignee. The move itself lives
//! on `inquiries.scheduled_date .. end_date`; these are separate dated entries.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// One appointment row joined with its assignee's name (if any).
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AppointmentRow {
    pub id: Uuid,
    pub kind: String,
    pub scheduled_date: NaiveDate,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub assignee_id: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub address_id: Option<Uuid>,
    pub notes: Option<String>,
    pub employee_notes: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Fields accepted when creating or updating an appointment.
#[derive(Debug, Default)]
pub(crate) struct AppointmentInput<'a> {
    pub kind: Option<&'a str>,
    pub scheduled_date: Option<NaiveDate>,
    pub start_time: Option<Option<NaiveTime>>,
    pub end_time: Option<Option<NaiveTime>>,
    pub assignee_id: Option<Option<Uuid>>,
    pub location: Option<Option<&'a str>>,
    pub description: Option<Option<&'a str>>,
    pub address_id: Option<Option<Uuid>>,
    pub notes: Option<Option<&'a str>>,
    pub employee_notes: Option<Option<&'a str>>,
    pub status: Option<&'a str>,
}

const SELECT_JOINED: &str = r#"
    SELECT a.id, a.kind, a.scheduled_date, a.start_time, a.end_time,
           a.assignee_id,
           CASE WHEN e.id IS NULL THEN NULL
                ELSE TRIM(CONCAT(e.first_name, ' ', e.last_name)) END AS assignee_name,
           a.location, a.description, a.address_id, a.notes, a.employee_notes,
           a.status, a.created_at
    FROM inquiry_appointments a
    LEFT JOIN employees e ON e.id = a.assignee_id
"#;

/// List all appointments for an inquiry, earliest first.
///
/// **Caller**: `inquiry_builder::build_inquiry_response`, appointment routes.
pub(crate) async fn list_for_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<AppointmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT_JOINED} WHERE a.inquiry_id = $1 ORDER BY a.scheduled_date, a.start_time NULLS FIRST"
    ))
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Fetch a single appointment scoped to its inquiry (so a wrong inquiry_id 404s).
pub(crate) async fn fetch_one(
    pool: &PgPool,
    inquiry_id: Uuid,
    appointment_id: Uuid,
) -> Result<Option<AppointmentRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT_JOINED} WHERE a.inquiry_id = $1 AND a.id = $2"
    ))
    .bind(inquiry_id)
    .bind(appointment_id)
    .fetch_optional(pool)
    .await
}

/// Insert a new appointment for an inquiry. `scheduled_date` is required; all
/// other fields fall back to their column defaults / NULL.
pub(crate) async fn create(
    pool: &PgPool,
    inquiry_id: Uuid,
    input: &AppointmentInput<'_>,
) -> Result<Uuid, sqlx::Error> {
    let id: (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO inquiry_appointments
            (inquiry_id, kind, scheduled_date, start_time, end_time,
             assignee_id, location, description, address_id, notes,
             employee_notes, status)
        VALUES ($1,
                COALESCE($2, 'besichtigung'),
                $3, $4, $5, $6, $7, $8, $9, $10, $11,
                COALESCE($12, 'scheduled'))
        RETURNING id
        "#,
    )
    .bind(inquiry_id)
    .bind(input.kind)
    .bind(input.scheduled_date)
    .bind(input.start_time.flatten())
    .bind(input.end_time.flatten())
    .bind(input.assignee_id.flatten())
    .bind(input.location.flatten())
    .bind(input.description.flatten())
    .bind(input.address_id.flatten())
    .bind(input.notes.flatten())
    .bind(input.employee_notes.flatten())
    .bind(input.status)
    .fetch_one(pool)
    .await?;
    Ok(id.0)
}

/// Partial update: every `Some` field is written; `None` leaves the column
/// untouched. A `Some(None)` on a nullable field clears it. Returns rows affected.
pub(crate) async fn update(
    pool: &PgPool,
    inquiry_id: Uuid,
    appointment_id: Uuid,
    input: &AppointmentInput<'_>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE inquiry_appointments SET
            kind           = COALESCE($3, kind),
            scheduled_date = COALESCE($4, scheduled_date),
            start_time     = CASE WHEN $5  THEN $6  ELSE start_time  END,
            end_time       = CASE WHEN $7  THEN $8  ELSE end_time    END,
            assignee_id    = CASE WHEN $9  THEN $10 ELSE assignee_id END,
            location       = CASE WHEN $11 THEN $12 ELSE location    END,
            notes          = CASE WHEN $13 THEN $14 ELSE notes       END,
            description    = CASE WHEN $15 THEN $16 ELSE description  END,
            address_id     = CASE WHEN $17 THEN $18 ELSE address_id   END,
            employee_notes = CASE WHEN $19 THEN $20 ELSE employee_notes END,
            status         = COALESCE($21, status)
        WHERE inquiry_id = $1 AND id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(appointment_id)
    .bind(input.kind)
    .bind(input.scheduled_date)
    .bind(input.start_time.is_some())
    .bind(input.start_time.flatten())
    .bind(input.end_time.is_some())
    .bind(input.end_time.flatten())
    .bind(input.assignee_id.is_some())
    .bind(input.assignee_id.flatten())
    .bind(input.location.is_some())
    .bind(input.location.flatten())
    .bind(input.notes.is_some())
    .bind(input.notes.flatten())
    .bind(input.description.is_some())
    .bind(input.description.flatten())
    .bind(input.address_id.is_some())
    .bind(input.address_id.flatten())
    .bind(input.employee_notes.is_some())
    .bind(input.employee_notes.flatten())
    .bind(input.status)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete an appointment scoped to its inquiry. Returns rows affected.
pub(crate) async fn delete(
    pool: &PgPool,
    inquiry_id: Uuid,
    appointment_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM inquiry_appointments WHERE inquiry_id = $1 AND id = $2",
    )
    .bind(inquiry_id)
    .bind(appointment_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ── Crew (inquiry_appointment_employees) ────────────────────────────────────
// An appointment is exactly one day, so there is one row per (appointment,
// employee) — no per-day rows and no GROUP BY aggregation (unlike the
// calendar_item_employees machinery, which spans a date range).

/// A crew assignment on an appointment, joined with the employee's name.
///
/// Carries everything `EmployeeAssignmentSnapshot` needs; the caller derives
/// `employee_actual_hours` from the worker self-reported clock times.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AppointmentEmployeeRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub employee_clock_in: Option<DateTime<Utc>>,
    pub employee_clock_out: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub transport_mode: Option<String>,
    pub travel_costs_cents: Option<i64>,
    pub accommodation_cents: Option<i64>,
    pub misc_costs_cents: Option<i64>,
    pub meal_deduction: Option<String>,
}

const SELECT_CREW: &str = r#"
    SELECT iae.employee_id, e.first_name, e.last_name,
           iae.clock_in, iae.clock_out, iae.start_time, iae.end_time,
           COALESCE(iae.break_minutes, 0)::int AS break_minutes,
           iae.actual_hours::float8 AS actual_hours,
           iae.employee_clock_in, iae.employee_clock_out,
           iae.notes, iae.transport_mode, iae.travel_costs_cents,
           iae.accommodation_cents, iae.misc_costs_cents, iae.meal_deduction
    FROM inquiry_appointment_employees iae
    JOIN employees e ON e.id = iae.employee_id
"#;

/// List all crew assignments for an appointment, ordered by name.
pub(crate) async fn fetch_appointment_employees(
    pool: &PgPool,
    appointment_id: Uuid,
) -> Result<Vec<AppointmentEmployeeRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT_CREW} WHERE iae.appointment_id = $1 ORDER BY e.last_name, e.first_name"
    ))
    .bind(appointment_id)
    .fetch_all(pool)
    .await
}

/// Assign an employee to an appointment, seeding planned start/end from the
/// appointment's own times (falling back to 08:00–16:30). No-op if already assigned.
pub(crate) async fn insert_appointment_employee(
    pool: &PgPool,
    appointment_id: Uuid,
    employee_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiry_appointment_employees
            (appointment_id, employee_id, start_time, end_time)
        SELECT $1, $2,
               COALESCE(a.start_time, '08:00'::time),
               COALESCE(a.end_time,   '16:30'::time)
        FROM inquiry_appointments a
        WHERE a.id = $1
        ON CONFLICT (appointment_id, employee_id) DO NOTHING
        "#,
    )
    .bind(appointment_id)
    .bind(employee_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a crew assignment's hours / clock times / notes / travel expenses.
///
/// `actual_hours`: an explicit override wins; otherwise it is re-derived from the
/// effective post-update clock times + break (same self-healing pattern as
/// `calendar_item_repo::update_item_employee`).
// repository fn — args mirror DB columns
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_appointment_employee(
    pool: &PgPool,
    appointment_id: Uuid,
    employee_id: Uuid,
    clock_in: Option<NaiveTime>,
    clock_out: Option<NaiveTime>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours_override: Option<f64>,
    notes: Option<&str>,
    transport_mode: Option<&str>,
    travel_costs_cents: Option<i64>,
    accommodation_cents: Option<i64>,
    misc_costs_cents: Option<i64>,
    meal_deduction: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE inquiry_appointment_employees SET
            clock_in            = COALESCE($3, clock_in),
            clock_out           = COALESCE($4, clock_out),
            start_time          = COALESCE($5, start_time),
            end_time            = COALESCE($6, end_time),
            break_minutes       = COALESCE($7, break_minutes),
            actual_hours        = COALESCE(
                $8,
                CASE
                    WHEN COALESCE($3, clock_in) IS NOT NULL
                         AND COALESCE($4, clock_out) IS NOT NULL
                    THEN ROUND((
                        EXTRACT(EPOCH FROM (COALESCE($4, clock_out) - COALESCE($3, clock_in))) / 3600.0
                        - COALESCE($7, break_minutes, 0) / 60.0
                    )::numeric, 2)::float8
                    ELSE actual_hours
                END
            ),
            notes               = COALESCE($9, notes),
            transport_mode      = COALESCE($10, transport_mode),
            travel_costs_cents  = COALESCE($11, travel_costs_cents),
            accommodation_cents = COALESCE($12, accommodation_cents),
            misc_costs_cents    = COALESCE($13, misc_costs_cents),
            meal_deduction      = COALESCE($14, meal_deduction)
        WHERE appointment_id = $1 AND employee_id = $2
        "#,
    )
    .bind(appointment_id)
    .bind(employee_id)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(actual_hours_override)
    .bind(notes)
    .bind(transport_mode)
    .bind(travel_costs_cents)
    .bind(accommodation_cents)
    .bind(misc_costs_cents)
    .bind(meal_deduction)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// One crew assignment for the full-replace (`PUT`) path.
#[derive(Debug, Default)]
pub(crate) struct AppointmentEmployeeInput {
    pub employee_id: Uuid,
    pub notes: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub transport_mode: Option<String>,
    pub travel_costs_cents: Option<i64>,
    pub accommodation_cents: Option<i64>,
    pub misc_costs_cents: Option<i64>,
    pub meal_deduction: Option<String>,
}

/// Full-replace an appointment's crew in a single transaction: drop all existing
/// rows, then insert the supplied set. `actual_hours` is the explicit override,
/// or derived from clock times minus break when both clock times are present.
pub(crate) async fn put_appointment_employees(
    pool: &PgPool,
    appointment_id: Uuid,
    inputs: &[AppointmentEmployeeInput],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM inquiry_appointment_employees WHERE appointment_id = $1")
        .bind(appointment_id)
        .execute(&mut *tx)
        .await?;
    for i in inputs {
        sqlx::query(
            r#"
            INSERT INTO inquiry_appointment_employees
                (appointment_id, employee_id, notes, start_time, end_time,
                 clock_in, clock_out, break_minutes, actual_hours,
                 transport_mode, travel_costs_cents, accommodation_cents,
                 misc_costs_cents, meal_deduction)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    COALESCE($9,
                        CASE WHEN $6 IS NOT NULL AND $7 IS NOT NULL
                             THEN ROUND((EXTRACT(EPOCH FROM ($7 - $6)) / 3600.0
                                         - COALESCE($8, 0) / 60.0)::numeric, 2)::float8
                             ELSE NULL END),
                    $10, $11, $12, $13, $14)
            "#,
        )
        .bind(appointment_id)
        .bind(i.employee_id)
        .bind(i.notes.as_deref())
        .bind(i.start_time)
        .bind(i.end_time)
        .bind(i.clock_in)
        .bind(i.clock_out)
        .bind(i.break_minutes)
        .bind(i.actual_hours)
        .bind(i.transport_mode.as_deref())
        .bind(i.travel_costs_cents)
        .bind(i.accommodation_cents)
        .bind(i.misc_costs_cents)
        .bind(i.meal_deduction.as_deref())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Unassign an employee from an appointment. Returns rows affected.
pub(crate) async fn delete_appointment_employee(
    pool: &PgPool,
    appointment_id: Uuid,
    employee_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM inquiry_appointment_employees WHERE appointment_id = $1 AND employee_id = $2",
    )
    .bind(appointment_id)
    .bind(employee_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// One appointment enriched with its inquiry's customer name, for the calendar.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ScheduleAppointmentRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub kind: String,
    pub scheduled_date: NaiveDate,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub assignee_name: Option<String>,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub status: String,
    pub customer_name: Option<String>,
    /// Names of the assigned crew (comma-separated), for the calendar card.
    pub crew_names: Option<String>,
    /// Number of assigned crew members.
    pub crew_count: i64,
}

/// Fetch appointments falling in `[from, to]` for the calendar schedule view.
/// Cancelled appointments are excluded. Joined with the inquiry's customer name
/// so each renders connected to its inquiry.
pub(crate) async fn fetch_for_schedule_range(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<ScheduleAppointmentRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT a.id, a.inquiry_id, a.kind, a.scheduled_date, a.start_time, a.end_time,
               CASE WHEN e.id IS NULL THEN NULL
                    ELSE TRIM(CONCAT(e.first_name, ' ', e.last_name)) END AS assignee_name,
               a.location, a.notes, a.status,
               COALESCE(
                   NULLIF(TRIM(COALESCE(c.first_name, '') || ' ' || COALESCE(c.last_name, '')), ''),
                   c.name, c.email
               ) AS customer_name,
               crew.crew_names,
               COALESCE(crew.crew_count, 0) AS crew_count
        FROM inquiry_appointments a
        JOIN inquiries i  ON i.id = a.inquiry_id
        JOIN customers c  ON c.id = i.customer_id
        LEFT JOIN employees e ON e.id = a.assignee_id
        LEFT JOIN LATERAL (
            SELECT STRING_AGG(TRIM(CONCAT(ce.first_name, ' ', ce.last_name)), ', '
                              ORDER BY ce.last_name, ce.first_name) AS crew_names,
                   COUNT(*) AS crew_count
            FROM inquiry_appointment_employees iae
            JOIN employees ce ON ce.id = iae.employee_id
            WHERE iae.appointment_id = a.id
        ) crew ON TRUE
        WHERE a.scheduled_date BETWEEN $1 AND $2
          AND a.status <> 'cancelled'
        ORDER BY a.scheduled_date, a.start_time NULLS FIRST
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    async fn seed_inquiry(pool: &PgPool) -> Uuid {
        let customer_id = test_helpers::insert_test_customer(pool).await;
        let origin_id = test_helpers::insert_test_address(pool, "Musterstr. 1", "Hildesheim", "31134", None, None).await;
        let dest_id = test_helpers::insert_test_address(pool, "Zielstr. 5", "Hannover", "30159", None, None).await;
        test_helpers::insert_test_inquiry_full(pool, customer_id, origin_id, dest_id, "accepted", "termin", None).await
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn create_list_and_scope_by_inquiry(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let emp_id = test_helpers::insert_test_employee(&pool, "Max", "Mustermann").await;

        // Besichtigung on 3 Jul; the move is weeks later — non-consecutive.
        let visit = date(2026, 7, 3);
        let followup = date(2026, 7, 25);
        let a1 = create(
            &pool,
            inquiry_id,
            &AppointmentInput {
                scheduled_date: Some(visit),
                assignee_id: Some(Some(emp_id)),
                ..Default::default()
            },
        )
        .await
        .expect("create visit");
        create(
            &pool,
            inquiry_id,
            &AppointmentInput {
                kind: Some("nachtermin"),
                scheduled_date: Some(followup),
                ..Default::default()
            },
        )
        .await
        .expect("create followup");

        let rows = list_for_inquiry(&pool, inquiry_id).await.expect("list");
        assert_eq!(rows.len(), 2, "both appointments listed");
        assert_eq!(rows[0].scheduled_date, visit, "ordered earliest first");
        assert_eq!(rows[0].kind, "besichtigung", "default kind applied");
        assert_eq!(rows[0].assignee_name.as_deref(), Some("Max Mustermann"));
        assert_eq!(rows[1].kind, "nachtermin");

        // fetch_one is scoped to the inquiry: a foreign inquiry_id must not find it.
        let other = seed_inquiry(&pool).await;
        assert!(fetch_one(&pool, other, a1).await.expect("fetch_one").is_none());
        assert!(fetch_one(&pool, inquiry_id, a1).await.expect("fetch_one").is_some());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn schedule_range_excludes_cancelled_and_carries_customer(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let keep = date(2026, 7, 3);
        let drop = date(2026, 7, 4);
        create(&pool, inquiry_id, &AppointmentInput { scheduled_date: Some(keep), ..Default::default() })
            .await
            .expect("create kept");
        create(
            &pool,
            inquiry_id,
            &AppointmentInput { scheduled_date: Some(drop), status: Some("cancelled"), ..Default::default() },
        )
        .await
        .expect("create cancelled");

        let rows = fetch_for_schedule_range(&pool, date(2026, 7, 1), date(2026, 7, 31))
            .await
            .expect("schedule range");
        assert_eq!(rows.len(), 1, "cancelled appointment excluded from schedule");
        assert_eq!(rows[0].scheduled_date, keep);
        assert_eq!(rows[0].inquiry_id, inquiry_id, "linked back to its inquiry");
        assert!(rows[0].customer_name.is_some(), "customer name joined in");

        // Out-of-range date is not returned.
        let empty = fetch_for_schedule_range(&pool, date(2026, 8, 1), date(2026, 8, 31))
            .await
            .expect("empty range");
        assert!(empty.is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_sets_and_clears_fields(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let emp_id = test_helpers::insert_test_employee(&pool, "Erika", "Musterfrau").await;
        let appt = create(
            &pool,
            inquiry_id,
            &AppointmentInput {
                scheduled_date: Some(date(2026, 7, 3)),
                assignee_id: Some(Some(emp_id)),
                notes: Some(Some("Bitte anrufen")),
                ..Default::default()
            },
        )
        .await
        .expect("create");

        // Change kind, mark done, and clear the assignee — leave notes untouched.
        let affected = update(
            &pool,
            inquiry_id,
            appt,
            &AppointmentInput {
                kind: Some("besichtigung_final"),
                status: Some("done"),
                assignee_id: Some(None), // explicit clear
                ..Default::default()
            },
        )
        .await
        .expect("update");
        assert_eq!(affected, 1);

        let row = fetch_one(&pool, inquiry_id, appt).await.expect("fetch").expect("exists");
        assert_eq!(row.kind, "besichtigung_final");
        assert_eq!(row.status, "done");
        assert!(row.assignee_id.is_none(), "assignee cleared");
        assert_eq!(row.notes.as_deref(), Some("Bitte anrufen"), "untouched field preserved");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn delete_is_scoped_and_removes(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let appt = create(&pool, inquiry_id, &AppointmentInput { scheduled_date: Some(date(2026, 7, 3)), ..Default::default() })
            .await
            .expect("create");

        // Wrong inquiry_id deletes nothing.
        let other = seed_inquiry(&pool).await;
        assert_eq!(delete(&pool, other, appt).await.expect("delete"), 0);
        assert_eq!(delete(&pool, inquiry_id, appt).await.expect("delete"), 1);
        assert!(fetch_one(&pool, inquiry_id, appt).await.expect("fetch").is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn crew_assign_update_and_derive_hours(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let emp = test_helpers::insert_test_employee(&pool, "Paul", "Packer").await;
        let appt = create(
            &pool,
            inquiry_id,
            &AppointmentInput {
                kind: Some("halteverbot"),
                scheduled_date: Some(date(2026, 7, 10)),
                ..Default::default()
            },
        )
        .await
        .expect("create appt");

        // Assign, then re-assign (idempotent — ON CONFLICT DO NOTHING).
        insert_appointment_employee(&pool, appt, emp).await.expect("assign");
        insert_appointment_employee(&pool, appt, emp).await.expect("assign again");
        let crew = fetch_appointment_employees(&pool, appt).await.expect("crew");
        assert_eq!(crew.len(), 1, "single row per (appointment, employee)");
        assert_eq!(crew[0].first_name, "Paul");

        // Enter clock times → actual_hours derived (08:00–16:30 − 30min = 8.0).
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let affected = update_appointment_employee(
            &pool, appt, emp,
            Some(t(8, 0)), Some(t(16, 30)), None, None, Some(30),
            None, Some("fertig"), None, None, None, None, None,
        )
        .await
        .expect("update crew");
        assert_eq!(affected, 1);
        let crew = fetch_appointment_employees(&pool, appt).await.expect("crew");
        assert_eq!(crew[0].actual_hours, Some(8.0), "hours derived from clock times");
        assert_eq!(crew[0].notes.as_deref(), Some("fertig"));

        // Deleting the appointment cascades to its crew.
        delete(&pool, inquiry_id, appt).await.expect("delete appt");
        assert!(fetch_appointment_employees(&pool, appt).await.expect("crew").is_empty());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn put_crew_full_replaces(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool).await;
        let a = test_helpers::insert_test_employee(&pool, "Anna", "A").await;
        let b = test_helpers::insert_test_employee(&pool, "Bert", "B").await;
        let appt = create(&pool, inquiry_id, &AppointmentInput { scheduled_date: Some(date(2026, 7, 11)), ..Default::default() })
            .await
            .expect("create");

        put_appointment_employees(
            &pool,
            appt,
            &[AppointmentEmployeeInput { employee_id: a, actual_hours: Some(4.0), ..Default::default() }],
        )
        .await
        .expect("put a");
        assert_eq!(fetch_appointment_employees(&pool, appt).await.unwrap().len(), 1);

        // Full replace drops A, adds B.
        put_appointment_employees(
            &pool,
            appt,
            &[AppointmentEmployeeInput { employee_id: b, ..Default::default() }],
        )
        .await
        .expect("put b");
        let crew = fetch_appointment_employees(&pool, appt).await.unwrap();
        assert_eq!(crew.len(), 1);
        assert_eq!(crew[0].employee_id, b, "A replaced by B");
    }

    /// Paid appointment hours must surface in the worker dashboard aggregations:
    /// the monthly total, the schedule list, and the pending-hours modal.
    #[sqlx::test(migrations = "../../migrations")]
    async fn appointment_hours_feed_worker_dashboard(pool: PgPool) {
        use crate::repositories::employee_repo;

        let inquiry_id = seed_inquiry(&pool).await;
        let emp = test_helpers::insert_test_employee(&pool, "Hank", "Helper").await;
        // A past date, after the pending-hours cutoff (June 2026).
        let day = date(2026, 7, 10);
        let appt = create(
            &pool,
            inquiry_id,
            &AppointmentInput { kind: Some("halteverbot"), scheduled_date: Some(day), ..Default::default() },
        )
        .await
        .expect("create");
        insert_appointment_employee(&pool, appt, emp).await.expect("assign");

        // Before any hours: appears in the schedule and pending list.
        let sched = employee_repo::fetch_schedule_appointments(&pool, emp, date(2026, 7, 1), date(2026, 7, 31))
            .await
            .expect("schedule");
        assert_eq!(sched.len(), 1);
        assert_eq!(sched[0].appointment_id, appt);
        assert_eq!(sched[0].inquiry_id, inquiry_id, "stays linked to its inquiry");

        let pending = employee_repo::fetch_pending_hours(&pool, emp, date(2026, 7, 22))
            .await
            .expect("pending");
        assert!(
            pending.iter().any(|p| p.entry_type == "appointment" && p.appointment_id == Some(appt)),
            "unlogged past appointment is pending"
        );

        // Admin enters clock times → hours land in the monthly total.
        let t = |h| NaiveTime::from_hms_opt(h, 0, 0).unwrap();
        update_appointment_employee(
            &pool, appt, emp, Some(t(8)), Some(t(12)), None, None, Some(0),
            None, None, None, None, None, None, None,
        )
        .await
        .expect("clock");

        let sums = employee_repo::fetch_month_hours(&pool, emp, date(2026, 7, 1), date(2026, 7, 31))
            .await
            .expect("month hours");
        assert_eq!(sums.actual, Some(4.0), "4h of paid appointment work counted");

        let entries = employee_repo::fetch_hours_entries(&pool, emp, date(2026, 7, 1), date(2026, 7, 31))
            .await
            .expect("hours entries");
        assert!(
            entries.iter().any(|e| e.entry_type == "appointment" && e.actual_hours == Some(4.0)),
            "appointment leg present in hours entries"
        );
    }
}
