use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{NaiveDate, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::middleware::employee_auth::EmployeeClaims;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Router constructors
// ---------------------------------------------------------------------------

/// Public employee auth routes (no token required).
///
/// **Caller**: `routes/mod.rs` — mounted under `/employee` in the public API.
pub fn auth_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/request", post(request_otp))
        .route("/auth/verify", post(verify_otp))
}

/// Protected employee routes (require employee session token via `require_employee_auth`).
///
/// **Caller**: `lib.rs` — wrapped with `require_employee_auth` middleware.
pub fn protected_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me", get(get_profile))
        .route("/schedule", get(get_schedule))
        .route("/jobs/{id}", get(get_job_detail))
        .route("/hours", get(get_hours))
}

// ---------------------------------------------------------------------------
// OTP Auth
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OtpRequest {
    email: String,
}

#[derive(Debug, Serialize)]
struct OtpResponse {
    message: String,
}

/// `POST /employee/auth/request` — generate a 6-digit OTP and send to the employee's email.
///
/// **Caller**: Worker portal login page.
/// **Why**: Magic-link / OTP login so employees don't need to manage passwords.
///          Only sends a code if the email matches a known employee — otherwise returns
///          the same generic message to avoid leaking which emails exist.
///
/// # Returns
/// Always `200 OK` with a generic message (no information leakage).
async fn request_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OtpRequest>,
) -> Result<Json<OtpResponse>, ApiError> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::Validation("Ungültige E-Mail-Adresse".into()));
    }

    // Check employee exists — do NOT reveal whether it does via the response
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM employees WHERE LOWER(email) = $1 AND active = TRUE")
            .bind(&email)
            .fetch_optional(&state.db)
            .await?;

    // Rate limit: max 3 OTPs per email in last 10 minutes
    let recent: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM employee_otps WHERE email = $1 AND created_at > NOW() - INTERVAL '10 minutes'",
    )
    .bind(&email)
    .fetch_one(&state.db)
    .await?;

    if recent.0 >= 3 {
        return Err(ApiError::BadRequest(
            "Zu viele Anfragen. Bitte warten Sie einige Minuten.".into(),
        ));
    }

    // Only actually send if the employee exists
    if exists.is_some() {
        let code: String = {
            let mut rng = rand::rng();
            format!("{:06}", rng.random_range(0..1_000_000u32))
        };
        let expires_at = Utc::now() + chrono::Duration::minutes(10);

        sqlx::query(
            "INSERT INTO employee_otps (email, code, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(&email)
        .bind(&code)
        .bind(expires_at)
        .execute(&state.db)
        .await?;

        let subject = "Ihr Zugangscode — Aust Umzüge Mitarbeiterportal";
        let body_text = format!(
            "Guten Tag,\n\nIhr Zugangscode lautet: {code}\n\nDieser Code ist 10 Minuten gültig.\n\nMit freundlichen Grüßen,\nAust Umzüge"
        );

        if let Err(e) = send_otp_email(&state.config.email, &email, subject, &body_text).await {
            tracing::error!("Failed to send employee OTP to {email}: {e}");
            return Err(ApiError::Internal("E-Mail konnte nicht gesendet werden".into()));
        }

        tracing::info!(email = %email, "Employee OTP sent");
    }

    Ok(Json(OtpResponse {
        message: "Falls diese E-Mail registriert ist, wurde ein Code gesendet.".to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    email: String,
    code: String,
}

#[derive(Debug, Serialize)]
struct VerifyResponse {
    token: String,
    employee: EmployeeInfo,
}

#[derive(Debug, Serialize)]
struct EmployeeInfo {
    id: Uuid,
    email: String,
    first_name: String,
    last_name: String,
    salutation: Option<String>,
    phone: Option<String>,
}

/// `POST /employee/auth/verify` — validate OTP, create 30-day session, return token.
///
/// **Caller**: Worker portal login page (code entry step).
/// **Why**: Second factor of the OTP flow — validates the code and issues a session token
///          the frontend stores in localStorage for persistence.
///
/// # Returns
/// `200 OK` with session `token` and basic `employee` profile.
async fn verify_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let email = body.email.trim().to_lowercase();
    let code = body.code.trim().to_string();

    if code.len() != 6 {
        return Err(ApiError::Validation("Code muss 6 Stellen haben".into()));
    }

    // Find matching unused, non-expired OTP
    let otp_row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT id FROM employee_otps
        WHERE email = $1 AND code = $2 AND used = FALSE AND expires_at > $3
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(&email)
    .bind(&code)
    .bind(Utc::now())
    .fetch_optional(&state.db)
    .await?;

    let (otp_id,) = otp_row.ok_or_else(|| {
        ApiError::Unauthorized("Ungültiger oder abgelaufener Code".into())
    })?;

    // Mark used
    sqlx::query("UPDATE employee_otps SET used = TRUE WHERE id = $1")
        .bind(otp_id)
        .execute(&state.db)
        .await?;

    // Look up employee
    let emp: Option<(Uuid, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, first_name, last_name, salutation, phone FROM employees WHERE LOWER(email) = $1 AND active = TRUE",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;

    let (emp_id, first_name, last_name, salutation, phone) = emp.ok_or_else(|| {
        ApiError::Unauthorized("Mitarbeiter nicht gefunden oder inaktiv".into())
    })?;

    // Create 30-day session
    let token = generate_session_token();
    let expires_at = Utc::now() + chrono::Duration::days(30);

    sqlx::query(
        "INSERT INTO employee_sessions (employee_id, token, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(emp_id)
    .bind(&token)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    tracing::info!(employee_id = %emp_id, email = %email, "Employee authenticated via OTP");

    Ok(Json(VerifyResponse {
        token,
        employee: EmployeeInfo {
            id: emp_id,
            email,
            first_name,
            last_name,
            salutation,
            phone,
        },
    }))
}

// ---------------------------------------------------------------------------
// Protected: Profile
// ---------------------------------------------------------------------------

/// `GET /employee/me` — return the authenticated employee's profile.
///
/// **Caller**: Worker portal (on load to populate header/nav).
/// **Why**: Lets the frontend display the employee's name without storing it separately.
async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
) -> Result<Json<EmployeeInfo>, ApiError> {
    let row: Option<(Uuid, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, first_name, last_name, salutation, phone FROM employees WHERE id = $1",
    )
    .bind(claims.employee_id)
    .fetch_optional(&state.db)
    .await?;

    let (id, first_name, last_name, salutation, phone) =
        row.ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    Ok(Json(EmployeeInfo {
        id,
        email: claims.email,
        first_name,
        last_name,
        salutation,
        phone,
    }))
}

// ---------------------------------------------------------------------------
// Protected: Schedule
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MonthQuery {
    month: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScheduleJob {
    /// `"job"` for moving inquiries, `"item"` for internal calendar items.
    entry_type: String,
    inquiry_id: Option<Uuid>,
    calendar_item_id: Option<Uuid>,
    title: Option<String>,
    location: Option<String>,
    category: Option<String>,
    job_date: Option<NaiveDate>,
    status: String,
    origin_street: Option<String>,
    origin_city: Option<String>,
    origin_postal_code: Option<String>,
    destination_street: Option<String>,
    destination_city: Option<String>,
    destination_postal_code: Option<String>,
    estimated_volume_m3: Option<f64>,
    customer_name: Option<String>,
    customer_phone: Option<String>,
    planned_hours: f64,
    actual_hours: Option<f64>,
    colleague_names: Vec<String>,
}

/// `GET /employee/schedule?month=YYYY-MM` — list assigned jobs and calendar items for a given month.
///
/// **Caller**: Worker portal schedule page.
/// **Why**: Shows the employee only their own jobs and internal events with logistics info —
///          no financial data. Defaults to the current month when no `month` param is supplied.
///          Returns a combined, date-sorted list of moving jobs (`entry_type="job"`) and
///          internal calendar items (`entry_type="item"`).
///
/// # Returns
/// Array of `ScheduleJob` objects ordered by job date ascending.
async fn get_schedule(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Query(query): Query<MonthQuery>,
) -> Result<Json<Vec<ScheduleJob>>, ApiError> {
    let month_str = query
        .month
        .unwrap_or_else(|| Utc::now().format("%Y-%m").to_string());

    let (from_date, to_date) = parse_month_range(&month_str)
        .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat (erwartet: YYYY-MM)".into()))?;

    // -----------------------------------------------------------------------
    // Fetch moving inquiry assignments
    // -----------------------------------------------------------------------

    #[derive(sqlx::FromRow)]
    struct JobRow {
        inquiry_id: Uuid,
        job_date: Option<NaiveDate>,
        status: String,
        origin_street: Option<String>,
        origin_city: Option<String>,
        origin_postal_code: Option<String>,
        destination_street: Option<String>,
        destination_city: Option<String>,
        destination_postal_code: Option<String>,
        estimated_volume_m3: Option<f64>,
        customer_name: Option<String>,
        customer_phone: Option<String>,
        planned_hours: f64,
        actual_hours: Option<f64>,
    }

    let job_rows: Vec<JobRow> = sqlx::query_as(
        r#"
        SELECT
            ie.inquiry_id,
            COALESCE(i.scheduled_date, i.preferred_date::date) AS job_date,
            i.status,
            oa.street      AS origin_street,
            oa.city        AS origin_city,
            oa.postal_code AS origin_postal_code,
            da.street      AS destination_street,
            da.city        AS destination_city,
            da.postal_code AS destination_postal_code,
            i.estimated_volume_m3,
            COALESCE(c.first_name || ' ' || c.last_name, c.name) AS customer_name,
            c.phone AS customer_phone,
            ie.planned_hours::float8 AS planned_hours,
            CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                 THEN EXTRACT(EPOCH FROM (ie.clock_out - ie.clock_in)) / 3600.0
                 ELSE NULL END AS actual_hours
        FROM inquiry_employees ie
        JOIN inquiries  i  ON ie.inquiry_id = i.id
        JOIN customers  c  ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id      = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ie.employee_id = $1
          AND COALESCE(i.scheduled_date, i.preferred_date::date, ie.created_at::date)
              BETWEEN $2 AND $3
        ORDER BY COALESCE(i.scheduled_date, i.preferred_date::date) ASC NULLS LAST
        "#,
    )
    .bind(claims.employee_id)
    .bind(from_date)
    .bind(to_date)
    .fetch_all(&state.db)
    .await?;

    // Fetch colleague names for each job in one batch query
    let inquiry_ids: Vec<Uuid> = job_rows.iter().map(|r| r.inquiry_id).collect();
    let colleague_map = fetch_colleague_names(&state.db, &inquiry_ids, claims.employee_id).await?;

    let mut entries: Vec<ScheduleJob> = job_rows
        .into_iter()
        .map(|r| {
            let colleagues = colleague_map.get(&r.inquiry_id).cloned().unwrap_or_default();
            ScheduleJob {
                entry_type: "job".to_string(),
                inquiry_id: Some(r.inquiry_id),
                calendar_item_id: None,
                title: None,
                location: None,
                category: None,
                job_date: r.job_date,
                status: r.status,
                origin_street: r.origin_street,
                origin_city: r.origin_city,
                origin_postal_code: r.origin_postal_code,
                destination_street: r.destination_street,
                destination_city: r.destination_city,
                destination_postal_code: r.destination_postal_code,
                estimated_volume_m3: r.estimated_volume_m3,
                customer_name: r.customer_name,
                customer_phone: r.customer_phone,
                planned_hours: r.planned_hours,
                actual_hours: r.actual_hours,
                colleague_names: colleagues,
            }
        })
        .collect();

    // -----------------------------------------------------------------------
    // Fetch calendar item assignments
    // -----------------------------------------------------------------------

    #[derive(sqlx::FromRow)]
    struct ItemRow {
        calendar_item_id: Uuid,
        title: String,
        location: Option<String>,
        category: String,
        scheduled_date: Option<NaiveDate>,
        status: String,
        planned_hours: f64,
        actual_hours: Option<f64>,
    }

    let item_rows: Vec<ItemRow> = sqlx::query_as(
        r#"
        SELECT
            cie.calendar_item_id,
            ci.title,
            ci.location,
            ci.category,
            ci.scheduled_date,
            ci.status,
            cie.planned_hours::float8 AS planned_hours,
            CASE WHEN cie.clock_out IS NOT NULL AND cie.clock_in IS NOT NULL
                 THEN EXTRACT(EPOCH FROM (cie.clock_out - cie.clock_in)) / 3600.0
                 ELSE NULL END AS actual_hours
        FROM calendar_item_employees cie
        JOIN calendar_items ci ON ci.id = cie.calendar_item_id
        WHERE cie.employee_id = $1
          AND ci.scheduled_date BETWEEN $2 AND $3
        ORDER BY ci.scheduled_date ASC NULLS LAST
        "#,
    )
    .bind(claims.employee_id)
    .bind(from_date)
    .bind(to_date)
    .fetch_all(&state.db)
    .await?;

    // Fetch colleague names for each calendar item (same batch pattern)
    let item_ids: Vec<Uuid> = item_rows.iter().map(|r| r.calendar_item_id).collect();
    let item_colleague_map =
        fetch_item_colleague_names(&state.db, &item_ids, claims.employee_id).await?;

    for r in item_rows {
        let colleagues = item_colleague_map
            .get(&r.calendar_item_id)
            .cloned()
            .unwrap_or_default();
        entries.push(ScheduleJob {
            entry_type: "item".to_string(),
            inquiry_id: None,
            calendar_item_id: Some(r.calendar_item_id),
            title: Some(r.title),
            location: r.location,
            category: Some(r.category),
            job_date: r.scheduled_date,
            status: r.status,
            origin_street: None,
            origin_city: None,
            origin_postal_code: None,
            destination_street: None,
            destination_city: None,
            destination_postal_code: None,
            estimated_volume_m3: None,
            customer_name: None,
            customer_phone: None,
            planned_hours: r.planned_hours,
            actual_hours: r.actual_hours,
            colleague_names: colleagues,
        });
    }

    // Sort combined list by date ascending, nulls last
    entries.sort_by(|a, b| match (a.job_date, b.job_date) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
        (Some(da), Some(db)) => da.cmp(&db),
    });

    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Protected: Job Detail
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JobDetail {
    inquiry_id: Uuid,
    job_date: Option<NaiveDate>,
    status: String,
    // Origin
    origin_street: Option<String>,
    origin_city: Option<String>,
    origin_postal_code: Option<String>,
    origin_floor: Option<i32>,
    origin_elevator: Option<bool>,
    // Destination
    destination_street: Option<String>,
    destination_city: Option<String>,
    destination_postal_code: Option<String>,
    destination_floor: Option<i32>,
    destination_elevator: Option<bool>,
    // Volume
    estimated_volume_m3: Option<f64>,
    items: Vec<ItemInfo>,
    // Customer contact (logistics only)
    customer_name: Option<String>,
    customer_phone: Option<String>,
    customer_email: Option<String>,
    // Assignment
    planned_hours: f64,
    actual_hours: Option<f64>,
    notes: Option<String>,
    // Team
    colleague_names: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ItemInfo {
    name: String,
    volume_m3: Option<f64>,
    quantity: i32,
}

/// `GET /employee/jobs/{id}` — full logistics detail for one assigned job.
///
/// **Caller**: Worker portal job detail page.
/// **Why**: Gives the employee everything they need to do the job — addresses, items,
///          customer contact, teammates. Financial data (price, offer) is excluded.
///          Verifies the requesting employee is actually assigned to this inquiry.
async fn get_job_detail(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<JobDetail>, ApiError> {
    // Verify assignment + fetch assignment fields
    #[derive(sqlx::FromRow)]
    struct AssignRow {
        planned_hours: f64,
        actual_hours: Option<f64>,
        notes: Option<String>,
    }

    let assign: Option<AssignRow> = sqlx::query_as(
        "SELECT planned_hours::float8, CASE WHEN clock_out IS NOT NULL AND clock_in IS NOT NULL THEN EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0 ELSE NULL END AS actual_hours, notes FROM inquiry_employees WHERE inquiry_id = $1 AND employee_id = $2",
    )
    .bind(inquiry_id)
    .bind(claims.employee_id)
    .fetch_optional(&state.db)
    .await?;

    let assign = assign.ok_or_else(|| {
        ApiError::NotFound("Einsatz nicht gefunden oder keine Berechtigung".into())
    })?;

    // Fetch inquiry + addresses + customer
    #[derive(sqlx::FromRow)]
    struct InquiryRow {
        job_date: Option<NaiveDate>,
        status: String,
        estimated_volume_m3: Option<f64>,
        origin_street: Option<String>,
        origin_city: Option<String>,
        origin_postal_code: Option<String>,
        origin_floor: Option<i32>,
        origin_elevator: Option<bool>,
        destination_street: Option<String>,
        destination_city: Option<String>,
        destination_postal_code: Option<String>,
        destination_floor: Option<i32>,
        destination_elevator: Option<bool>,
        customer_name: Option<String>,
        customer_phone: Option<String>,
        customer_email: Option<String>,
    }

    let row: Option<InquiryRow> = sqlx::query_as(
        r#"
        SELECT
            COALESCE(i.scheduled_date, i.preferred_date::date) AS job_date,
            i.status,
            i.estimated_volume_m3,
            oa.street      AS origin_street,
            oa.city        AS origin_city,
            oa.postal_code AS origin_postal_code,
            oa.floor       AS origin_floor,
            oa.elevator    AS origin_elevator,
            da.street      AS destination_street,
            da.city        AS destination_city,
            da.postal_code AS destination_postal_code,
            da.floor       AS destination_floor,
            da.elevator    AS destination_elevator,
            COALESCE(c.first_name || ' ' || c.last_name, c.name) AS customer_name,
            c.phone  AS customer_phone,
            c.email  AS customer_email
        FROM inquiries i
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id      = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE i.id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Fetch items from latest volume estimation
    let items = fetch_estimation_items(&state.db, inquiry_id).await?;

    // Fetch colleagues
    let colleague_map =
        fetch_colleague_names(&state.db, &[inquiry_id], claims.employee_id).await?;
    let colleague_names = colleague_map.get(&inquiry_id).cloned().unwrap_or_default();

    Ok(Json(JobDetail {
        inquiry_id,
        job_date: row.job_date,
        status: row.status,
        origin_street: row.origin_street,
        origin_city: row.origin_city,
        origin_postal_code: row.origin_postal_code,
        origin_floor: row.origin_floor,
        origin_elevator: row.origin_elevator,
        destination_street: row.destination_street,
        destination_city: row.destination_city,
        destination_postal_code: row.destination_postal_code,
        destination_floor: row.destination_floor,
        destination_elevator: row.destination_elevator,
        estimated_volume_m3: row.estimated_volume_m3,
        items,
        customer_name: row.customer_name,
        customer_phone: row.customer_phone,
        customer_email: row.customer_email,
        planned_hours: assign.planned_hours,
        actual_hours: assign.actual_hours,
        notes: assign.notes,
        colleague_names,
    }))
}

// ---------------------------------------------------------------------------
// Protected: Hours
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct HoursSummary {
    month: String,
    target_hours: f64,
    planned_hours: f64,
    actual_hours: f64,
    assignment_count: usize,
    assignments: Vec<HoursEntry>,
}

#[derive(Debug, Serialize)]
struct HoursEntry {
    /// `"job"` for moving inquiries, `"item"` for internal calendar items.
    entry_type: String,
    inquiry_id: Option<Uuid>,
    calendar_item_id: Option<Uuid>,
    title: Option<String>,
    location: Option<String>,
    job_date: Option<NaiveDate>,
    origin_city: Option<String>,
    destination_city: Option<String>,
    planned_hours: f64,
    actual_hours: Option<f64>,
    status: String,
}

/// `GET /employee/hours?month=YYYY-MM` — monthly hours overview for the authenticated employee.
///
/// **Caller**: Worker portal hours page.
/// **Why**: Employees need to see their own planned vs. actual hours per month across both
///          moving jobs and internal calendar items (training, maintenance, etc.).
///          No financial data included.
async fn get_hours(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Query(query): Query<MonthQuery>,
) -> Result<Json<HoursSummary>, ApiError> {
    let month_str = query
        .month
        .unwrap_or_else(|| Utc::now().format("%Y-%m").to_string());

    let (from_date, to_date) = parse_month_range(&month_str)
        .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat (erwartet: YYYY-MM)".into()))?;

    // Employee's monthly target
    let target: Option<(f64,)> = sqlx::query_as(
        "SELECT monthly_hours_target::float8 FROM employees WHERE id = $1",
    )
    .bind(claims.employee_id)
    .fetch_optional(&state.db)
    .await?;

    let target_hours = target.map(|t| t.0).unwrap_or(0.0);

    #[derive(sqlx::FromRow)]
    struct HoursRow {
        entry_type: String,
        inquiry_id: Option<Uuid>,
        calendar_item_id: Option<Uuid>,
        title: Option<String>,
        location: Option<String>,
        job_date: Option<NaiveDate>,
        origin_city: Option<String>,
        destination_city: Option<String>,
        planned_hours: f64,
        actual_hours: Option<f64>,
        status: String,
    }

    let rows: Vec<HoursRow> = sqlx::query_as(
        r#"
        SELECT
            'job'                                              AS entry_type,
            ie.inquiry_id                                     AS inquiry_id,
            NULL::uuid                                        AS calendar_item_id,
            NULL::text                                        AS title,
            NULL::text                                        AS location,
            COALESCE(i.scheduled_date, i.preferred_date::date) AS job_date,
            oa.city                                           AS origin_city,
            da.city                                           AS destination_city,
            ie.planned_hours::float8                          AS planned_hours,
            CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                 THEN EXTRACT(EPOCH FROM (ie.clock_out - ie.clock_in)) / 3600.0
                 ELSE NULL END                                AS actual_hours,
            i.status                                          AS status
        FROM inquiry_employees ie
        JOIN inquiries i ON ie.inquiry_id = i.id
        LEFT JOIN addresses oa ON i.origin_address_id      = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ie.employee_id = $1
          AND COALESCE(i.scheduled_date, i.preferred_date::date, ie.created_at::date)
              BETWEEN $2 AND $3

        UNION ALL

        SELECT
            'item'                   AS entry_type,
            NULL::uuid               AS inquiry_id,
            cie.calendar_item_id     AS calendar_item_id,
            ci.title                 AS title,
            ci.location              AS location,
            ci.scheduled_date        AS job_date,
            NULL::text               AS origin_city,
            NULL::text               AS destination_city,
            cie.planned_hours::float8 AS planned_hours,
            CASE WHEN cie.clock_out IS NOT NULL AND cie.clock_in IS NOT NULL
                 THEN EXTRACT(EPOCH FROM (cie.clock_out - cie.clock_in)) / 3600.0
                 ELSE NULL END        AS actual_hours,
            ci.status                AS status
        FROM calendar_item_employees cie
        JOIN calendar_items ci ON ci.id = cie.calendar_item_id
        WHERE cie.employee_id = $1
          AND ci.scheduled_date BETWEEN $2 AND $3

        ORDER BY job_date ASC NULLS LAST
        "#,
    )
    .bind(claims.employee_id)
    .bind(from_date)
    .bind(to_date)
    .fetch_all(&state.db)
    .await?;

    let mut planned_sum = 0.0_f64;
    let mut actual_sum = 0.0_f64;

    let assignments: Vec<HoursEntry> = rows
        .into_iter()
        .map(|r| {
            planned_sum += r.planned_hours;
            if let Some(a) = r.actual_hours {
                actual_sum += a;
            }
            HoursEntry {
                entry_type: r.entry_type,
                inquiry_id: r.inquiry_id,
                calendar_item_id: r.calendar_item_id,
                title: r.title,
                location: r.location,
                job_date: r.job_date,
                origin_city: r.origin_city,
                destination_city: r.destination_city,
                planned_hours: r.planned_hours,
                actual_hours: r.actual_hours,
                status: r.status,
            }
        })
        .collect();

    Ok(Json(HoursSummary {
        month: month_str,
        target_hours,
        planned_hours: planned_sum,
        actual_hours: actual_sum,
        assignment_count: assignments.len(),
        assignments,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a secure 64-character hex session token.
fn generate_session_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Parse "YYYY-MM" into (first_day, last_day) NaiveDate range.
fn parse_month_range(month: &str) -> Option<(NaiveDate, NaiveDate)> {
    let parts: Vec<&str> = month.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let year: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let from = NaiveDate::from_ymd_opt(year, m, 1)?;
    let to = if m == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, m + 1, 1)?
    }
    .pred_opt()?;
    Some((from, to))
}

/// Fetch colleague names for calendar items (other employees on the same items, excluding self).
///
/// **Caller**: `get_schedule`.
/// **Why**: Employees need to know who else is attending the same training / maintenance
/// session. Mirrors `fetch_colleague_names` but queries `calendar_item_employees`.
///
/// # Returns
/// Map of `calendar_item_id` → list of `"FirstName LastName"` strings.
async fn fetch_item_colleague_names(
    pool: &sqlx::PgPool,
    item_ids: &[Uuid],
    exclude_employee_id: Uuid,
) -> Result<HashMap<Uuid, Vec<String>>, ApiError> {
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }

    #[derive(sqlx::FromRow)]
    struct ColleagueRow {
        calendar_item_id: Uuid,
        first_name: String,
        last_name: String,
    }

    let rows: Vec<ColleagueRow> = sqlx::query_as(
        r#"
        SELECT cie.calendar_item_id, e.first_name, e.last_name
        FROM calendar_item_employees cie
        JOIN employees e ON cie.employee_id = e.id
        WHERE cie.calendar_item_id = ANY($1) AND cie.employee_id != $2
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(item_ids)
    .bind(exclude_employee_id)
    .fetch_all(pool)
    .await?;

    let mut map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for r in rows {
        map.entry(r.calendar_item_id)
            .or_default()
            .push(format!("{} {}", r.first_name, r.last_name));
    }
    Ok(map)
}

/// Fetch colleague names (other employees assigned to the same inquiries, excluding self).
///
/// Returns a map of inquiry_id → list of "FirstName LastName" strings.
async fn fetch_colleague_names(
    pool: &sqlx::PgPool,
    inquiry_ids: &[Uuid],
    exclude_employee_id: Uuid,
) -> Result<HashMap<Uuid, Vec<String>>, ApiError> {
    if inquiry_ids.is_empty() {
        return Ok(HashMap::new());
    }

    #[derive(sqlx::FromRow)]
    struct ColleagueRow {
        inquiry_id: Uuid,
        first_name: String,
        last_name: String,
    }

    let rows: Vec<ColleagueRow> = sqlx::query_as(
        r#"
        SELECT ie.inquiry_id, e.first_name, e.last_name
        FROM inquiry_employees ie
        JOIN employees e ON ie.employee_id = e.id
        WHERE ie.inquiry_id = ANY($1) AND ie.employee_id != $2
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(inquiry_ids)
    .bind(exclude_employee_id)
    .fetch_all(pool)
    .await?;

    let mut map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for r in rows {
        map.entry(r.inquiry_id)
            .or_default()
            .push(format!("{} {}", r.first_name, r.last_name));
    }
    Ok(map)
}

/// Fetch and parse items from the latest volume estimation for an inquiry.
///
/// Extracts items from the `result_data` JSON stored in `volume_estimations`.
/// Returns an empty vec if no estimation exists or the JSON cannot be parsed.
async fn fetch_estimation_items(
    pool: &sqlx::PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<ItemInfo>, ApiError> {
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        r#"
        SELECT result_data FROM volume_estimations
        WHERE inquiry_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    let Some((data,)) = row else {
        return Ok(vec![]);
    };

    // Items are stored under result_data.items as an array
    let items_arr = match data.get("items").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => return Ok(vec![]),
    };

    let items = items_arr
        .iter()
        .filter_map(|v| {
            let name = v.get("name")?.as_str()?.to_string();
            let volume_m3 = v.get("volume_m3").and_then(|x| x.as_f64());
            let quantity = v
                .get("quantity")
                .and_then(|x| x.as_i64())
                .unwrap_or(1) as i32;
            Some(ItemInfo { name, volume_m3, quantity })
        })
        .collect();

    Ok(items)
}

/// Send OTP email via SMTP.
async fn send_otp_email(
    email_config: &aust_core::config::EmailConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_plain_email, send_email};

    let message = build_plain_email(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())
}
