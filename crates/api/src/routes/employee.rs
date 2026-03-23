use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::middleware::employee_auth::EmployeeClaims;
use crate::repositories::employee_repo;
use crate::services::otp_service::{
    self, OtpBackend, OtpRequest, OtpResponse, VerifyRequest,
};
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
        .route("/jobs/{id}/clock", axum::routing::patch(patch_employee_clock))
        .route("/hours", get(get_hours))
}

// ---------------------------------------------------------------------------
// OTP Auth (delegates to shared otp_service)
// ---------------------------------------------------------------------------

/// Employee OTP backend — routes OTP operations to `employee_otps` / `employee_sessions` tables.
///
/// **Caller**: `request_otp`, `verify_otp` handlers below.
/// **Why**: Implements `OtpBackend` so the shared OTP logic works for employee auth.
///          Employees must exist before a code is sent (`check_existence_before_send` = true)
///          to avoid leaking whether an email is registered.
struct EmployeeOtpBackend;

impl OtpBackend for EmployeeOtpBackend {
    fn check_existence_before_send(&self) -> bool {
        true
    }

    async fn user_exists(&self, pool: &PgPool, email: &str) -> Result<bool, sqlx::Error> {
        Ok(employee_repo::find_active_by_email(pool, email)
            .await?
            .is_some())
    }

    async fn count_recent_otps(&self, pool: &PgPool, email: &str) -> Result<i64, sqlx::Error> {
        employee_repo::count_recent_otps(pool, email).await
    }

    async fn insert_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        employee_repo::insert_otp(pool, email, code, expires_at).await
    }

    async fn find_valid_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        employee_repo::find_valid_otp(pool, email, code, now).await
    }

    async fn mark_otp_used(&self, pool: &PgPool, otp_id: Uuid) -> Result<(), sqlx::Error> {
        employee_repo::mark_otp_used(pool, otp_id).await
    }

    fn otp_email_subject(&self) -> &str {
        "Ihr Zugangscode — Aust Umzüge Mitarbeiterportal"
    }

    fn request_success_message(&self) -> &str {
        "Falls diese E-Mail registriert ist, wurde ein Code gesendet."
    }

    fn user_label(&self) -> &str {
        "Employee"
    }
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
    let resp = otp_service::handle_request_otp(
        &EmployeeOtpBackend,
        &state.db,
        &state.config.email,
        &body.email,
    )
    .await?;
    Ok(Json(resp))
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

    // Shared OTP validation + token generation
    let token =
        otp_service::handle_verify_otp(&EmployeeOtpBackend, &state.db, &body.email, &body.code)
            .await?;

    // Look up employee
    let (emp_id, first_name, last_name, salutation, phone) =
        employee_repo::fetch_by_email(&state.db, &email)
            .await?
            .ok_or_else(|| ApiError::Unauthorized("Mitarbeiter nicht gefunden oder inaktiv".into()))?;

    // Create 30-day session
    let now = Utc::now();
    let expires_at = now + chrono::Duration::days(30);
    employee_repo::create_session(&state.db, emp_id, &token, expires_at).await?;

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
    let profile = employee_repo::fetch_profile(&state.db, claims.employee_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    let (id, first_name, last_name, salutation, phone) = profile;

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

    let job_rows = employee_repo::fetch_schedule_jobs(&state.db, claims.employee_id, from_date, to_date).await?;

    // Fetch colleague names for each job in one batch query
    let inquiry_ids: Vec<Uuid> = job_rows.iter().map(|r| r.inquiry_id).collect();
    let colleague_map = fetch_colleague_names(&state.db, &inquiry_ids, claims.employee_id).await?;

    let mut entries: Vec<ScheduleJob> = job_rows
        .into_iter()
        .map(|r| {
            let colleagues = colleague_map.get(&r.inquiry_id).cloned().unwrap_or_default();
            ScheduleJob {
                entry_type: "job".into(),
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

    let item_rows = employee_repo::fetch_schedule_items(&state.db, claims.employee_id, from_date, to_date).await?;

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
    origin_floor: Option<String>,
    origin_elevator: Option<bool>,
    // Destination
    destination_street: Option<String>,
    destination_city: Option<String>,
    destination_postal_code: Option<String>,
    destination_floor: Option<String>,
    destination_elevator: Option<bool>,
    // Volume
    estimated_volume_m3: Option<f64>,
    items: Vec<ItemInfo>,
    // Customer contact — phone only, no financial data
    customer_name: Option<String>,
    customer_phone: Option<String>,
    // Assignment
    planned_hours: f64,
    notes: Option<String>,
    // Employee self-reported times (editable via PATCH /jobs/{id}/clock)
    employee_clock_in: Option<DateTime<Utc>>,
    employee_clock_out: Option<DateTime<Utc>>,
    employee_actual_hours: Option<f64>,
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
    // Verify assignment + fetch assignment fields (including employee self-reported times)
    let assign = employee_repo::fetch_assignment(&state.db, inquiry_id, claims.employee_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Einsatz nicht gefunden oder keine Berechtigung".into()))?;

    // Fetch inquiry + addresses + customer
    let row = employee_repo::fetch_job_inquiry(&state.db, inquiry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Compute employee actual hours from their self-reported times
    let employee_actual_hours = match (assign.employee_clock_in, assign.employee_clock_out) {
        (Some(ci), Some(co)) => {
            let secs = (co - ci).num_seconds();
            if secs > 0 {
                Some(secs as f64 / 3600.0)
            } else {
                None
            }
        }
        _ => None,
    };

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
        planned_hours: assign.planned_hours,
        notes: assign.notes,
        employee_clock_in: assign.employee_clock_in,
        employee_clock_out: assign.employee_clock_out,
        employee_actual_hours,
        colleague_names,
    }))
}

// ---------------------------------------------------------------------------
// Protected: Employee self-reported clock times
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ClockBody {
    employee_clock_in: Option<String>,
    employee_clock_out: Option<String>,
}

/// `PATCH /employee/jobs/{id}/clock` — employee submits their own clock-in/out.
///
/// **Caller**: Worker portal job detail page (post-job time logging).
/// **Why**: Employees report their actual start/end times independently of what the admin sets.
///          Both sets are stored and shown side-by-side in the admin interface for
///          discrepancy checking. Only the authenticated employee can write their own times.
///
/// # Parameters
/// - `id` — Inquiry UUID
/// - `employee_clock_in`  — ISO 8601 datetime string or null to clear
/// - `employee_clock_out` — ISO 8601 datetime string or null to clear
async fn patch_employee_clock(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Path(inquiry_id): Path<Uuid>,
    Json(body): Json<ClockBody>,
) -> Result<StatusCode, ApiError> {
    let parse_dt = |s: Option<String>| -> Result<Option<DateTime<Utc>>, ApiError> {
        match s {
            None => Ok(None),
            Some(ref v) if v.is_empty() => Ok(None),
            Some(v) => v
                .parse::<DateTime<Utc>>()
                .map(Some)
                .map_err(|_| ApiError::Validation(format!("Ungültiges Datumsformat: {v}"))),
        }
    };

    let clock_in = parse_dt(body.employee_clock_in)?;
    let clock_out = parse_dt(body.employee_clock_out)?;

    let rows_affected = employee_repo::update_clock_times(&state.db, inquiry_id, claims.employee_id, clock_in, clock_out).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(
            "Einsatz nicht gefunden oder keine Berechtigung".into(),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
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
    let target_hours = employee_repo::fetch_monthly_target(&state.db, claims.employee_id)
        .await?
        .unwrap_or(0.0);

    let rows = employee_repo::fetch_hours_entries(&state.db, claims.employee_id, from_date, to_date).await?;

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
    employee_repo::fetch_item_colleague_names(pool, item_ids, exclude_employee_id).await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Fetch colleague names (other employees assigned to the same inquiries, excluding self).
///
/// Returns a map of inquiry_id → list of "FirstName LastName" strings.
async fn fetch_colleague_names(
    pool: &sqlx::PgPool,
    inquiry_ids: &[Uuid],
    exclude_employee_id: Uuid,
) -> Result<HashMap<Uuid, Vec<String>>, ApiError> {
    employee_repo::fetch_colleague_names(pool, inquiry_ids, exclude_employee_id).await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Fetch and parse items from the latest volume estimation for an inquiry.
///
/// Extracts items from the `result_data` JSON stored in `volume_estimations`.
/// Returns an empty vec if no estimation exists or the JSON cannot be parsed.
async fn fetch_estimation_items(
    pool: &sqlx::PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<ItemInfo>, ApiError> {
    let data = employee_repo::fetch_estimation_result_data(pool, inquiry_id).await?;

    let Some(data) = data else {
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

