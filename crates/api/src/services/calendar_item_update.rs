//! The one write path for editing a calendar item (Termin).
//!
//! `PATCH /admin/calendar-items/{id}` and Josie's Termin tools both land here, so
//! moving a Termin from Telegram keeps a multi-day span intact and carries the
//! crew to the new day exactly as a drag in the admin calendar does. Josie's
//! bridge used to write `scheduled_date` alone: the end date stayed behind (the
//! item could end before it started and vanish from the calendar) and the crew
//! stayed on the old day.

use chrono::{NaiveDate, NaiveTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repositories::calendar_item_repo;
use crate::ApiError;

/// Body for partially updating an existing calendar item.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateItemBody {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub location: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub duration_hours: Option<f64>,
    pub status: Option<String>,
    pub customer_id: Option<Uuid>,
    /// Set to true to explicitly remove the customer assignment.
    #[serde(default)]
    pub remove_customer: bool,
    pub employee_notes: Option<String>,
    /// Set end_date for multi-day items; send null to clear (single-day).
    pub end_date: Option<serde_json::Value>,
    /// Enable travel daily allowance (Verpflegungspauschale) for multi-day trips.
    pub has_pauschale: Option<bool>,
}

/// Apply a partial update to a calendar item and return the refreshed row.
///
/// Only fields set in `body` change. Moving `scheduled_date` shifts `end_date`
/// (unless given) and every crew row without recorded work by the same delta.
pub(crate) async fn update_item(
    db: &PgPool,
    id: Uuid,
    body: UpdateItemBody,
) -> Result<calendar_item_repo::CalendarItemRow, ApiError> {
    // Verify the item exists before building the dynamic update
    if !calendar_item_repo::item_exists(db, id).await? {
        return Err(ApiError::NotFound("Kalendereintrag nicht gefunden".into()));
    }

    // Build dynamic SET clause — kept inline because SQL is dynamically constructed
    let mut sets: Vec<String> = Vec::new();
    let mut idx = 1usize;

    if body.title.is_some() {
        sets.push(format!("title = ${idx}"));
        idx += 1;
    }
    if body.description.is_some() {
        sets.push(format!("description = ${idx}"));
        idx += 1;
    }
    if body.category.is_some() {
        sets.push(format!("category = ${idx}"));
        idx += 1;
    }
    if body.location.is_some() {
        sets.push(format!("location = ${idx}"));
        idx += 1;
    }
    if body.scheduled_date.is_some() {
        sets.push(format!("scheduled_date = ${idx}"));
        // Preserve the multi-day span: shift end_date by the same delta unless the caller
        // is overriding end_date explicitly below. Without this, dragging forward leaves
        // end_date < scheduled_date and the generate_series in fetch_schedule_calendar_items
        // returns empty, making the item vanish from the calendar.
        if body.end_date.is_none() {
            sets.push(format!("end_date = end_date + (${idx}::date - scheduled_date)"));
        }
        idx += 1;
    }
    if body.start_time.is_some() {
        sets.push(format!("start_time = ${idx}"));
        idx += 1;
    }
    if body.end_time.is_some() {
        sets.push(format!("end_time = ${idx}"));
        idx += 1;
    }
    if body.duration_hours.is_some() {
        sets.push(format!("duration_hours = ${idx}"));
        idx += 1;
    }
    if body.status.is_some() {
        sets.push(format!("status = ${idx}"));
        idx += 1;
    }
    if body.remove_customer {
        sets.push("customer_id = NULL".to_string());
    } else if body.customer_id.is_some() {
        sets.push(format!("customer_id = ${idx}"));
        idx += 1;
    }
    if body.employee_notes.is_some() {
        sets.push(format!("employee_notes = ${idx}"));
        idx += 1;
    }
    let update_end_date = body.end_date.is_some();
    if update_end_date {
        sets.push(format!("end_date = ${idx}"));
        idx += 1;
    }
    if body.has_pauschale.is_some() {
        sets.push(format!("has_pauschale = ${idx}"));
        idx += 1;
    }

    // Always update updated_at
    sets.push(format!("updated_at = ${idx}"));

    if sets.len() == 1 {
        // Only updated_at would be set — nothing actually changed
        return Ok(calendar_item_repo::fetch_item_row(db, id).await?);
    }

    let sql = format!(
        "UPDATE calendar_items SET {} WHERE id = ${}",
        sets.join(", "),
        idx + 1,
    );

    // Bind values in the same order as the SET clause was built
    let mut q = sqlx::query(&sql);
    if let Some(v) = body.title {
        q = q.bind(v);
    }
    if let Some(v) = body.description {
        q = q.bind(v);
    }
    if let Some(v) = body.category {
        q = q.bind(v);
    }
    if let Some(v) = body.location {
        q = q.bind(v);
    }
    if let Some(v) = body.scheduled_date {
        q = q.bind(v);
    }
    if let Some(v) = body.start_time {
        q = q.bind(v);
    }
    if let Some(v) = body.end_time {
        q = q.bind(v);
    }
    if let Some(v) = body.duration_hours {
        q = q.bind(v);
    }
    if let Some(v) = body.status {
        q = q.bind(v);
    }
    if !body.remove_customer
        && let Some(v) = body.customer_id {
            q = q.bind(v);
        }
    if let Some(v) = body.employee_notes {
        q = q.bind(v);
    }
    if update_end_date {
        // end_date is stored as Option<serde_json::Value>; null → None, date string → Some(NaiveDate)
        let end_date_val: Option<NaiveDate> = body.end_date
            .as_ref()
            .and_then(|v| v.as_str())
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        q = q.bind(end_date_val);
    }
    if let Some(v) = body.has_pauschale {
        q = q.bind(v);
    }
    q = q.bind(Utc::now()); // updated_at
    q = q.bind(id);         // WHERE id

    let mut tx = db.begin().await?;

    // Capture the pre-update start date so a reschedule can translate the crew
    // by the same delta the Termin moves (see the assignment shift below).
    let old_scheduled_date: Option<NaiveDate> = if body.scheduled_date.is_some() {
        let row: Option<Option<NaiveDate>> =
            sqlx::query_scalar("SELECT scheduled_date FROM calendar_items WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        row.flatten()
    } else {
        None
    };

    q.execute(&mut *tx).await?;

    // Reschedule: the crew must travel with the Termin when its date moves
    // (feedback report 70bccd4f / 2026-06-11 review finding #4). Shift every
    // assignment row with no recorded work by the move delta so the crew lands
    // on the corresponding new day, instead of being stranded on the old day
    // (the earlier fix merely deleted the stranded row, dropping the crew).
    //
    // Stash → delete → re-insert in three separate statements, not one CTE: a
    // CTE's sub-statements share one snapshot, so a small overlapping shift
    // (2-day Termin moved by 1 day) would trip the
    // (calendar_item_id, employee_id, job_date) unique index. A shifted row
    // outside the new window is dropped; a collision with recorded work is
    // skipped. Recorded work never moves and is never deleted. See the mirror
    // in `inquiry_repo::update_fields`.
    match (body.scheduled_date, old_scheduled_date) {
        (Some(new_sd), Some(old_sd)) if new_sd != old_sd => {
            sqlx::query(
                r#"
                CREATE TEMP TABLE _moved_cal_crew ON COMMIT DROP AS
                SELECT * FROM calendar_item_employees
                WHERE calendar_item_id = $1
                  AND clock_in IS NULL AND clock_out IS NULL AND actual_hours IS NULL
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;

            // Shift the stashed copies, then put them back verbatim. SELECT * on both
            // ends so a column added later cannot be dropped by a move — the explicit
            // nine-column list this replaced discarded the worker's own punch, their
            // break, and every travel-expense field.
            sqlx::query("UPDATE _moved_cal_crew SET job_date = job_date + ($1::date - $2::date)")
                .bind(new_sd)
                .bind(old_sd)
                .execute(&mut *tx)
                .await?;

            sqlx::query(
                r#"
                DELETE FROM calendar_item_employees
                WHERE calendar_item_id = $1
                  AND clock_in IS NULL AND clock_out IS NULL AND actual_hours IS NULL
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO calendar_item_employees
                SELECT m.* FROM _moved_cal_crew m
                JOIN calendar_items c ON c.id = m.calendar_item_id
                WHERE m.job_date
                      BETWEEN c.scheduled_date AND COALESCE(c.end_date, c.scheduled_date)
                ON CONFLICT (calendar_item_id, employee_id, job_date) DO NOTHING
                "#,
            )
            .execute(&mut *tx)
            .await?;
        }
        // Only the end date changed, or the start was set for the first time:
        // keep in-window crew, drop no-work rows now outside the window.
        _ if body.scheduled_date.is_some() || update_end_date => {
            sqlx::query(
                r#"
                DELETE FROM calendar_item_employees cie
                USING calendar_items c
                WHERE cie.calendar_item_id = c.id AND c.id = $1
                  AND cie.clock_in IS NULL AND cie.clock_out IS NULL AND cie.actual_hours IS NULL
                  AND (cie.job_date < c.scheduled_date
                       OR cie.job_date > COALESCE(c.end_date, c.scheduled_date))
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        _ => {}
    }

    tx.commit().await?;
    Ok(calendar_item_repo::fetch_item_row(db, id).await?)
}
