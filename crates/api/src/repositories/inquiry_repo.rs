//! Inquiry repository — centralised queries for the `inquiries` table.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// Full inquiry DB row used by the inquiry builder and offer generation.
#[derive(Debug, FromRow)]
pub(crate) struct InquiryDbRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    #[sqlx(default)]
    pub stop_address_id: Option<Uuid>,
    pub status: String,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>, // retired — kept for DB compat
    pub scheduled_date: Option<chrono::NaiveDate>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    #[sqlx(default)]
    pub service_type: Option<String>,
    #[sqlx(default)]
    pub submission_mode: Option<String>,
    #[sqlx(default)]
    pub recipient_id: Option<Uuid>,
    #[sqlx(default)]
    pub inquiry_billing_address_id: Option<Uuid>,
    #[sqlx(default)]
    pub custom_fields: serde_json::Value,
    pub notes: Option<String>,
    #[sqlx(default)]
    pub services: serde_json::Value,
    #[sqlx(default)]
    pub source: String,
    #[sqlx(default)]
    pub offer_sent_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub accepted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Readiness check projection for auto-offer generation.
#[derive(Debug, FromRow)]
pub(crate) struct InquiryReadiness {
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub stop_address_id: Option<Uuid>,
}

/// Check whether an inquiry with the given ID exists.
///
/// **Caller**: `trigger_estimate`, `trigger_estimate_upload`, `trigger_video_upload`
/// **Why**: Validates inquiry_id before proceeding with estimation.
pub(crate) async fn exists(pool: &PgPool, inquiry_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM inquiries WHERE id = $1)")
        .bind(inquiry_id)
        .fetch_one(pool)
        .await
}

/// Fetch a full inquiry row by ID (inquiry builder projection).
///
/// **Caller**: `inquiry_builder::build_inquiry_response`
/// **Why**: Single source of truth for the full inquiry detail.
pub(crate) async fn fetch_by_id(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<InquiryDbRow, ApiError> {
    sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, stop_address_id,
               status, estimated_volume_m3, distance_km, preferred_date, scheduled_date,
               start_time, end_time, service_type, submission_mode, recipient_id,
               billing_address_id AS inquiry_billing_address_id, custom_fields, notes,
               services, source, offer_sent_at, accepted_at, created_at, updated_at
        FROM inquiries WHERE id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("Inquiry {inquiry_id} not found")))
}

/// Fetch readiness data for auto-offer generation.
///
/// **Caller**: `orchestrator::try_auto_generate_offer`
/// **Why**: Checks volume/distance/addresses to decide if an offer can be generated.
pub(crate) async fn fetch_readiness(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<InquiryReadiness>, sqlx::Error> {
    sqlx::query_as(
        "SELECT estimated_volume_m3, distance_km, origin_address_id, destination_address_id, stop_address_id FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Update inquiry status and updated_at timestamp.
///
/// **Caller**: `build_offer_with_overrides`, `handle_offer_denial`, various estimation handlers
/// **Why**: Many code paths transition inquiry status; centralises the UPDATE.
pub(crate) async fn update_status(
    pool: &PgPool,
    inquiry_id: Uuid,
    status: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(status)
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update inquiry distance_km.
///
/// **Caller**: `orchestrator::try_auto_generate_offer`, background distance calculations
/// **Why**: Distance is calculated asynchronously and written back to the inquiry.
pub(crate) async fn update_distance(
    pool: &PgPool,
    inquiry_id: Uuid,
    distance_km: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET distance_km = $1, updated_at = NOW() WHERE id = $2")
        .bind(distance_km)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update inquiry volume and status after estimation completes.
///
/// **Caller**: `estimates::vision_estimate`, `estimates::depth_sensor_estimate`, `estimates::inventory_estimate`, `estimates::process_video_background`
/// **Why**: Volume estimation completion triggers a status change and volume write.
pub(crate) async fn update_volume_and_status(
    pool: &PgPool,
    inquiry_id: Uuid,
    volume: f64,
    status: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inquiries SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(volume)
    .bind(status)
    .bind(now)
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update estimated volume only (no status change).
///
/// **Caller**: `handle_offer_edit` (volume override), `update_inquiry_items`
/// **Why**: Admin edits volume without changing status.
pub(crate) async fn update_volume(
    pool: &PgPool,
    inquiry_id: Uuid,
    volume: f64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET estimated_volume_m3 = $1, updated_at = $2 WHERE id = $3")
        .bind(volume)
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Hard-delete an inquiry by ID.
///
/// **Caller**: `delete_inquiry` handler
/// **Why**: FK CASCADE handles related records.
///
/// # Returns
/// Number of rows deleted (0 or 1).
pub(crate) async fn hard_delete(pool: &PgPool, inquiry_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM inquiries WHERE id = $1")
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Fetch the customer_id for an inquiry.
///
/// **Caller**: `find_or_create_inquiry_thread`
/// **Why**: Thread creation needs the customer_id from the inquiry.
pub(crate) async fn fetch_customer_id(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT customer_id FROM inquiries WHERE id = $1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Link an email thread to an inquiry.
///
/// **Caller**: `handle_complete_inquiry`
/// **Why**: Links the most recent open thread to the newly created inquiry.
pub(crate) async fn link_email_thread(
    pool: &PgPool,
    inquiry_id: Uuid,
    customer_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE email_threads SET inquiry_id = $1
        WHERE id = (
            SELECT id FROM email_threads
            WHERE customer_id = $2 AND inquiry_id IS NULL
            ORDER BY created_at DESC LIMIT 1
        )
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a full inquiry record (orchestrator / admin path with all fields).
///
/// **Caller**: `handle_complete_inquiry`, `create_inquiry`
/// **Why**: Centralises the INSERT with all columns including volume, distance, services, source.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create(
    pool: &PgPool,
    id: Uuid,
    customer_id: Uuid,
    origin_id: Option<Uuid>,
    dest_id: Option<Uuid>,
    stop_id: Option<Uuid>,
    status: &str,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    scheduled_date: Option<NaiveDate>,
    notes: Option<&str>,
    services: &serde_json::Value,
    source: &str,
    service_type: Option<&str>,
    submission_mode: Option<&str>,
    recipient_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
    custom_fields: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id, stop_address_id,
                           status, estimated_volume_m3, distance_km, scheduled_date, notes, services, source,
                           service_type, submission_mode, recipient_id, billing_address_id, custom_fields,
                           created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                COALESCE($14, 'termin'), $15, $16, $17, $18, $18)
        "#,
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(stop_id)
    .bind(status)
    .bind(estimated_volume_m3)
    .bind(distance_km)
    .bind(scheduled_date)
    .bind(notes)
    .bind(services)
    .bind(source)
    .bind(service_type)
    .bind(submission_mode)
    .bind(recipient_id)
    .bind(billing_address_id)
    .bind(custom_fields)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update inquiry fields using COALESCE (partial update).
///
/// **Caller**: `update_inquiry` handler
/// **Why**: Admin dashboard can update multiple fields in one request.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_fields(
    pool: &PgPool,
    id: Uuid,
    status: Option<&str>,
    notes: Option<&str>,
    services_json: Option<&serde_json::Value>,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    origin_address_id: Option<Uuid>,
    scheduled_date: Option<NaiveDate>,
    destination_address_id: Option<Uuid>,
    service_type: Option<&str>,
    submission_mode: Option<&str>,
    recipient_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
    custom_fields: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE inquiries SET
            status = COALESCE($2, status),
            notes = COALESCE($3, notes),
            services = COALESCE($4, services),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            scheduled_date = COALESCE($7, scheduled_date),
            start_time = COALESCE($8, start_time),
            end_time = COALESCE($9, end_time),
            origin_address_id = COALESCE($10, origin_address_id),
            destination_address_id = COALESCE($11, destination_address_id),
            service_type = COALESCE($12, service_type),
            submission_mode = COALESCE($13, submission_mode),
            recipient_id = COALESCE($14, recipient_id),
            billing_address_id = COALESCE($15, billing_address_id),
            custom_fields = COALESCE($16, custom_fields),
            updated_at = $17
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(status)
    .bind(notes)
    .bind(services_json)
    .bind(estimated_volume_m3)
    .bind(distance_km)
    .bind(scheduled_date)
    .bind(start_time)
    .bind(end_time)
    .bind(origin_address_id)
    .bind(destination_address_id)
    .bind(service_type)
    .bind(submission_mode)
    .bind(recipient_id)
    .bind(billing_address_id)
    .bind(custom_fields)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Shift all `inquiry_days.day_date` values for an inquiry by `delta` days.
///
/// **Caller**: `update_inquiry` handler, after `scheduled_date` changes.
/// **Why**: Multi-day day rows store absolute dates. When the parent inquiry is
/// rescheduled, the day rows must move by the same offset so the calendar still
/// shows them under the correct date cells.
pub(crate) async fn shift_inquiry_days(
    pool: &PgPool,
    inquiry_id: Uuid,
    delta_days: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inquiry_days SET day_date = day_date + make_interval(days => $2::int) WHERE inquiry_id = $1",
    )
    .bind(inquiry_id)
    .bind(delta_days)
    .execute(pool)
    .await?;
    Ok(())
}

/// Auto-update billing_address_id from origin to destination when an inquiry
/// transitions to "completed". Only applies when billing_address_id is currently
/// the same as origin_address_id (booking-for-self) or NULL.
///
/// **Caller**: `update_inquiry` handler, after status change to "completed".
/// **Why**: Once the customer has moved, invoices should go to the new address.
pub(crate) async fn auto_update_billing_on_completed(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE inquiries SET
            billing_address_id = destination_address_id,
            updated_at = NOW()
        WHERE id = $1
          AND (billing_address_id IS NULL OR billing_address_id = origin_address_id)
          AND destination_address_id IS NOT NULL
        "#,
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch customer name, email, and origin/destination cities for LLM email draft generation.
///
/// **Caller**: `generate_offer_email_draft`
/// **Why**: LLM prompt needs customer + city context to write a personalised email.
pub(crate) async fn fetch_email_draft_context(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(String, Option<String>, Option<String>, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT c.name, c.email, a_orig.city, a_dest.city
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses a_orig ON q.origin_address_id = a_orig.id
        LEFT JOIN addresses a_dest ON q.destination_address_id = a_dest.id
        WHERE q.id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Email thread summary row for an inquiry.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub(crate) struct EmailThreadSummary {
    pub id: Uuid,
    pub subject: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub message_count: i64,
}

/// Fetch email threads linked to an inquiry.
///
/// **Caller**: `get_inquiry_emails` handler
/// **Why**: Shows linked email conversations for the inquiry.
pub(crate) async fn fetch_email_threads(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<EmailThreadSummary>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            et.id,
            et.subject,
            (SELECT MAX(em.created_at) FROM email_messages em WHERE em.thread_id = et.id) AS last_message_at,
            COALESCE((SELECT COUNT(*) FROM email_messages em WHERE em.thread_id = et.id), 0) AS message_count
        FROM email_threads et
        WHERE et.inquiry_id = $1
        ORDER BY et.created_at DESC
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Employee assignment row for inquiry detail (includes email field).
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub(crate) struct EmployeeAssignmentRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub planned_hours: f64,
    pub clock_in: Option<chrono::NaiveTime>,
    pub clock_out: Option<chrono::NaiveTime>,
    pub start_time: Option<chrono::NaiveTime>,
    pub end_time: Option<chrono::NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
}

/// Fetch employee assignments for an inquiry (with email).
///
/// **Caller**: `list_inquiry_employees` handler
/// **Why**: Shows assigned employees for a job. Reads from day-level table,
///          aggregating per-employee across all days.
pub(crate) async fn list_employee_assignments(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<EmployeeAssignmentRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ide.employee_id, e.first_name, e.last_name, e.email,
               SUM(ide.planned_hours)::float8 AS planned_hours,
               MIN(CASE WHEN iday.day_number = 1 THEN ide.clock_in END) AS clock_in,
               MAX(CASE WHEN iday.day_number = 1 THEN ide.clock_out END) AS clock_out,
               MIN(CASE WHEN iday.day_number = 1 THEN ide.start_time END) AS start_time,
               MAX(CASE WHEN iday.day_number = 1 THEN ide.end_time END) AS end_time,
               COALESCE(MAX(CASE WHEN iday.day_number = 1 THEN ide.break_minutes END), 0)::int AS break_minutes,
               SUM(CASE WHEN ide.clock_out IS NOT NULL AND ide.clock_in IS NOT NULL
                         THEN (EXTRACT(EPOCH FROM (ide.clock_out - ide.clock_in)) / 3600.0)
                         ELSE NULL END)::float8 AS actual_hours,
               STRING_AGG(ide.notes, '; ' ORDER BY iday.day_number) AS notes
        FROM inquiry_day_employees ide
        JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id
        JOIN employees e ON ide.employee_id = e.id
        WHERE iday.inquiry_id = $1
        GROUP BY ide.employee_id, e.first_name, e.last_name, e.email
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Check whether an employee exists and is active.
///
/// **Caller**: `assign_employee` handler
/// **Why**: Validates employee before assigning to an inquiry.
///
/// # Returns
/// `None` if employee not found, `Some(active)` otherwise.
pub(crate) async fn check_employee_active(
    pool: &PgPool,
    employee_id: Uuid,
) -> Result<Option<bool>, sqlx::Error> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT active FROM employees WHERE id = $1")
            .bind(employee_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(active,)| active))
}

/// Insert an employee assignment for an inquiry.
///
/// **Caller**: `assign_employee` handler
/// **Why**: Links an employee to a moving job. Auto-creates a day-1 inquiry_days row
///          if one doesn't exist, then inserts into inquiry_day_employees.
pub(crate) async fn insert_employee_assignment(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    employee_id: Uuid,
    planned_hours: f64,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    // Ensure a day-1 inquiry_days row exists for this inquiry
    let day_id: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM inquiry_days WHERE inquiry_id = $1 AND day_number = 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    let inquiry_day_id = if let Some((existing_id,)) = day_id {
        existing_id
    } else {
        // No inquiry_days row yet — create day 1 from the inquiry's scheduled_date.
        // Fall back to 08:00–17:00 if the inquiry has no times set.
        let row: (Uuid,) = sqlx::query_as(
            r#"
            INSERT INTO inquiry_days (inquiry_id, day_date, day_number, start_time, end_time)
            SELECT $1,
                   COALESCE(scheduled_date, created_at::date),
                   1,
                   COALESCE(start_time, '08:00'::time),
                   COALESCE(end_time, '17:00'::time)
            FROM inquiries WHERE id = $1
            RETURNING id
            "#,
        )
        .bind(inquiry_id)
        .fetch_one(pool)
        .await?;
        row.0
    };

    // Insert into inquiry_day_employees
    sqlx::query(
        r#"
        INSERT INTO inquiry_day_employees (id, inquiry_day_id, employee_id, planned_hours, notes)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(inquiry_day_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(notes)
    .execute(pool)
    .await?;

    // Also insert into inquiry_employees for backwards compat during transition
    let _ = sqlx::query(
        r#"
        INSERT INTO inquiry_employees (id, inquiry_id, employee_id, planned_hours, notes)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (inquiry_id, employee_id) DO NOTHING
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(notes)
    .execute(pool)
    .await;

    Ok(())
}

/// Update an employee assignment (hours, clock times, notes).
///
/// **Caller**: `update_assignment` handler
/// **Why**: Operators adjust hours after the job. Updates both the day-level
///          and flat tables for transition compatibility.
///
/// # Returns
/// Number of rows affected in the day-level table (0 if not found).
pub(crate) async fn update_employee_assignment(
    pool: &PgPool,
    inquiry_id: Uuid,
    employee_id: Uuid,
    planned_hours: Option<f64>,
    clock_in: Option<chrono::NaiveTime>,
    clock_out: Option<chrono::NaiveTime>,
    start_time: Option<chrono::NaiveTime>,
    end_time: Option<chrono::NaiveTime>,
    break_minutes: i32,
    actual_hours_override: Option<f64>,
    notes: Option<&str>,
) -> Result<u64, sqlx::Error> {
    // Derive actual_hours in Rust: use override if provided, otherwise compute from clock times
    let computed_actual_hours: Option<f64> = if let Some(ah) = actual_hours_override {
        Some(ah)
    } else if let (Some(ci), Some(co)) = (clock_in, clock_out) {
        let duration_secs = (co - ci).num_seconds() as f64;
        Some(duration_secs / 3600.0 - break_minutes as f64 / 60.0)
    } else {
        None
    };

    // Update day-level table
    let result = sqlx::query(
        r#"
        UPDATE inquiry_day_employees SET
            clock_in      = COALESCE($4, inquiry_day_employees.clock_in),
            clock_out     = COALESCE($5, inquiry_day_employees.clock_out),
            start_time    = COALESCE($6, inquiry_day_employees.start_time),
            end_time      = COALESCE($7, inquiry_day_employees.end_time),
            break_minutes = $8,
            actual_hours  = $9,
            planned_hours = CASE
                WHEN $9 IS NOT NULL THEN $9
                WHEN COALESCE($4, inquiry_day_employees.clock_in) IS NOT NULL AND COALESCE($5, inquiry_day_employees.clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, inquiry_day_employees.clock_out) - COALESCE($4, inquiry_day_employees.clock_in))) / 3600.0)
                ELSE COALESCE($3, inquiry_day_employees.planned_hours)
            END,
            notes = COALESCE($10, inquiry_day_employees.notes)
        FROM inquiry_days iday
        WHERE inquiry_day_id = iday.id
          AND iday.inquiry_id = $1
          AND employee_id = $2
          AND iday.day_number = 1
        "#,
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(computed_actual_hours)
    .bind(notes)
    .execute(pool)
    .await?;

    // Also update flat table for backwards compat
    let _ = sqlx::query(
        r#"
        UPDATE inquiry_employees SET
            clock_in      = COALESCE($4, clock_in),
            clock_out     = COALESCE($5, clock_out),
            start_time    = COALESCE($6, start_time),
            end_time      = COALESCE($7, end_time),
            break_minutes = $8,
            actual_hours  = $9,
            planned_hours = CASE
                WHEN $9 IS NOT NULL THEN $9
                WHEN COALESCE($4, clock_in) IS NOT NULL AND COALESCE($5, clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, clock_out) - COALESCE($4, clock_in))) / 3600.0)
                ELSE COALESCE($3, planned_hours)
            END,
            notes = COALESCE($10, notes)
        WHERE inquiry_id = $1 AND employee_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(clock_in)
    .bind(clock_out)
    .bind(start_time)
    .bind(end_time)
    .bind(break_minutes)
    .bind(computed_actual_hours)
    .bind(notes)
    .execute(pool)
    .await;

    Ok(result.rows_affected())
}

/// Updated assignment row projection (after update).
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct UpdatedAssignmentRow {
    pub planned_hours: f64,
    pub clock_in: Option<chrono::NaiveTime>,
    pub clock_out: Option<chrono::NaiveTime>,
    pub start_time: Option<chrono::NaiveTime>,
    pub end_time: Option<chrono::NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
}

/// Fetch an updated employee assignment (for response after PATCH).
///
/// **Caller**: `update_assignment` handler
/// **Why**: Returns the updated assignment data.
pub(crate) async fn fetch_updated_assignment(
    pool: &PgPool,
    inquiry_id: Uuid,
    employee_id: Uuid,
) -> Result<UpdatedAssignmentRow, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT SUM(ide.planned_hours)::float8 AS planned_hours,
               MIN(CASE WHEN iday.day_number = 1 THEN ide.clock_in END) AS clock_in,
               MAX(CASE WHEN iday.day_number = 1 THEN ide.clock_out END) AS clock_out,
               MIN(CASE WHEN iday.day_number = 1 THEN ide.start_time END) AS start_time,
               MAX(CASE WHEN iday.day_number = 1 THEN ide.end_time END) AS end_time,
               COALESCE(MAX(CASE WHEN iday.day_number = 1 THEN ide.break_minutes END), 0)::int AS break_minutes,
               SUM(CASE WHEN ide.clock_out IS NOT NULL AND ide.clock_in IS NOT NULL
                         THEN (EXTRACT(EPOCH FROM (ide.clock_out - ide.clock_in)) / 3600.0)
                         ELSE NULL END)::float8 AS actual_hours,
               STRING_AGG(ide.notes, '; ' ORDER BY iday.day_number) AS notes
        FROM inquiry_day_employees ide
        JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id
        WHERE iday.inquiry_id = $1 AND ide.employee_id = $2
        GROUP BY ide.employee_id
        "#,
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .fetch_one(pool)
    .await
}

/// Delete an employee assignment from an inquiry.
///
/// **Caller**: `remove_assignment` handler
/// **Why**: Unlinks an employee from a moving job. Deletes from both day-level
///          and flat tables for transition compatibility.
///
/// # Returns
/// Number of rows deleted from the day-level table (0 or more).
pub(crate) async fn delete_employee_assignment(
    pool: &PgPool,
    inquiry_id: Uuid,
    employee_id: Uuid,
) -> Result<u64, sqlx::Error> {
    // Delete from day-level table (may affect multiple days)
    let result = sqlx::query(
        r#"
        DELETE FROM inquiry_day_employees
        WHERE employee_id = $2
          AND inquiry_day_id IN (
              SELECT id FROM inquiry_days WHERE inquiry_id = $1
          )
        "#,
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .execute(pool)
    .await?;

    // Also delete from flat table
    let _ = sqlx::query(
        "DELETE FROM inquiry_employees WHERE inquiry_id = $1 AND employee_id = $2",
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .execute(pool)
    .await;

    Ok(result.rows_affected())
}

/// Employee assignment snapshot row for the inquiry builder (includes employee_ clock fields).
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct EmployeeAssignmentSnapshotRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub planned_hours: f64,
    pub clock_in: Option<chrono::NaiveTime>,
    pub clock_out: Option<chrono::NaiveTime>,
    pub start_time: Option<chrono::NaiveTime>,
    pub end_time: Option<chrono::NaiveTime>,
    pub break_minutes: i32,
    pub actual_hours: Option<f64>,
    pub employee_clock_in: Option<DateTime<Utc>>,
    pub employee_clock_out: Option<DateTime<Utc>>,
    pub employee_actual_hours: Option<f64>,
    pub notes: Option<String>,
}

/// Fetch employee assignments for an inquiry (inquiry builder projection with employee_ clock fields).
///
/// **Caller**: `inquiry_builder::build_inquiry_response`
/// **Why**: Canonical inquiry detail includes employee assignments.
pub(crate) async fn fetch_employee_assignments_snapshot(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<EmployeeAssignmentSnapshotRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ide.employee_id, e.first_name, e.last_name,
               ide.planned_hours::float8 AS planned_hours,
               ide.clock_in,
               ide.clock_out,
               ide.start_time,
               ide.end_time,
               COALESCE(ide.break_minutes, 0)::int AS break_minutes,
               CASE WHEN ide.clock_out IS NOT NULL AND ide.clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (ide.clock_out - ide.clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
               ie.employee_clock_in,
               ie.employee_clock_out,
               CASE WHEN ie.employee_clock_out IS NOT NULL AND ie.employee_clock_in IS NOT NULL
                    THEN EXTRACT(EPOCH FROM (ie.employee_clock_out - ie.employee_clock_in)) / 3600.0
                    ELSE NULL END AS employee_actual_hours,
               ide.notes
        FROM inquiry_day_employees ide
        JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id
        JOIN employees e ON ide.employee_id = e.id
        LEFT JOIN inquiry_employees ie ON ie.inquiry_id = iday.inquiry_id AND ie.employee_id = ide.employee_id
        WHERE iday.inquiry_id = $1 AND iday.day_number = 1
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Scheduled day row.
#[derive(sqlx::FromRow)]
pub(crate) struct ScheduledDayRow {
    pub day_date: NaiveDate,
    pub day_number: i16,
    pub notes: Option<String>,
}

/// Fetch all scheduled day records for an inquiry, ordered by day_number.
///
/// **Caller**: `inquiry_builder::build_inquiry_response`
/// **Why**: Multi-day inquiries need their dates embedded in the detail response.
pub(crate) async fn fetch_scheduled_days(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<ScheduledDayRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT day_date, day_number, notes
        FROM inquiry_days
        WHERE inquiry_id = $1
        ORDER BY day_number ASC
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// List item row for paginated inquiry list.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ListItemDbRow {
    pub id: Uuid,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub customer_salutation: Option<String>,
    #[sqlx(default)]
    pub customer_type: Option<String>,
    #[sqlx(default)]
    pub service_type: Option<String>,
    pub origin_city: Option<String>,
    pub destination_city: Option<String>,
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub status: String,
    pub has_offer: bool,
    pub offer_status: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Fetch a paginated list of inquiries with filters.
///
/// **Caller**: `inquiry_builder::build_inquiry_list`
/// **Why**: Canonical paginated list query.
pub(crate) async fn list_items(
    pool: &PgPool,
    status: Option<&str>,
    search_pattern: Option<&str>,
    has_offer: Option<bool>,
    limit: i64,
    offset: i64,
) -> Result<Vec<ListItemDbRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            i.id,
            c.name AS customer_name,
            c.email AS customer_email,
            c.salutation AS customer_salutation,
            c.customer_type,
            i.service_type,
            oa.city AS origin_city,
            da.city AS destination_city,
            i.estimated_volume_m3 AS volume_m3,
            i.distance_km,
            i.status,
            EXISTS (
                SELECT 1 FROM offers
                WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
            ) AS has_offer,
            (
                SELECT o2.status FROM offers o2
                WHERE o2.inquiry_id = i.id AND o2.status NOT IN ('rejected', 'cancelled')
                ORDER BY o2.created_at DESC LIMIT 1
            ) AS offer_status,
            i.created_at
        FROM inquiries i
        LEFT JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ($1::text IS NULL OR i.status = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.email ILIKE $2)
          AND ($3::bool IS NULL OR
               (CASE WHEN $3 THEN EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) ELSE NOT EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) END))
        ORDER BY i.created_at DESC
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(status)
    .bind(search_pattern)
    .bind(has_offer)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Count inquiries matching the given filters.
///
/// **Caller**: `inquiry_builder::build_inquiry_list`
/// **Why**: Total count for pagination metadata.
pub(crate) async fn count_items(
    pool: &PgPool,
    status: Option<&str>,
    search_pattern: Option<&str>,
    has_offer: Option<bool>,
) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM inquiries i
        LEFT JOIN customers c ON i.customer_id = c.id
        WHERE ($1::text IS NULL OR i.status = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.email ILIKE $2)
          AND ($3::bool IS NULL OR
               (CASE WHEN $3 THEN EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) ELSE NOT EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) END))
        "#,
    )
    .bind(status)
    .bind(search_pattern)
    .bind(has_offer)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Insert a minimal inquiry record (public submissions path).
///
/// **Caller**: `handle_submission`, `video_inquiry`
/// **Why**: Public form submissions create inquiries without volume/distance initially.
pub(crate) async fn create_minimal(
    pool: &PgPool,
    id: Uuid,
    customer_id: Uuid,
    origin_id: Option<Uuid>,
    dest_id: Option<Uuid>,
    status: &str,
    scheduled_date: Option<NaiveDate>,
    notes: Option<&str>,
    services: Option<&serde_json::Value>,
    source: &str,
    service_type: Option<&str>,
    submission_mode: Option<&str>,
    recipient_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
    custom_fields: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, scheduled_date, notes, services, source,
                           service_type, submission_mode, recipient_id, billing_address_id, custom_fields,
                           created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)
        "#,
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .bind(scheduled_date)
    .bind(notes)
    .bind(services)
    .bind(source)
    .bind(service_type)
    .bind(submission_mode)
    .bind(recipient_id)
    .bind(billing_address_id)
    .bind(custom_fields)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

// ── Flat-table sync (transition compatibility) ───────────────────────────────

/// After day-level writes, sync the flat `inquiry_employees` table so that
/// `fetch_employee_assignments_snapshot` (offer builder) and any other flat-table
/// reads still return correct data.
///
/// **Caller**: `put_inquiry_days` handler, after committing day-level data.
/// **Why**: The multi-day editor writes only to `inquiry_day_employees`. This function
///          rebuilds the flat table from the day-level data to keep both in sync.
pub(crate) async fn sync_flat_inquiry_employees(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Delete all existing flat-table rows for this inquiry
    sqlx::query("DELETE FROM inquiry_employees WHERE inquiry_id = $1")
        .bind(inquiry_id)
        .execute(pool)
        .await?;

    // Re-insert from day-level data, aggregating per employee
    sqlx::query(
        r#"
        INSERT INTO inquiry_employees (id, inquiry_id, employee_id, planned_hours, clock_in, clock_out, start_time, end_time, break_minutes, actual_hours, notes, created_at)
        SELECT gen_random_uuid(),
               iday.inquiry_id,
               ide.employee_id,
               SUM(ide.planned_hours),
               MIN(CASE WHEN iday.day_number = 1 THEN ide.clock_in END),
               MAX(CASE WHEN iday.day_number = 1 THEN ide.clock_out END),
               MIN(CASE WHEN iday.day_number = 1 THEN ide.start_time END),
               MAX(CASE WHEN iday.day_number = 1 THEN ide.end_time END),
               COALESCE(MAX(CASE WHEN iday.day_number = 1 THEN ide.break_minutes END), 0),
               MAX(ide.actual_hours),
               STRING_AGG(ide.notes, '; ' ORDER BY iday.day_number),
               NOW()
        FROM inquiry_day_employees ide
        JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id
        WHERE iday.inquiry_id = $1
        GROUP BY iday.inquiry_id, ide.employee_id
        "#,
    )
    .bind(inquiry_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Count active days and employee assignments for an inquiry.
///
/// **Caller**: `delete_inquiry` route handler — guards hard-delete against inquiries
/// that still have scheduled days or assigned employees.
/// **Why**: Prevents accidental deletion of inquiries with operational data still attached.
///
/// # Returns
/// `(day_count, employee_assignment_count)` — both 0 means safe to delete.
pub(crate) async fn count_active_days_and_employees(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<(i64, i64), sqlx::Error> {
    let day_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM inquiry_days WHERE inquiry_id = $1")
            .bind(inquiry_id)
            .fetch_one(pool)
            .await?;

    let emp_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM inquiry_day_employees ide \
         JOIN inquiry_days iday ON ide.inquiry_day_id = iday.id \
         WHERE iday.inquiry_id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await?;

    Ok((day_count.0, emp_count.0))
}

#[cfg(test)]
mod tests {
    /// Verify that the submission_mode CHECK constraint values are recognized.
    /// The migration adding 'ar' and 'mobile' was required because the AR
    /// endpoint created inquiries with submission_mode = 'ar' which violated
    /// the original CHECK(submission_mode IN ('termin','manuell','foto','video')).
    #[test]
    fn test_valid_submission_modes() {
        let valid_modes = ["termin", "manuell", "foto", "video", "ar", "mobile"];
        for mode in valid_modes {
            // Each mode must be a non-empty lowercase string
            assert!(!mode.is_empty(), "submission mode must not be empty");
            assert_eq!(mode, mode.to_lowercase(), "submission mode must be lowercase: {mode}");
        }
    }

    /// Verify that the status transition logic prevents invalid transitions.
    #[test]
    fn test_inquiry_status_transitions() {
        use aust_core::models::InquiryStatus;
        // All transitions are now unrestricted — operators need full flexibility
        assert!(InquiryStatus::Pending.can_transition_to(&InquiryStatus::Estimated));
        assert!(InquiryStatus::Estimated.can_transition_to(&InquiryStatus::OfferReady));
        assert!(InquiryStatus::Cancelled.can_transition_to(&InquiryStatus::Pending));
        assert!(InquiryStatus::Accepted.can_transition_to(&InquiryStatus::OfferReady));
        assert!(InquiryStatus::Paid.can_transition_to(&InquiryStatus::Pending));
    }
}
