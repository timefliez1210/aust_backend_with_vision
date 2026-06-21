use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
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
        .route("/pending-hours", get(get_pending_hours))
        .route("/jobs/{id}", get(get_job_detail))
        .route("/jobs/{id}/clock", axum::routing::patch(patch_employee_clock))
        .route("/items/{id}", get(get_item_detail))
        .route("/items/{id}/clock", axum::routing::patch(patch_item_clock))
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

    tracing::info!(employee_id = %emp_id, "Employee authenticated via OTP");

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

/// Optional `?date=YYYY-MM-DD` selecting one day of a multi-day inquiry.
///
/// The schedule lists one entry per assigned `inquiry_employees.job_date`, so
/// the job-detail and clock endpoints take the tapped day to stay in sync —
/// without it they would always resolve to the inquiry's primary day.
#[derive(Debug, Deserialize)]
struct DateQuery {
    date: Option<NaiveDate>,
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
    actual_hours: Option<f64>,
    colleague_names: Vec<String>,
    employee_notes: Option<String>,
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
                actual_hours: r.actual_hours,
                colleague_names: colleagues,
                employee_notes: r.employee_notes,
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
            actual_hours: r.actual_hours,
            colleague_names: colleagues,
            employee_notes: r.employee_notes,
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

/// `GET /employee/pending-hours` — past assignments the worker still owes hours for.
///
/// **Caller**: Worker portal blocking modal.
/// **Why**: Workers have no running hours record; instead, any assignment whose day
///          has passed and that they have not logged is surfaced as a mandatory
///          modal. Returns moving jobs and Termine, oldest first.
async fn get_pending_hours(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
) -> Result<Json<Vec<employee_repo::PendingHoursRow>>, ApiError> {
    let today = Utc::now()
        .with_timezone(&chrono_tz::Europe::Berlin)
        .date_naive();
    let rows = employee_repo::fetch_pending_hours(&state.db, claims.employee_id, today).await?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Protected: Job Detail
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JobDetail {
    inquiry_id: Uuid,
    job_date: Option<NaiveDate>,
    // Planned start time of the move (HH:MM:SS). Workers need to know when to
    // start; the end time is intentionally not exposed.
    start_time: Option<NaiveTime>,
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
    notes: Option<String>,
    // Admin note visible to all employees on this job
    employee_notes: Option<String>,
    // Employee self-reported times (editable via PATCH /jobs/{id}/clock).
    // RFC3339 timestamps — the worker UI converts to local HH:MM for display.
    employee_clock_in: Option<chrono::DateTime<chrono::Utc>>,
    employee_clock_out: Option<chrono::DateTime<chrono::Utc>>,
    // Self-reported break (minutes); already subtracted from employee_actual_hours.
    employee_break_minutes: Option<i32>,
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
    Query(query): Query<DateQuery>,
) -> Result<Json<JobDetail>, ApiError> {
    // Verify assignment + fetch assignment fields (including employee self-reported times).
    // `date` selects the tapped day of a multi-day inquiry; without it the earliest day wins.
    let assign = employee_repo::fetch_assignment(&state.db, inquiry_id, claims.employee_id, query.date)
        .await?
        .ok_or_else(|| ApiError::NotFound("Einsatz nicht gefunden oder keine Berechtigung".into()))?;

    // Fetch inquiry + addresses + customer
    let row = employee_repo::fetch_job_inquiry(&state.db, inquiry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Compute employee actual hours from their self-reported times
    let employee_actual_hours = match (assign.employee_clock_in, assign.employee_clock_out) {
        (Some(ci), Some(co)) => {
            let break_secs = assign.employee_break_minutes.unwrap_or(0).max(0) as i64 * 60;
            let secs = (co - ci).num_seconds() - break_secs;
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
        // The assigned day (per-employee, per-day) is authoritative and matches
        // the schedule list. Fall back to the inquiry's scheduled date only for
        // legacy assignments whose job_date is null.
        job_date: assign.job_date.or(row.job_date),
        start_time: row.start_time,
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
        notes: assign.notes,
        employee_notes: row.employee_notes,
        employee_clock_in: assign.employee_clock_in,
        employee_clock_out: assign.employee_clock_out,
        employee_break_minutes: assign.employee_break_minutes,
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
    /// Self-reported break in minutes (informational; admin's break stays authoritative).
    #[serde(default)]
    employee_break_minutes: Option<i32>,
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
    Query(query): Query<DateQuery>,
    Json(body): Json<ClockBody>,
) -> Result<StatusCode, ApiError> {
    // The job date anchors lenient bare-time input (HH:MM / HH:MM:SS). Prefer the
    // day the employee is viewing; fall back to the inquiry's scheduled date.
    let job_date = match query.date {
        Some(d) => d,
        None => employee_repo::fetch_job_inquiry(&state.db, inquiry_id)
            .await?
            .and_then(|r| r.job_date)
            .unwrap_or_else(|| Utc::now().date_naive()),
    };

    let clock_in = parse_clock_time(body.employee_clock_in, job_date)?;
    let clock_out = parse_clock_time(body.employee_clock_out, job_date)?;
    let break_minutes = body.employee_break_minutes.map(|m| m.max(0));

    let rows_affected = employee_repo::update_clock_times(&state.db, inquiry_id, claims.employee_id, clock_in, clock_out, break_minutes, query.date).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(
            "Einsatz nicht gefunden oder keine Berechtigung".into(),
        ));
    }

    // A complete log (start + end) notifies the office via Telegram.
    if let (Some(ci), Some(co)) = (clock_in, clock_out) {
        let ctx = employee_repo::fetch_job_log_notify_ctx(&state.db, claims.employee_id, inquiry_id)
            .await
            .ok()
            .flatten();
        notify_hours_logged(&state, ctx, true, ci, co, break_minutes).await;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Build and send the "worker logged hours" Telegram notification to the office.
///
/// Fire-and-forget: a Telegram failure must never fail the worker's save.
async fn notify_hours_logged(
    state: &AppState,
    ctx: Option<employee_repo::HoursLogNotifyCtx>,
    is_job: bool,
    clock_in: DateTime<Utc>,
    clock_out: DateTime<Utc>,
    break_minutes: Option<i32>,
) {
    let Some(ctx) = ctx else { return };
    let name = format!("{} {}", ctx.first_name, ctx.last_name);
    let label = ctx.job_label.unwrap_or_else(|| "—".into());
    let text = format_hours_log_message(&name, &label, is_job, clock_in, clock_out, break_minutes);
    crate::services::telegram_service::send_admin_message(&state.config.telegram, &text).await;
}

/// Build the German "worker logged hours" message for the office.
///
/// Pure (no I/O) so it can be unit-tested. Times render in Europe/Berlin.
fn format_hours_log_message(
    employee_name: &str,
    job_label: &str,
    is_job: bool,
    clock_in: DateTime<Utc>,
    clock_out: DateTime<Utc>,
    break_minutes: Option<i32>,
) -> String {
    let break_secs = break_minutes.unwrap_or(0).max(0) as i64 * 60;
    let hours = ((clock_out - clock_in).num_seconds() - break_secs).max(0) as f64 / 3600.0;

    let tz = chrono_tz::Europe::Berlin;
    let start = clock_in.with_timezone(&tz).format("%H:%M");
    let end = clock_out.with_timezone(&tz).format("%H:%M");
    let break_part = match break_minutes.unwrap_or(0) {
        0 => String::new(),
        m => format!(", Pause {m} Min"),
    };
    let subject = if is_job {
        format!("den Auftrag von {job_label}")
    } else {
        format!("den Termin „{job_label}“")
    };
    format!("🕒 {employee_name} hat Stunden erfasst: {hours:.1} h ({start}–{end}{break_part}) für {subject}.")
}

/// Parse a clock time string for the employee self-report endpoints.
///
/// Accepts an ISO 8601 datetime (preferred, sent by the worker UI) or a bare
/// `HH:MM` / `HH:MM:SS` combined with `day` (interpreted as UTC). Empty/None → None.
fn parse_clock_time(s: Option<String>, day: NaiveDate) -> Result<Option<DateTime<Utc>>, ApiError> {
    match s {
        None => Ok(None),
        Some(ref v) if v.is_empty() => Ok(None),
        Some(v) => {
            if let Ok(dt) = v.parse::<DateTime<Utc>>() {
                return Ok(Some(dt));
            }
            if let Ok(t) = v.parse::<NaiveTime>() {
                return Ok(Some(day.and_time(t).and_utc()));
            }
            Err(ApiError::Validation(format!("Ungültiges Zeitformat: {v}")))
        }
    }
}

// ---------------------------------------------------------------------------
// Protected: Calendar item (Termin) detail
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CalendarItemDetail {
    calendar_item_id: Uuid,
    job_date: Option<NaiveDate>,
    status: String,
    title: String,
    category: String,
    location: Option<String>,
    description: Option<String>,
    // Planned start/end time (HH:MM:SS) — this is the "start time" workers need.
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    // Per-assignment note + admin note visible to all assigned employees.
    notes: Option<String>,
    employee_notes: Option<String>,
    // Employee self-reported times (editable via PATCH /items/{id}/clock).
    employee_clock_in: Option<DateTime<Utc>>,
    employee_clock_out: Option<DateTime<Utc>>,
    employee_break_minutes: Option<i32>,
    employee_actual_hours: Option<f64>,
    colleague_names: Vec<String>,
}

/// `GET /employee/items/{id}` — full detail for one assigned calendar item (Termin).
///
/// **Caller**: Worker portal Termin detail page.
/// **Why**: Termine (training, maintenance, and moves scheduled on the calendar)
///          must be openable in the worker portal just like inquiry jobs, so the
///          assigned employee can read the title, time, location, description and
///          notes. Verifies the requesting employee is actually assigned.
async fn get_item_detail(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Path(calendar_item_id): Path<Uuid>,
    Query(query): Query<DateQuery>,
) -> Result<Json<CalendarItemDetail>, ApiError> {
    // Verify assignment + fetch assignment fields (incl. employee self-reported times).
    let assign = employee_repo::fetch_item_assignment(&state.db, calendar_item_id, claims.employee_id, query.date)
        .await?
        .ok_or_else(|| ApiError::NotFound("Termin nicht gefunden oder keine Berechtigung".into()))?;

    let row = employee_repo::fetch_item_detail(&state.db, calendar_item_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Termin nicht gefunden".into()))?;

    let employee_actual_hours = match (assign.employee_clock_in, assign.employee_clock_out) {
        (Some(ci), Some(co)) => {
            let break_secs = assign.employee_break_minutes.unwrap_or(0).max(0) as i64 * 60;
            let secs = (co - ci).num_seconds() - break_secs;
            if secs > 0 {
                Some(secs as f64 / 3600.0)
            } else {
                None
            }
        }
        _ => None,
    };

    let colleague_map =
        fetch_item_colleague_names(&state.db, &[calendar_item_id], claims.employee_id).await?;
    let colleague_names = colleague_map.get(&calendar_item_id).cloned().unwrap_or_default();

    Ok(Json(CalendarItemDetail {
        calendar_item_id,
        // The assigned day (per-employee, per-day) is authoritative; fall back to
        // the item's scheduled date for legacy assignments with a null job_date.
        job_date: assign.job_date.or(row.scheduled_date),
        status: row.status,
        title: row.title,
        category: row.category,
        location: row.location,
        description: row.description,
        start_time: row.start_time,
        end_time: row.end_time,
        notes: assign.notes,
        employee_notes: row.employee_notes,
        employee_clock_in: assign.employee_clock_in,
        employee_clock_out: assign.employee_clock_out,
        employee_break_minutes: assign.employee_break_minutes,
        employee_actual_hours,
        colleague_names,
    }))
}

/// `PATCH /employee/items/{id}/clock` — employee submits their own clock-in/out for a Termin.
///
/// **Caller**: Worker portal Termin detail page (post-job time logging).
/// **Why**: Mirrors the inquiry-job clock endpoint so movers can log their actual
///          times on Termine too (these feed the monthly hours summary).
async fn patch_item_clock(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<EmployeeClaims>,
    Path(calendar_item_id): Path<Uuid>,
    Query(query): Query<DateQuery>,
    Json(body): Json<ClockBody>,
) -> Result<StatusCode, ApiError> {
    // The job date anchors lenient bare-time input. Prefer the day the employee is
    // viewing; fall back to the item's scheduled date.
    let job_date = match query.date {
        Some(d) => d,
        None => employee_repo::fetch_item_detail(&state.db, calendar_item_id)
            .await?
            .and_then(|r| r.scheduled_date)
            .unwrap_or_else(|| Utc::now().date_naive()),
    };

    let clock_in = parse_clock_time(body.employee_clock_in, job_date)?;
    let clock_out = parse_clock_time(body.employee_clock_out, job_date)?;
    let break_minutes = body.employee_break_minutes.map(|m| m.max(0));

    let rows_affected = employee_repo::update_item_clock_times(&state.db, calendar_item_id, claims.employee_id, clock_in, clock_out, break_minutes, query.date).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(
            "Termin nicht gefunden oder keine Berechtigung".into(),
        ));
    }

    // A complete log (start + end) notifies the office via Telegram.
    if let (Some(ci), Some(co)) = (clock_in, clock_out) {
        let ctx = employee_repo::fetch_item_log_notify_ctx(&state.db, claims.employee_id, calendar_item_id)
            .await
            .ok()
            .flatten();
        notify_hours_logged(&state, ctx, false, ci, co, break_minutes).await;
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

    let mut actual_sum = 0.0_f64;

    let assignments: Vec<HoursEntry> = rows
        .into_iter()
        .map(|r| {
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
                actual_hours: r.actual_hours,
                status: r.status,
            }
        })
        .collect();

    Ok(Json(HoursSummary {
        month: month_str,
        target_hours,
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

    // Items live either under `result_data.items` (vision pipeline) or as a bare
    // top-level array (admin item editor, `PUT /inquiries/{id}/items`). Accept both
    // so an admin-edited furniture list is visible to the worker.
    let items_arr = match data.get("items").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => match data.as_array() {
            Some(arr) => arr.clone(),
            None => return Ok(vec![]),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // 2026-06-15 is in CEST (UTC+2): a UTC h:m renders as (h+2):m in Berlin.
    fn dt(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 15, h, m, 0).unwrap()
    }

    #[test]
    fn formats_job_hours_message_with_names_break_and_berlin_times() {
        // 06:00–14:30 UTC = 08:00–16:30 Berlin; 8.5 h gross − 30 min break = 8.0 h.
        let msg = format_hours_log_message(
            "Max Helfer",
            "Familie Muster",
            true,
            dt(6, 0),
            dt(14, 30),
            Some(30),
        );
        assert!(msg.contains("Max Helfer"), "{msg}");
        assert!(msg.contains("hat Stunden erfasst"), "{msg}");
        assert!(msg.contains("8.0 h"), "{msg}");
        assert!(msg.contains("den Auftrag von Familie Muster"), "{msg}");
        assert!(msg.contains("08:00"), "{msg}");
        assert!(msg.contains("16:30"), "{msg}");
        assert!(msg.contains("Pause 30 Min"), "{msg}");
    }

    #[test]
    fn formats_termin_message_without_break() {
        let msg =
            format_hours_log_message("Anna Klein", "Lagerumzug", false, dt(6, 0), dt(10, 0), None);
        assert!(msg.contains("Anna Klein"), "{msg}");
        assert!(msg.contains("den Termin „Lagerumzug“"), "{msg}");
        assert!(msg.contains("4.0 h"), "{msg}");
        assert!(!msg.contains("Pause"), "{msg}");
    }

    /// The notification actually POSTs to the (overridable) Telegram endpoint.
    #[tokio::test]
    async fn send_admin_message_hits_the_endpoint() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = counter.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                c2.fetch_add(1, Ordering::SeqCst);
                let _ = s
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
                    .await;
            }
        });

        let base = format!("http://127.0.0.1:{}", addr.port());
        let cfg = aust_core::config::TelegramConfig {
            bot_token: "TEST_BOT_TOKEN".into(),
            admin_chat_id: 42,
            flash_contact_bot_token: "TEST_FLASH".into(),
        };
        crate::services::telegram_service::send_admin_message_with_base(&cfg, &base, "🕒 hallo")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}

