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
    pub notes: Option<String>,
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
    pub notes: Option<Option<&'a str>>,
    pub status: Option<&'a str>,
}

const SELECT_JOINED: &str = r#"
    SELECT a.id, a.kind, a.scheduled_date, a.start_time, a.end_time,
           a.assignee_id,
           CASE WHEN e.id IS NULL THEN NULL
                ELSE TRIM(CONCAT(e.first_name, ' ', e.last_name)) END AS assignee_name,
           a.location, a.notes, a.status, a.created_at
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
             assignee_id, location, notes, status)
        VALUES ($1,
                COALESCE($2, 'besichtigung'),
                $3, $4, $5, $6, $7, $8,
                COALESCE($9, 'scheduled'))
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
    .bind(input.notes.flatten())
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
            status         = COALESCE($15, status)
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
               ) AS customer_name
        FROM inquiry_appointments a
        JOIN inquiries i  ON i.id = a.inquiry_id
        JOIN customers c  ON c.id = i.customer_id
        LEFT JOIN employees e ON e.id = a.assignee_id
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
}
