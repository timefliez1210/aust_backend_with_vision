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
    pub preferred_date: Option<DateTime<Utc>>,
    pub scheduled_date: Option<chrono::NaiveDate>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
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
pub(crate) struct QuoteReadiness {
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
               start_time, end_time, notes,
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
) -> Result<Option<QuoteReadiness>, sqlx::Error> {
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
/// **Caller**: `services::db::update_quote_volume` (moved here)
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
    preferred_date: Option<DateTime<Utc>>,
    notes: Option<&str>,
    services: &serde_json::Value,
    source: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id, stop_address_id,
                           status, estimated_volume_m3, distance_km, preferred_date, notes, services, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)
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
    .bind(preferred_date)
    .bind(notes)
    .bind(services)
    .bind(source)
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
    preferred_date: Option<DateTime<Utc>>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    origin_address_id: Option<Uuid>,
    scheduled_date: Option<NaiveDate>,
    destination_address_id: Option<Uuid>,
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
            preferred_date = COALESCE($7, preferred_date),
            scheduled_date = CASE WHEN $7 IS NOT NULL THEN NULL WHEN $11 IS NOT NULL THEN $11 ELSE scheduled_date END,
            start_time = COALESCE($8, start_time),
            end_time = COALESCE($9, end_time),
            origin_address_id = COALESCE($10, origin_address_id),
            destination_address_id = COALESCE($12, destination_address_id),
            updated_at = $13
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(status)
    .bind(notes)
    .bind(services_json)
    .bind(estimated_volume_m3)
    .bind(distance_km)
    .bind(preferred_date)
    .bind(start_time)
    .bind(end_time)
    .bind(origin_address_id)
    .bind(scheduled_date)
    .bind(destination_address_id)
    .bind(now)
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
    pub clock_in: Option<DateTime<Utc>>,
    pub clock_out: Option<DateTime<Utc>>,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
}

/// Fetch employee assignments for an inquiry (with email).
///
/// **Caller**: `list_inquiry_employees` handler
/// **Why**: Shows assigned employees for a job.
pub(crate) async fn list_employee_assignments(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<EmployeeAssignmentRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT ie.employee_id, e.first_name, e.last_name, e.email,
               ie.planned_hours::float8 AS planned_hours,
               ie.clock_in,
               ie.clock_out,
               CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (ie.clock_out - ie.clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
               ie.notes
        FROM inquiry_employees ie
        JOIN employees e ON ie.employee_id = e.id
        WHERE ie.inquiry_id = $1
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
/// **Why**: Links an employee to a moving job.
pub(crate) async fn insert_employee_assignment(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    employee_id: Uuid,
    planned_hours: f64,
    notes: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiry_employees (id, inquiry_id, employee_id, planned_hours, notes)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(notes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update an employee assignment (hours, clock times, notes).
///
/// **Caller**: `update_assignment` handler
/// **Why**: Operators adjust hours after the job.
///
/// # Returns
/// Number of rows affected (0 if not found).
pub(crate) async fn update_employee_assignment(
    pool: &PgPool,
    inquiry_id: Uuid,
    employee_id: Uuid,
    planned_hours: Option<f64>,
    clock_in: Option<DateTime<Utc>>,
    clock_out: Option<DateTime<Utc>>,
    notes: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE inquiry_employees SET
            clock_in  = COALESCE($4, clock_in),
            clock_out = COALESCE($5, clock_out),
            planned_hours = CASE
                WHEN COALESCE($4, clock_in) IS NOT NULL AND COALESCE($5, clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, clock_out) - COALESCE($4, clock_in))) / 3600.0)::float8
                ELSE COALESCE($3, planned_hours)
            END,
            notes = COALESCE($6, notes)
        WHERE inquiry_id = $1 AND employee_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .bind(planned_hours)
    .bind(clock_in)
    .bind(clock_out)
    .bind(notes)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Updated assignment row projection (after update).
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct UpdatedAssignmentRow {
    pub planned_hours: f64,
    pub clock_in: Option<DateTime<Utc>>,
    pub clock_out: Option<DateTime<Utc>>,
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
        SELECT planned_hours::float8 AS planned_hours,
               clock_in,
               clock_out,
               CASE WHEN clock_out IS NOT NULL AND clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
               notes
        FROM inquiry_employees
        WHERE inquiry_id = $1 AND employee_id = $2
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
/// **Why**: Unlinks an employee from a moving job.
///
/// # Returns
/// Number of rows deleted (0 or 1).
pub(crate) async fn delete_employee_assignment(
    pool: &PgPool,
    inquiry_id: Uuid,
    employee_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM inquiry_employees WHERE inquiry_id = $1 AND employee_id = $2",
    )
    .bind(inquiry_id)
    .bind(employee_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Employee assignment snapshot row for the inquiry builder (includes employee_ clock fields).
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct EmployeeAssignmentSnapshotRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub planned_hours: f64,
    pub clock_in: Option<DateTime<Utc>>,
    pub clock_out: Option<DateTime<Utc>>,
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
        SELECT ie.employee_id, e.first_name, e.last_name,
               ie.planned_hours::float8 AS planned_hours,
               ie.clock_in,
               ie.clock_out,
               CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (ie.clock_out - ie.clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
               ie.employee_clock_in,
               ie.employee_clock_out,
               CASE WHEN ie.employee_clock_out IS NOT NULL AND ie.employee_clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (ie.employee_clock_out - ie.employee_clock_in)) / 3600.0)::float8
                    ELSE NULL END AS employee_actual_hours,
               ie.notes
        FROM inquiry_employees ie
        JOIN employees e ON ie.employee_id = e.id
        WHERE ie.inquiry_id = $1
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
    pub customer_email: String,
    pub customer_salutation: Option<String>,
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
    preferred_date: Option<DateTime<Utc>>,
    notes: Option<&str>,
    services: Option<&serde_json::Value>,
    source: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, preferred_date, notes, services, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
        "#,
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .bind(preferred_date)
    .bind(notes)
    .bind(services)
    .bind(source)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
