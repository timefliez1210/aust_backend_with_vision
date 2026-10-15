//! Bridge impl for `CalendarService`.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{
    AvailableSlot, CalendarItem, CalendarItemPatch, CalendarService, CrewMember,
    EmployeeSchedulePatch, EmployeeWorkloadEntry, ServiceError,
};

use crate::repositories::{calendar_item_repo, inquiry_appointment_repo, inquiry_repo};
use crate::services::bridge::InquiryServiceImpl;
use crate::services::calendar_item_update::{self, UpdateItemBody};
use crate::services::inquiry_update::{self, UpdateInquiryRequest};
use crate::ApiError;
use aust_core::services::InquiryService;

/// Row shape for a single `calendar_items` read with time + location columns.
type TerminRow = (
    Uuid,
    String,
    String,
    Option<NaiveDate>,
    Option<NaiveDate>,
    Option<NaiveTime>,
    Option<NaiveTime>,
    Option<String>,
);

pub struct CalendarServiceImpl {
    pool: PgPool,
}

impl CalendarServiceImpl {
    /// The dates an employee is actually rostered on for one parent, in order.
    ///
    /// Covers a move and a Termin. A Zusatztermin is one day by construction — its
    /// junction has no `job_date` at all — so it never needs a day selector.
    async fn assigned_days(
        &self,
        parent_id: Uuid,
        employee_id: Uuid,
    ) -> Result<Vec<chrono::NaiveDate>, ServiceError> {
        let rows: Vec<(chrono::NaiveDate,)> = sqlx::query_as(
            r#"
            SELECT job_date FROM inquiry_employees
             WHERE inquiry_id = $1 AND employee_id = $2
            UNION
            SELECT job_date FROM calendar_item_employees
             WHERE calendar_item_id = $1 AND employee_id = $2
            ORDER BY 1
            "#,
        )
        .bind(parent_id)
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(rows.into_iter().map(|(d,)| d).collect())
    }

    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reject unknown or inactive employees up front, as the admin assign does, so
    /// a bad ID fails the call instead of being written or skipped silently.
    async fn ensure_active_crew(&self, crew: &[Uuid]) -> Result<(), ServiceError> {
        for employee_id in crew {
            match inquiry_repo::check_employee_active(&self.pool, *employee_id)
                .await
                .map_err(super::map_sqlx)?
            {
                None => return Err(ServiceError::NotFound(format!("Mitarbeiter {employee_id} nicht gefunden"))),
                Some(false) => return Err(ServiceError::Validation(format!("Mitarbeiter {employee_id} ist inaktiv"))),
                Some(true) => {}
            }
        }
        Ok(())
    }

    /// Edit a calendar item through the admin PATCH path (`calendar_item_update`).
    async fn patch_item(&self, id: Uuid, body: UpdateItemBody) -> Result<(), ServiceError> {
        calendar_item_update::update_item(&self.pool, id, body)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                ApiError::NotFound(msg) => ServiceError::NotFound(msg),
                ApiError::Validation(msg) | ApiError::BadRequest(msg) => ServiceError::Validation(msg),
                other => ServiceError::Db(anyhow::anyhow!(other.to_string())),
            })
    }
}

#[async_trait]
impl CalendarService for CalendarServiceImpl {
    async fn get_range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<CalendarItem>, ServiceError> {
        // Internal calendar items overlapping the range. Use overlap logic
        // (start <= to AND end >= from) so multi-day items that begin before the
        // window are still returned.
        type CalRow = (
            Uuid,
            String,
            String,
            Option<NaiveDate>,
            Option<NaiveDate>,
            Option<NaiveTime>,
            Option<NaiveTime>,
            Option<String>,
        );
        let cal_rows: Vec<CalRow> = sqlx::query_as(
            r#"
                SELECT id, title, category, scheduled_date, end_date,
                       start_time, end_time, location
                FROM calendar_items
                WHERE scheduled_date <= $2
                  AND COALESCE(end_date, scheduled_date) >= $1
                ORDER BY scheduled_date ASC, start_time ASC
                "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Actual moving jobs (inquiries with a scheduled date) overlapping the
        // range. Without this the assistant's calendar was blind to every real
        // Umzug/Montage — only internal calendar_items showed up.
        type InqRow = (
            Uuid,
            String,
            Option<String>,
            Option<NaiveDate>,
            Option<NaiveDate>,
            Option<NaiveTime>,
            Option<NaiveTime>,
            Option<String>,
        );
        let inq_rows: Vec<InqRow> = sqlx::query_as(
            r#"
                SELECT
                    i.id,
                    COALESCE(
                        NULLIF(TRIM(COALESCE(c.first_name,'') || ' ' || COALESCE(c.last_name,'')), ''),
                        c.name, c.email, 'Anfrage'
                    ) AS title,
                    i.service_type,
                    i.scheduled_date,
                    i.end_date,
                    i.start_time,
                    i.end_time,
                    NULLIF(TRIM(
                        COALESCE(a.street, '') || ' ' || COALESCE(a.house_number, '') || ', ' ||
                        COALESCE(a.postal_code, '') || ' ' || COALESCE(a.city, '')
                    ), ', ') AS location
                FROM inquiries i
                JOIN customers c ON c.id = i.customer_id
                LEFT JOIN addresses a ON a.id = i.origin_address_id
                WHERE i.scheduled_date IS NOT NULL
                  AND i.scheduled_date <= $2
                  AND COALESCE(i.end_date, i.scheduled_date) >= $1
                  AND i.status NOT IN ('cancelled', 'rejected', 'expired')
                ORDER BY i.scheduled_date ASC
                "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let mut items: Vec<CalendarItem> = cal_rows
            .into_iter()
            .map(
                |(id, title, category, scheduled_date, end_date, start_time, end_time, location)| {
                    CalendarItem {
                        id,
                        title,
                        category,
                        scheduled_date,
                        end_date,
                        start_time,
                        end_time,
                        location,
                        kind: "termin".to_string(),
                        inquiry_id: None,
                    }
                },
            )
            .collect();
        items.extend(inq_rows.into_iter().map(
            |(id, title, service_type, scheduled_date, end_date, start_time, end_time, location)| {
                CalendarItem {
                    id,
                    title,
                    category: service_type.unwrap_or_else(|| "umzug".to_string()),
                    scheduled_date,
                    end_date,
                    start_time,
                    end_time,
                    location,
                    kind: "auftrag".to_string(),
                    inquiry_id: None,
                }
            },
        ));

        // Light appointments (Besichtigung etc.) hanging off an inquiry. The admin
        // UI renders these (routes::calendar), so leaving them out here made the
        // assistant blind to Besichtigungen it had just created — which drove it to
        // create a duplicate standalone calendar_item on top.
        let appt_rows = inquiry_appointment_repo::fetch_for_schedule_range(&self.pool, from, to)
            .await
            .map_err(super::map_sqlx)?;
        items.extend(appt_rows.into_iter().map(|r| {
            let title = match r.customer_name.as_deref() {
                Some(name) => format!("{} — {name}", r.kind),
                None => r.kind.clone(),
            };
            CalendarItem {
                id: r.id,
                title,
                category: r.kind,
                scheduled_date: Some(r.scheduled_date),
                end_date: None,
                start_time: r.start_time,
                end_time: r.end_time,
                location: r.location,
                kind: "besichtigung".to_string(),
                inquiry_id: Some(r.inquiry_id),
            }
        }));

        items.sort_by(|a, b| {
            a.scheduled_date
                .cmp(&b.scheduled_date)
                .then(a.start_time.cmp(&b.start_time))
        });
        Ok(items)
    }

    async fn find_available_slots(
        &self,
        earliest: NaiveDate,
        latest: NaiveDate,
    ) -> Result<Vec<AvailableSlot>, ServiceError> {
        // Count active employees and subtract those assigned on each date.
        let total_active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM employees WHERE active = true",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // For each date in range, count assigned employees (inquiries + calendar items).
        let rows: Vec<(NaiveDate, i64)> = sqlx::query_as(
            r#"
            SELECT d::date, COUNT(DISTINCT e.employee_id) AS busy
            FROM generate_series($1::date, $2::date, '1 day'::interval) d
            LEFT JOIN (
                SELECT job_date AS work_date, employee_id FROM inquiry_employees
                UNION ALL
                SELECT job_date AS work_date, employee_id FROM calendar_item_employees
            ) e ON e.work_date = d::date
            GROUP BY d::date
            ORDER BY d::date
            "#,
        )
        .bind(earliest)
        .bind(latest)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(date, busy)| AvailableSlot {
                date,
                available_crew: (total_active - busy).max(0) as i32,
            })
            .collect())
    }

    async fn create_item(
        &self,
        scheduled_date: NaiveDate,
        category: &str,
        title: &str,
        notes: Option<&str>,
        end_date: Option<NaiveDate>,
        start_time: Option<NaiveTime>,
        end_time: Option<NaiveTime>,
        location: Option<&str>,
    ) -> Result<CalendarItem, ServiceError> {
        let start = start_time.unwrap_or_else(|| NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default());

        let new_id = calendar_item_repo::insert_item(
            &self.pool,
            title,
            notes,
            category,
            location,
            Some(scheduled_date),
            start,
            end_time,
            0.0,
            None,
            end_date,
        )
        .await
        .map_err(super::map_sqlx)?;

        Ok(CalendarItem {
            id: new_id,
            title: title.to_string(),
            category: category.to_string(),
            scheduled_date: Some(scheduled_date),
            end_date,
            start_time: Some(start),
            end_time,
            location: location.map(str::to_string),
            kind: "termin".to_string(),
            inquiry_id: None,
        })
    }

    async fn update_item(
        &self,
        id: Uuid,
        patch: CalendarItemPatch,
    ) -> Result<CalendarItem, ServiceError> {
        // Same path as the admin calendar's PATCH (span + crew move with the date).
        self.patch_item(
            id,
            UpdateItemBody {
                title: patch.title,
                description: patch.notes,
                category: patch.category,
                location: patch.location,
                scheduled_date: patch.scheduled_date,
                start_time: patch.start_time,
                end_time: patch.end_time,
                end_date: patch.end_date.map(|d| serde_json::json!(d.format("%Y-%m-%d").to_string())),
                ..Default::default()
            },
        )
        .await?;

        let row: Option<TerminRow> = sqlx::query_as(
            "SELECT id, title, category, scheduled_date, end_date, start_time, end_time, location \
             FROM calendar_items WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, title, category, scheduled_date, end_date, start_time, end_time, location) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Kalendereintrag {id}")))?;

        Ok(CalendarItem {
            id,
            title,
            category,
            scheduled_date,
            end_date,
            start_time,
            end_time,
            location,
            kind: "termin".to_string(),
            inquiry_id: None,
        })
    }

    async fn delete_item(&self, id: Uuid) -> Result<(), ServiceError> {
        calendar_item_repo::delete_item(&self.pool, id)
            .await
            .map_err(super::map_sqlx)?;
        Ok(())
    }

    /// Put an inquiry on a day (first scheduling or a move) and staff it.
    ///
    /// Goes through `inquiry_update::update_inquiry`, the admin PATCH path, so the
    /// existing crew moves with the job and `status.changed` is emitted. The job
    /// appears in the calendar as itself (the Auftrag) — this used to also insert
    /// a free-standing "moving" calendar item with no inquiry link and no crew,
    /// which put every Josie-scheduled move in the calendar twice.
    async fn schedule_inquiry(
        &self,
        inquiry_id: Uuid,
        date: NaiveDate,
        crew: Vec<Uuid>,
        notes: Option<&str>,
        start_time: Option<NaiveTime>,
        end_time: Option<NaiveTime>,
    ) -> Result<CalendarItem, ServiceError> {
        // Check the crew before touching the job, as the admin assign does.
        self.ensure_active_crew(&crew).await?;

        let events = aust_core::events::EventEmitter::new(self.pool.clone());
        let request = UpdateInquiryRequest {
            scheduled_date: Some(date.format("%Y-%m-%d").to_string()),
            status: Some("scheduled".to_string()),
            start_time,
            end_time,
            ..Default::default()
        };
        let inquiry = inquiry_update::update_inquiry(&self.pool, &events, inquiry_id, request)
            .await
            .map_err(|e| match e {
                ApiError::NotFound(msg) => ServiceError::NotFound(msg),
                ApiError::Validation(msg) | ApiError::BadRequest(msg) => ServiceError::Validation(msg),
                other => ServiceError::Db(anyhow::anyhow!(other.to_string())),
            })?;

        // Staff the day, carrying the job's shift times onto each crew row.
        for employee_id in &crew {
            sqlx::query(
                r#"
                INSERT INTO inquiry_employees (inquiry_id, employee_id, job_date, start_time, end_time)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (inquiry_id, employee_id, job_date) DO UPDATE SET
                    start_time = COALESCE(EXCLUDED.start_time, inquiry_employees.start_time),
                    end_time   = COALESCE(EXCLUDED.end_time, inquiry_employees.end_time)
                "#,
            )
            .bind(inquiry_id)
            .bind(employee_id)
            .bind(date)
            .bind(start_time)
            .bind(end_time)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        }

        if let Some(text) = notes.map(str::trim).filter(|t| !t.is_empty()) {
            InquiryServiceImpl::new(self.pool.clone())
                .add_note(inquiry_id, text, "assistant")
                .await?;
        }

        Ok(CalendarItem {
            id: inquiry_id,
            title: inquiry.customer.as_ref().and_then(|c| c.name.clone()).unwrap_or_else(|| "Umzug".to_string()),
            category: "moving".to_string(),
            scheduled_date: Some(date),
            end_date: None,
            start_time: Some(inquiry.start_time),
            end_time: Some(inquiry.end_time),
            location: None,
            kind: "auftrag".to_string(),
            inquiry_id: Some(inquiry_id),
        })
    }

    async fn reassign_termin(
        &self,
        termin_id: Uuid,
        new_date: Option<NaiveDate>,
        new_crew: Option<Vec<Uuid>>,
    ) -> Result<CalendarItem, ServiceError> {
        if let Some(date) = new_date {
            // Admin PATCH path: the span and the existing crew move with the date.
            self.patch_item(termin_id, UpdateItemBody { scheduled_date: Some(date), ..Default::default() })
                .await?;
        }

        if let Some(crew) = new_crew {
            // Replace the crew SET while preserving rows that stay assigned —
            // DELETE-all + re-INSERT wiped entered clock times/hours (same
            // class as the 2026-06-10 PUT-wipe incident; see set_inquiry_crew).
            sqlx::query(
                "DELETE FROM calendar_item_employees WHERE calendar_item_id = $1 AND NOT (employee_id = ANY($2))",
            )
            .bind(termin_id)
            .bind(&crew)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

            let date_row: Option<(Option<NaiveDate>,)> = sqlx::query_as(
                "SELECT scheduled_date FROM calendar_items WHERE id = $1",
            )
            .bind(termin_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

            if let Some((Some(_),)) = date_row {
                // Stale rows OUTSIDE the item's current date span are removed,
                // but only when they carry no recorded hours — rows with clock
                // data are historical fact. In-span rows (multi-day items) stay.
                let _ = sqlx::query(
                    r#"
                    DELETE FROM calendar_item_employees cie
                    USING calendar_items ci
                    WHERE cie.calendar_item_id = $1 AND ci.id = $1
                      AND cie.job_date NOT BETWEEN ci.scheduled_date
                          AND COALESCE(ci.end_date, ci.scheduled_date)
                      AND cie.clock_in IS NULL AND cie.clock_out IS NULL AND cie.actual_hours IS NULL
                    "#,
                )
                .bind(termin_id)
                .execute(&self.pool)
                .await;
                // Every day of the span, with the item's times — as the admin assign does.
                for employee_id in &crew {
                    calendar_item_repo::insert_item_employee(&self.pool, termin_id, *employee_id)
                        .await
                        .map_err(super::map_sqlx)?;
                }
            }
        }

        let row: Option<TerminRow> = sqlx::query_as(
            "SELECT id, title, category, scheduled_date, end_date, start_time, end_time, location \
             FROM calendar_items WHERE id = $1",
        )
        .bind(termin_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, title, category, scheduled_date, end_date, start_time, end_time, location) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Termin {termin_id}")))?;

        Ok(CalendarItem {
            id,
            title,
            category,
            scheduled_date,
            end_date,
            start_time,
            end_time,
            location,
            kind: "termin".to_string(),
            inquiry_id: None,
        })
    }

    async fn cancel_termin(&self, id: Uuid, _reason: &str) -> Result<(), ServiceError> {
        self.patch_item(id, UpdateItemBody { status: Some("cancelled".to_string()), ..Default::default() })
            .await
    }

    async fn assign_employee(
        &self,
        calendar_item_id: Uuid,
        employee_id: Uuid,
    ) -> Result<(), ServiceError> {
        // The admin assign path: checks both sides exist and staffs EVERY day of a
        // multi-day Termin with the item's times. This used to insert day one only,
        // without times, so the worker saw half a job.
        if !calendar_item_repo::item_exists(&self.pool, calendar_item_id).await.map_err(super::map_sqlx)? {
            return Err(ServiceError::NotFound(format!("Kalendereintrag {calendar_item_id}")));
        }
        if !calendar_item_repo::employee_exists(&self.pool, employee_id).await.map_err(super::map_sqlx)? {
            return Err(ServiceError::NotFound(format!("Mitarbeiter {employee_id}")));
        }
        calendar_item_repo::insert_item_employee(&self.pool, calendar_item_id, employee_id)
            .await
            .map_err(super::map_sqlx)
    }

    async fn get_employee_assignments(
        &self,
        employee_id: Uuid,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError> {
        // Query both inquiry assignments and calendar item assignments.
        type AssignmentRow = (
            NaiveDate,
            Option<Uuid>,
            Option<Uuid>,
            String,
            String,
            Option<NaiveTime>,
            Option<NaiveTime>,
            Option<f64>,
        );
        let rows: Vec<AssignmentRow> =
            sqlx::query_as(
                r#"
                SELECT ie.job_date, ie.inquiry_id, NULL::uuid AS calendar_item_id,
                       COALESCE(c.name, 'Umzug') AS title, 'moving' AS category,
                       ie.clock_in, ie.clock_out, ie.actual_hours::float8 AS actual_hours
                FROM inquiry_employees ie
                JOIN inquiries i ON ie.inquiry_id = i.id
                LEFT JOIN customers c ON i.customer_id = c.id
                WHERE ie.employee_id = $1 AND ie.job_date BETWEEN $2 AND $3
                UNION ALL
                SELECT cie.job_date, NULL::uuid, cie.calendar_item_id,
                       ci.title, ci.category,
                       cie.clock_in, cie.clock_out, cie.actual_hours::float8
                FROM calendar_item_employees cie
                JOIN calendar_items ci ON cie.calendar_item_id = ci.id
                WHERE cie.employee_id = $1 AND cie.job_date BETWEEN $2 AND $3
                ORDER BY job_date
                "#,
            )
            .bind(employee_id)
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(date, inquiry_id, calendar_item_id, title, category, clock_in, clock_out, actual_hours)| {
                EmployeeWorkloadEntry {
                    date,
                    inquiry_id,
                    calendar_item_id,
                    title,
                    category,
                    clock_in,
                    clock_out,
                    actual_hours,
                }
            })
            .collect())
    }

    async fn get_assigned_crew(&self, id: Uuid) -> Result<Vec<CrewMember>, ServiceError> {
        // The id may be a calendar_item OR an inquiry — crew lives in two
        // separate junction tables. Check both so the caller (the assistant)
        // doesn't have to know which kind of id it holds. This is the read path
        // that prevents the agent from confabulating a crew list.
        type CrewRow = (
            Uuid,
            String,
            String,
            NaiveDate,
            Option<NaiveTime>,
            Option<NaiveTime>,
            Option<f64>,
            Option<NaiveTime>,
            Option<NaiveTime>,
            Option<i32>,
            Option<f64>,
            String,
        );
        let rows: Vec<CrewRow> = sqlx::query_as(
            r#"
            SELECT e.id   AS employee_id,
                   e.first_name AS first_name,
                   e.last_name  AS last_name,
                   cie.job_date AS job_date,
                   cie.start_time AS start_time,
                   cie.end_time AS end_time,
                   cie.planned_hours::float8 AS planned_hours,
                   cie.clock_in AS clock_in,
                   cie.clock_out AS clock_out,
                   cie.break_minutes AS break_minutes,
                   cie.actual_hours::float8 AS actual_hours,
                   'termin'::text AS source
            FROM calendar_item_employees cie
            JOIN employees e ON e.id = cie.employee_id
            WHERE cie.calendar_item_id = $1
            UNION ALL
            SELECT e.id, e.first_name, e.last_name, ie.job_date,
                   ie.start_time, ie.end_time, ie.planned_hours::float8,
                   ie.clock_in, ie.clock_out, ie.break_minutes, ie.actual_hours::float8,
                   'auftrag'::text
            FROM inquiry_employees ie
            JOIN employees e ON e.id = ie.employee_id
            WHERE ie.inquiry_id = $1
            UNION ALL
            -- A Zusatztermin (Halteverbot aufbauen, Besichtigung) carries a paid crew
            -- too. It was missing here, so the assistant saw an empty crew for one —
            -- and since set_employee_schedule returns its result through this function,
            -- a successful write to an appointment read back as "no crew".
            -- The junction has no job_date: an appointment is exactly one day, so the
            -- date comes from the appointment itself.
            SELECT e.id, e.first_name, e.last_name, a.scheduled_date,
                   iae.start_time, iae.end_time, iae.planned_hours::float8,
                   iae.clock_in, iae.clock_out, iae.break_minutes, iae.actual_hours::float8,
                   'zusatztermin'::text
            FROM inquiry_appointment_employees iae
            JOIN inquiry_appointments a ON a.id = iae.appointment_id
            JOIN employees e ON e.id = iae.employee_id
            WHERE iae.appointment_id = $1
            ORDER BY last_name, first_name
            "#,
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(
                |(employee_id, first_name, last_name, job_date, start_time, end_time, planned_hours, clock_in, clock_out, break_minutes, actual_hours, source)| {
                    CrewMember {
                        employee_id,
                        first_name,
                        last_name,
                        job_date,
                        start_time,
                        end_time,
                        planned_hours,
                        clock_in,
                        clock_out,
                        break_minutes,
                        actual_hours,
                        source,
                    }
                },
            )
            .collect())
    }

    async fn set_inquiry_crew(
        &self,
        inquiry_id: Uuid,
        crew: Vec<Uuid>,
        date: Option<NaiveDate>,
    ) -> Result<Vec<CrewMember>, ServiceError> {
        // Resolve the job date: explicit arg wins, otherwise the inquiry's own
        // scheduled_date. This is what stops crew rows from being stranded on a
        // stale date (the 2026-05-27 vs 2026-06-12 Schauer bug).
        let inq: Option<(Option<NaiveDate>, Option<NaiveDate>)> =
            sqlx::query_as("SELECT scheduled_date, end_date FROM inquiries WHERE id = $1")
                .bind(inquiry_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(super::map_sqlx)?;
        let (scheduled, end) = inq
            .ok_or_else(|| ServiceError::NotFound(format!("Anfrage {inquiry_id}")))?;
        self.ensure_active_crew(&crew).await?;
        let first = date.or(scheduled).ok_or_else(|| {
            ServiceError::Validation(
                "Anfrage hat kein geplantes Datum — bitte Datum angeben.".to_string(),
            )
        })?;

        // A crew is booked for the whole job, not for its first morning. Writing only
        // one date and deleting the employee's rows on every other date stripped days 2
        // and 3 of a Mon-Wed move the moment anyone confirmed the crew. An explicit
        // `date` still means that one day; without one, the crew covers the job's whole
        // window.
        let last = if date.is_some() {
            first
        } else {
            end.filter(|e| *e >= first).unwrap_or(first)
        };
        let job_dates: Vec<NaiveDate> = std::iter::successors(Some(first), |d| {
            d.succ_opt().filter(|next| *next <= last)
        })
        .collect();

        // Replace the crew SET while preserving rows that stay assigned —
        // DELETE-all + re-INSERT wiped entered clock times/hours on existing
        // rows (same class as the 2026-06-10 PUT-wipe incident). Rows for
        // employees no longer in the list are removed; surviving rows keep all
        // their data via ON CONFLICT DO NOTHING.
        let mut tx = self.pool.begin().await.map_err(super::map_sqlx)?;
        sqlx::query(
            "DELETE FROM inquiry_employees WHERE inquiry_id = $1 AND NOT (employee_id = ANY($2))",
        )
        .bind(inquiry_id)
        .bind(&crew)
        .execute(&mut *tx)
        .await
        .map_err(super::map_sqlx)?;
        // Stale rows OUTSIDE the job's dates (the original stranded-date bug) are only
        // removed when they carry no recorded hours — rows with clock data are
        // historical fact and stay.
        sqlx::query(
            r#"
            DELETE FROM inquiry_employees
            WHERE inquiry_id = $1 AND employee_id = ANY($2) AND job_date <> ALL($3)
              AND clock_in IS NULL AND clock_out IS NULL AND actual_hours IS NULL
            "#,
        )
        .bind(inquiry_id)
        .bind(&crew)
        .bind(&job_dates)
        .execute(&mut *tx)
        .await
        .map_err(super::map_sqlx)?;
        for employee_id in &crew {
            for day in &job_dates {
                sqlx::query(
                    r#"
                    INSERT INTO inquiry_employees (inquiry_id, employee_id, job_date)
                    VALUES ($1, $2, $3)
                    ON CONFLICT (inquiry_id, employee_id, job_date) DO NOTHING
                    "#,
                )
                .bind(inquiry_id)
                .bind(employee_id)
                .bind(day)
                .execute(&mut *tx)
                .await
                .map_err(super::map_sqlx)?;
            }
        }
        tx.commit().await.map_err(super::map_sqlx)?;

        // Return the freshly written crew so the caller can confirm the result.
        self.get_assigned_crew(inquiry_id).await
    }

    async fn set_employee_schedule(
        &self,
        parent_id: Uuid,
        employee_id: Uuid,
        patch: EmployeeSchedulePatch,
    ) -> Result<Vec<CrewMember>, ServiceError> {
        // The parent may be an inquiry (auftrag) or a calendar item (termin).
        // Try both junction tables; whichever holds the row gets updated.
        // actual_hours self-heals from the effective post-update clock times +
        // break (same derivation as the admin PATCH paths in inquiry_repo /
        // calendar_item_repo).
        //
        // `patch.job_date` selects WHICH day is being edited; it never moves a row.
        // Without it this wrote every day of a multi-day job at once — setting one
        // clock-in on day 2 of a Mon-Wed move rewrote all three days — and when a date
        // was supplied it tried to force all three rows onto it, aborting on the
        // (inquiry, employee, job_date) unique key. Moving crew between dates is
        // rescheduling, which shifts the whole job.
        let days = self.assigned_days(parent_id, employee_id).await?;
        if patch.job_date.is_none() && days.len() > 1 {
            let list: Vec<String> = days.iter().map(|d| d.format("%d.%m.%Y").to_string()).collect();
            return Err(ServiceError::Validation(format!(
                "Dieser Einsatz geht über mehrere Tage ({}). Bitte den Tag angeben, der geändert werden soll.",
                list.join(", ")
            )));
        }
        if let Some(day) = patch.job_date
            && !days.is_empty()
            && !days.contains(&day)
        {
            let list: Vec<String> = days.iter().map(|d| d.format("%d.%m.%Y").to_string()).collect();
            return Err(ServiceError::Validation(format!(
                "Der Mitarbeiter ist am {} nicht eingeteilt (eingeteilt: {}).",
                day.format("%d.%m.%Y"),
                list.join(", ")
            )));
        }

        let inq = sqlx::query(
            r#"
            UPDATE inquiry_employees SET
                start_time    = COALESCE($4, start_time),
                end_time      = COALESCE($5, end_time),
                planned_hours = COALESCE($6::numeric, planned_hours),
                clock_in      = COALESCE($7, clock_in),
                clock_out     = COALESCE($8, clock_out),
                break_minutes = COALESCE($9, break_minutes),
                actual_hours  = CASE
                    WHEN COALESCE($7, clock_in) IS NOT NULL
                         AND COALESCE($8, clock_out) IS NOT NULL
                    THEN ROUND((
                        aust_shift_hours(COALESCE($7, clock_in), COALESCE($8, clock_out))
                        - COALESCE($9, break_minutes, 0) / 60.0
                    )::numeric, 2)::float8
                    ELSE actual_hours
                END,
                updated_at    = NOW()
            WHERE inquiry_id = $1 AND employee_id = $2
              AND ($3::date IS NULL OR job_date = $3)
            "#,
        )
        .bind(parent_id)
        .bind(employee_id)
        .bind(patch.job_date)
        .bind(patch.start_time)
        .bind(patch.end_time)
        .bind(patch.planned_hours)
        .bind(patch.clock_in)
        .bind(patch.clock_out)
        .bind(patch.break_minutes)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let cal = sqlx::query(
            r#"
            UPDATE calendar_item_employees SET
                start_time    = COALESCE($4, start_time),
                end_time      = COALESCE($5, end_time),
                planned_hours = COALESCE($6::numeric, planned_hours),
                clock_in      = COALESCE($7, clock_in),
                clock_out     = COALESCE($8, clock_out),
                break_minutes = COALESCE($9, break_minutes),
                actual_hours  = CASE
                    WHEN COALESCE($7, clock_in) IS NOT NULL
                         AND COALESCE($8, clock_out) IS NOT NULL
                    THEN ROUND((
                        aust_shift_hours(COALESCE($7, clock_in), COALESCE($8, clock_out))
                        - COALESCE($9, break_minutes, 0) / 60.0
                    )::numeric, 2)::float8
                    ELSE actual_hours
                END
            WHERE calendar_item_id = $1 AND employee_id = $2
              AND ($3::date IS NULL OR job_date = $3)
            "#,
        )
        .bind(parent_id)
        .bind(employee_id)
        .bind(patch.job_date)
        .bind(patch.start_time)
        .bind(patch.end_time)
        .bind(patch.planned_hours)
        .bind(patch.clock_in)
        .bind(patch.clock_out)
        .bind(patch.break_minutes)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        if inq.rows_affected() == 0 && cal.rows_affected() == 0 {
            return Err(ServiceError::NotFound(format!(
                "Mitarbeiter {employee_id} ist {parent_id} nicht zugewiesen"
            )));
        }

        self.get_assigned_crew(parent_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::calendar_repo;

    async fn try_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    /// Regression (2026-09-10): moving a scheduled job through this path left the
    /// crew standing on the old day. The workers assigned to a Grünebaum move that
    /// slid from the 15th to the 14th reported the job had vanished from their app
    /// entirely — it had not, it was still sitting on the 15th while the job itself
    /// was on the 14th. `update_fields` shifted the crew; this path never did.
    #[tokio::test]
    async fn rescheduling_carries_the_crew_to_the_new_day() {
        let Some(pool) = try_pool().await else { return };

        let customer_id = Uuid::now_v7();
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $2, $3)")
            .bind(customer_id)
            .bind("Grünebaum")
            .bind(format!("{customer_id}@test.de"))
            .execute(&pool)
            .await
            .expect("insert customer");

        let inquiry_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status, source) VALUES ($1, $2, 'accepted', 'test')",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .expect("insert inquiry");

        let employee_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO employees (id, first_name, last_name, email)
             VALUES ($1, 'Dmytro', 'Koren', $2)",
        )
        .bind(employee_id)
        .bind(format!("{employee_id}@test.de"))
        .execute(&pool)
        .await
        .expect("insert employee");

        let svc = CalendarServiceImpl::new(pool.clone());
        let first = NaiveDate::from_ymd_opt(2031, 9, 15).expect("valid date");
        let moved = NaiveDate::from_ymd_opt(2031, 9, 14).expect("valid date");

        svc.schedule_inquiry(inquiry_id, first, vec![employee_id], None, None, None)
            .await
            .expect("first scheduling");

        // The move slips a day. No crew list is passed, exactly as it happens when
        // only the date is being corrected.
        svc.schedule_inquiry(inquiry_id, moved, vec![], None, None, None)
            .await
            .expect("reschedule");

        let days: Vec<NaiveDate> = sqlx::query_scalar(
            "SELECT job_date FROM inquiry_employees WHERE inquiry_id = $1 AND employee_id = $2",
        )
        .bind(inquiry_id)
        .bind(employee_id)
        .fetch_all(&pool)
        .await
        .expect("read crew");

        assert_eq!(
            days,
            vec![moved],
            "the crew must travel with the job, not stay on the day it left"
        );

        sqlx::query("DELETE FROM inquiry_employees WHERE inquiry_id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM inquiries WHERE id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM employees WHERE id = $1").bind(employee_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM customers WHERE id = $1").bind(customer_id).execute(&pool).await.ok();
    }

    /// A move Josie schedules must show in the admin calendar exactly once, as the
    /// Auftrag itself, with its crew. `schedule_inquiry` used to also insert an
    /// unlinked "moving" calendar item, so every such job appeared twice and the
    /// copy carried no crew.
    #[tokio::test]
    async fn josie_scheduled_move_shows_once_in_the_admin_calendar() {
        let Some(pool) = try_pool().await else { return };

        let customer_id = Uuid::now_v7();
        let name = format!("Kalender Test {customer_id}");
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $2, $3)")
            .bind(customer_id)
            .bind(&name)
            .bind(format!("{customer_id}@test.de"))
            .execute(&pool)
            .await
            .expect("insert customer");
        let inquiry_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status, source) VALUES ($1, $2, 'accepted', 'test')",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .expect("insert inquiry");
        let employee_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO employees (id, first_name, last_name, email) VALUES ($1, 'Lea', 'Kalender', $2)",
        )
        .bind(employee_id)
        .bind(format!("{employee_id}@test.de"))
        .execute(&pool)
        .await
        .expect("insert employee");

        let day = NaiveDate::from_ymd_opt(2032, 3, 9).expect("valid date");
        let svc = CalendarServiceImpl::new(pool.clone());
        let item = svc
            .schedule_inquiry(inquiry_id, day, vec![employee_id], Some("Klavier im 3. OG"), None, None)
            .await
            .expect("schedule");
        assert_eq!(item.inquiry_id, Some(inquiry_id));
        assert_eq!(item.kind, "auftrag");

        // What the admin calendar (`GET /calendar/schedule`) reads.
        let jobs = calendar_repo::fetch_schedule_inquiries(&pool, day, day).await.expect("schedule inquiries");
        assert_eq!(jobs.iter().filter(|j| j.inquiry_id == inquiry_id).count(), 1, "the Auftrag shows once");
        let items = calendar_repo::fetch_schedule_calendar_items(&pool, day, day).await.expect("schedule items");
        assert!(
            !items.iter().any(|i| i.title == name),
            "no duplicate free-standing calendar item for the same move"
        );

        let crew: Vec<Uuid> = sqlx::query_scalar(
            "SELECT employee_id FROM inquiry_employees WHERE inquiry_id = $1 AND job_date = $2",
        )
        .bind(inquiry_id)
        .bind(day)
        .fetch_all(&pool)
        .await
        .expect("read crew");
        assert_eq!(crew, vec![employee_id]);

        let (status, notes): (String, Option<String>) =
            sqlx::query_as("SELECT status::text, notes FROM inquiries WHERE id = $1")
                .bind(inquiry_id)
                .fetch_one(&pool)
                .await
                .expect("read inquiry");
        assert_eq!(status, "scheduled");
        assert!(notes.unwrap_or_default().contains("Klavier im 3. OG"));

        sqlx::query("DELETE FROM domain_events WHERE aggregate = $1").bind(format!("inquiry:{inquiry_id}")).execute(&pool).await.ok();
        sqlx::query("DELETE FROM inquiry_employees WHERE inquiry_id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM inquiries WHERE id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM employees WHERE id = $1").bind(employee_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM customers WHERE id = $1").bind(customer_id).execute(&pool).await.ok();
    }

    /// Josie moving a two-day Termin must move it like a drag in the admin
    /// calendar: the end date follows the start and the crew lands on the new
    /// days. The bridge used to write `scheduled_date` alone — the Termin ended
    /// before it began (and dropped out of the calendar) and the crew stayed behind.
    #[tokio::test]
    async fn josie_moving_a_termin_carries_span_and_crew() {
        let Some(pool) = try_pool().await else { return };

        let start = NaiveDate::from_ymd_opt(2032, 5, 10).expect("valid date");
        let end = NaiveDate::from_ymd_opt(2032, 5, 11).expect("valid date");
        let item_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO calendar_items (id, title, category, scheduled_date, end_date, start_time)
             VALUES ($1, 'Lagerräumung Test', 'other', $2, $3, '08:00')",
        )
        .bind(item_id)
        .bind(start)
        .bind(end)
        .execute(&pool)
        .await
        .expect("insert calendar item");
        let employee_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO employees (id, first_name, last_name, email) VALUES ($1, 'Tom', 'Termin', $2)",
        )
        .bind(employee_id)
        .bind(format!("{employee_id}@test.de"))
        .execute(&pool)
        .await
        .expect("insert employee");
        for day in [start, end] {
            sqlx::query(
                "INSERT INTO calendar_item_employees (calendar_item_id, employee_id, job_date) VALUES ($1, $2, $3)",
            )
            .bind(item_id)
            .bind(employee_id)
            .bind(day)
            .execute(&pool)
            .await
            .expect("insert crew");
        }

        let moved = NaiveDate::from_ymd_opt(2032, 5, 17).expect("valid date");
        let svc = CalendarServiceImpl::new(pool.clone());
        let item = svc.reassign_termin(item_id, Some(moved), None).await.expect("move");
        assert_eq!(item.scheduled_date, Some(moved));
        assert_eq!(item.end_date, NaiveDate::from_ymd_opt(2032, 5, 18), "the span moves with the start");

        let days: Vec<NaiveDate> = sqlx::query_scalar(
            "SELECT job_date FROM calendar_item_employees WHERE calendar_item_id = $1 ORDER BY job_date",
        )
        .bind(item_id)
        .fetch_all(&pool)
        .await
        .expect("read crew");
        assert_eq!(days, vec![moved, NaiveDate::from_ymd_opt(2032, 5, 18).expect("valid date")]);

        svc.cancel_termin(item_id, "Test").await.expect("cancel");
        let status: String = sqlx::query_scalar("SELECT status FROM calendar_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .expect("read status");
        assert_eq!(status, "cancelled");

        sqlx::query("DELETE FROM calendar_item_employees WHERE calendar_item_id = $1").bind(item_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM calendar_items WHERE id = $1").bind(item_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM employees WHERE id = $1").bind(employee_id).execute(&pool).await.ok();
    }

    /// Regression (2026-07-13): a Besichtigung created via create_inquiry_appointment
    /// was invisible to the assistant's get_calendar, because get_range only read
    /// calendar_items and inquiries. Josie saw an empty day, concluded the write had
    /// failed, and created a duplicate standalone calendar_item on top.
    #[tokio::test]
    async fn get_range_includes_inquiry_appointments() {
        let Some(pool) = try_pool().await else { return };

        let customer_id = Uuid::now_v7();
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $2, $3)")
            .bind(customer_id)
            .bind("Schilling")
            .bind(format!("{customer_id}@test.de"))
            .execute(&pool)
            .await
            .expect("insert customer");

        let inquiry_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status, source) VALUES ($1, $2, 'pending', 'test')",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .expect("insert inquiry");

        let day = NaiveDate::from_ymd_opt(2031, 7, 16).expect("valid date");
        let appt_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiry_appointments
                (id, inquiry_id, kind, scheduled_date, start_time, end_time, location, status)
             VALUES ($1, $2, 'besichtigung', $3, '14:00', '15:00', 'An der Renne 71', 'scheduled')",
        )
        .bind(appt_id)
        .bind(inquiry_id)
        .bind(day)
        .execute(&pool)
        .await
        .expect("insert appointment");

        let svc = CalendarServiceImpl::new(pool.clone());
        let items = svc.get_range(day, day).await.expect("get_range");

        let appt = items
            .iter()
            .find(|i| i.id == appt_id)
            .expect("Besichtigung must appear in the assistant's calendar");
        assert_eq!(appt.kind, "besichtigung");
        assert_eq!(appt.inquiry_id, Some(inquiry_id), "links back to its inquiry");
        assert_eq!(appt.start_time, NaiveTime::from_hms_opt(14, 0, 0));
        assert!(appt.title.contains("Schilling"), "title carries the customer");

        sqlx::query("DELETE FROM inquiry_appointments WHERE id = $1").bind(appt_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM inquiries WHERE id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM customers WHERE id = $1").bind(customer_id).execute(&pool).await.ok();
    }

    /// A cancelled appointment must not show up on the calendar.
    #[tokio::test]
    async fn get_range_excludes_cancelled_appointments() {
        let Some(pool) = try_pool().await else { return };

        let customer_id = Uuid::now_v7();
        sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $2, $3)")
            .bind(customer_id)
            .bind("Abgesagt")
            .bind(format!("{customer_id}@test.de"))
            .execute(&pool)
            .await
            .expect("insert customer");

        let inquiry_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status, source) VALUES ($1, $2, 'pending', 'test')",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .expect("insert inquiry");

        let day = NaiveDate::from_ymd_opt(2031, 7, 17).expect("valid date");
        let appt_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiry_appointments
                (id, inquiry_id, kind, scheduled_date, status)
             VALUES ($1, $2, 'besichtigung', $3, 'cancelled')",
        )
        .bind(appt_id)
        .bind(inquiry_id)
        .bind(day)
        .execute(&pool)
        .await
        .expect("insert appointment");

        let svc = CalendarServiceImpl::new(pool.clone());
        let items = svc.get_range(day, day).await.expect("get_range");
        assert!(
            !items.iter().any(|i| i.id == appt_id),
            "cancelled Besichtigung must not appear"
        );

        sqlx::query("DELETE FROM inquiry_appointments WHERE id = $1").bind(appt_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM inquiries WHERE id = $1").bind(inquiry_id).execute(&pool).await.ok();
        sqlx::query("DELETE FROM customers WHERE id = $1").bind(customer_id).execute(&pool).await.ok();
    }
}
