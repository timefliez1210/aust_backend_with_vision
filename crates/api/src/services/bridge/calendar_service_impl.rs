//! Bridge impl for `CalendarService`.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{
    AvailableSlot, CalendarItem, CalendarItemPatch, CalendarService, CrewMember,
    EmployeeWorkloadEntry, ServiceError,
};

use crate::repositories::calendar_item_repo;

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
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
                }
            },
        ));
        items.sort_by(|a, b| a.scheduled_date.cmp(&b.scheduled_date));
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
    ) -> Result<CalendarItem, ServiceError> {
        let default_time = NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default();

        let new_id = calendar_item_repo::insert_item(
            &self.pool,
            title,
            notes,
            category,
            None,
            Some(scheduled_date),
            default_time,
            None,
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
            start_time: Some(default_time),
            end_time: None,
            location: None,
            kind: "termin".to_string(),
        })
    }

    async fn update_item(
        &self,
        id: Uuid,
        patch: CalendarItemPatch,
    ) -> Result<CalendarItem, ServiceError> {
        // Build a partial update.
        sqlx::query(
            r#"
            UPDATE calendar_items SET
                title = COALESCE($2, title),
                category = COALESCE($3, category),
                scheduled_date = COALESCE($4, scheduled_date),
                end_date = COALESCE($5, end_date),
                description = COALESCE($6, description),
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(patch.title.as_deref())
        .bind(patch.category.as_deref())
        .bind(patch.scheduled_date)
        .bind(patch.end_date)
        .bind(patch.notes.as_deref())
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

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
        })
    }

    async fn delete_item(&self, id: Uuid) -> Result<(), ServiceError> {
        calendar_item_repo::delete_item(&self.pool, id)
            .await
            .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn schedule_inquiry(
        &self,
        inquiry_id: Uuid,
        date: NaiveDate,
        crew: Vec<Uuid>,
        notes: Option<&str>,
    ) -> Result<CalendarItem, ServiceError> {
        // Fetch inquiry title for the calendar item.
        let title_row: Option<(String,)> = sqlx::query_as(
            r#"
            SELECT COALESCE(c.name, 'Umzug') AS title
            FROM inquiries i
            LEFT JOIN customers c ON i.customer_id = c.id
            WHERE i.id = $1
            "#,
        )
        .bind(inquiry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let title = title_row.map(|(t,)| t).unwrap_or_else(|| "Umzug".to_string());

        // Update inquiry scheduled_date and status.
        sqlx::query(
            "UPDATE inquiries SET scheduled_date = $1, status = 'scheduled', updated_at = NOW() WHERE id = $2",
        )
        .bind(date)
        .bind(inquiry_id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // Assign employees (use existing inquiry_employees table).
        for employee_id in &crew {
            let _ = sqlx::query(
                r#"
                INSERT INTO inquiry_employees (inquiry_id, employee_id, job_date)
                VALUES ($1, $2, $3)
                ON CONFLICT (inquiry_id, employee_id, job_date) DO NOTHING
                "#,
            )
            .bind(inquiry_id)
            .bind(employee_id)
            .bind(date)
            .execute(&self.pool)
            .await;
        }

        // Create a calendar item referencing the inquiry.
        let default_time = NaiveTime::from_hms_opt(8, 0, 0).unwrap_or_default();
        let new_id = calendar_item_repo::insert_item(
            &self.pool,
            &title,
            notes,
            "moving",
            None,
            Some(date),
            default_time,
            None,
            0.0,
            None,
            None,
        )
        .await
        .map_err(super::map_sqlx)?;

        Ok(CalendarItem {
            id: new_id,
            title,
            category: "moving".to_string(),
            scheduled_date: Some(date),
            end_date: None,
            start_time: Some(default_time),
            end_time: None,
            location: None,
            kind: "termin".to_string(),
        })
    }

    async fn reassign_termin(
        &self,
        termin_id: Uuid,
        new_date: Option<NaiveDate>,
        new_crew: Option<Vec<Uuid>>,
    ) -> Result<CalendarItem, ServiceError> {
        if let Some(date) = new_date {
            sqlx::query(
                "UPDATE calendar_items SET scheduled_date = $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(date)
            .bind(termin_id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        }

        if let Some(crew) = new_crew {
            // Replace calendar item employees.
            sqlx::query("DELETE FROM calendar_item_employees WHERE calendar_item_id = $1")
                .bind(termin_id)
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

            if let Some((Some(date),)) = date_row {
                for employee_id in &crew {
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO calendar_item_employees (calendar_item_id, employee_id, job_date)
                        VALUES ($1, $2, $3)
                        ON CONFLICT DO NOTHING
                        "#,
                    )
                    .bind(termin_id)
                    .bind(employee_id)
                    .bind(date)
                    .execute(&self.pool)
                    .await;
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
        })
    }

    async fn cancel_termin(&self, id: Uuid, _reason: &str) -> Result<(), ServiceError> {
        sqlx::query(
            "UPDATE calendar_items SET status = 'cancelled', updated_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        Ok(())
    }

    async fn assign_employee(
        &self,
        calendar_item_id: Uuid,
        employee_id: Uuid,
    ) -> Result<(), ServiceError> {
        let date_row: Option<(Option<NaiveDate>,)> = sqlx::query_as(
            "SELECT scheduled_date FROM calendar_items WHERE id = $1",
        )
        .bind(calendar_item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let date = date_row
            .ok_or_else(|| ServiceError::NotFound(format!("Kalendereintrag {calendar_item_id}")))?
            .0
            .unwrap_or_else(|| chrono::Local::now().date_naive());

        sqlx::query(
            r#"
            INSERT INTO calendar_item_employees (calendar_item_id, employee_id, job_date)
            VALUES ($1, $2, $3)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(calendar_item_id)
        .bind(employee_id)
        .bind(date)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(())
    }

    async fn get_employee_assignments(
        &self,
        employee_id: Uuid,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError> {
        // Query both inquiry assignments and calendar item assignments.
        let rows: Vec<(NaiveDate, Option<Uuid>, Option<Uuid>, String, String)> =
            sqlx::query_as(
                r#"
                SELECT ie.job_date, ie.inquiry_id, NULL::uuid AS calendar_item_id,
                       COALESCE(c.name, 'Umzug') AS title, 'moving' AS category
                FROM inquiry_employees ie
                JOIN inquiries i ON ie.inquiry_id = i.id
                LEFT JOIN customers c ON i.customer_id = c.id
                WHERE ie.employee_id = $1 AND ie.job_date BETWEEN $2 AND $3
                UNION ALL
                SELECT cie.job_date, NULL::uuid, cie.calendar_item_id,
                       ci.title, ci.category
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
            .map(|(date, inquiry_id, calendar_item_id, title, category)| {
                EmployeeWorkloadEntry {
                    date,
                    inquiry_id,
                    calendar_item_id,
                    title,
                    category,
                }
            })
            .collect())
    }

    async fn get_assigned_crew(&self, id: Uuid) -> Result<Vec<CrewMember>, ServiceError> {
        // The id may be a calendar_item OR an inquiry — crew lives in two
        // separate junction tables. Check both so the caller (the assistant)
        // doesn't have to know which kind of id it holds. This is the read path
        // that prevents the agent from confabulating a crew list.
        let rows: Vec<(Uuid, String, String, NaiveDate, String)> = sqlx::query_as(
            r#"
            SELECT e.id   AS employee_id,
                   e.first_name AS first_name,
                   e.last_name  AS last_name,
                   cie.job_date AS job_date,
                   'termin'::text AS source
            FROM calendar_item_employees cie
            JOIN employees e ON e.id = cie.employee_id
            WHERE cie.calendar_item_id = $1
            UNION ALL
            SELECT e.id, e.first_name, e.last_name, ie.job_date, 'auftrag'::text
            FROM inquiry_employees ie
            JOIN employees e ON e.id = ie.employee_id
            WHERE ie.inquiry_id = $1
            ORDER BY last_name, first_name
            "#,
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(employee_id, first_name, last_name, job_date, source)| CrewMember {
                employee_id,
                first_name,
                last_name,
                job_date,
                source,
            })
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
        let inq: Option<(Option<NaiveDate>,)> =
            sqlx::query_as("SELECT scheduled_date FROM inquiries WHERE id = $1")
                .bind(inquiry_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(super::map_sqlx)?;
        let scheduled = inq
            .ok_or_else(|| ServiceError::NotFound(format!("Anfrage {inquiry_id}")))?
            .0;
        let job_date = date.or(scheduled).ok_or_else(|| {
            ServiceError::Validation(
                "Anfrage hat kein geplantes Datum — bitte Datum angeben.".to_string(),
            )
        })?;

        // Replace the crew wholesale: clear existing rows, insert the new set.
        // No status change, no calendar_item created — purely the inquiry crew.
        let mut tx = self.pool.begin().await.map_err(super::map_sqlx)?;
        sqlx::query("DELETE FROM inquiry_employees WHERE inquiry_id = $1")
            .bind(inquiry_id)
            .execute(&mut *tx)
            .await
            .map_err(super::map_sqlx)?;
        for employee_id in &crew {
            sqlx::query(
                r#"
                INSERT INTO inquiry_employees (inquiry_id, employee_id, job_date)
                VALUES ($1, $2, $3)
                ON CONFLICT (inquiry_id, employee_id, job_date) DO NOTHING
                "#,
            )
            .bind(inquiry_id)
            .bind(employee_id)
            .bind(job_date)
            .execute(&mut *tx)
            .await
            .map_err(super::map_sqlx)?;
        }
        tx.commit().await.map_err(super::map_sqlx)?;

        // Return the freshly written crew so the caller can confirm the result.
        self.get_assigned_crew(inquiry_id).await
    }
}
