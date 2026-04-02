use axum::{
    extract::{Multipart, Path, Query, State},
    http::header,
    response::Response,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
use crate::repositories::{admin_repo, employee_repo, feedback_repo};
use crate::{ApiError, AppState};

use super::admin_customers;
use super::admin_emails;

/// Register all admin-panel routes (protected under JWT middleware).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly, nested under the admin
/// JWT authentication middleware.
/// **Why**: Consolidates dashboard, customer, address, email, user, and order endpoints
/// into a single router mounted at `/api/v1/admin`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(dashboard))
        .route("/customers", get(admin_customers::list_customers).post(admin_customers::create_customer))
        .route("/customers/{id}", get(admin_customers::get_customer).patch(admin_customers::update_customer))
        .route("/customers/{id}/delete", post(admin_customers::delete_customer))
        .route("/addresses/{id}", patch(admin_customers::update_address))
        .route("/emails", get(admin_emails::list_email_threads))
        .route("/emails/{id}", get(admin_emails::get_email_thread))
        .route("/emails/messages/{id}", patch(admin_emails::update_draft_email))
        .route("/emails/messages/{id}/send", post(admin_emails::send_draft_email))
        .route("/emails/messages/{id}/discard", post(admin_emails::discard_draft_email))
        .route("/emails/{id}/reply", post(admin_emails::reply_to_thread))
        .route("/emails/compose", post(admin_emails::compose_email))
        .route("/users", get(list_users))
        .route("/users/{id}/delete", post(delete_user))
        .route("/orders", get(list_orders))
        .route("/employees", get(list_employees).post(create_employee))
        .route("/employees/{id}", get(get_employee).patch(update_employee))
        .route("/employees/{id}/delete", post(delete_employee))
        .route("/employees/{id}/hours", get(employee_hours_summary))
        .route(
            "/employees/{id}/documents/{doc_type}",
            post(upload_employee_document)
                .get(download_employee_document)
                .delete(delete_employee_document),
        )
        .route("/notes", get(list_notes).post(create_note))
        .route("/notes/{id}", patch(update_note).delete(delete_note))
        .route("/feedback", get(list_feedback).post(create_feedback))
        .route("/feedback/{id}", get(get_feedback).patch(patch_feedback))
        .route("/feedback/{id}/attachments/{idx}", get(download_feedback_attachment))
}

// --- Dashboard ---

#[derive(Debug, Serialize)]
struct DashboardResponse {
    open_quotes: i64,
    pending_offers: i64,
    todays_bookings: i64,
    total_customers: i64,
    recent_activity: Vec<ActivityItem>,
    conflict_dates: Vec<ConflictDate>,
}

#[derive(Debug, Serialize)]
struct ConflictDate {
    date: NaiveDate,
    booked: i64,
    capacity: i32,
}

#[derive(Debug, Serialize)]
struct ActivityItem {
    #[serde(rename = "type")]
    activity_type: String,
    description: String,
    created_at: DateTime<Utc>,
    /// UUID of the target resource (inquiry id, email thread id, or calendar item id).
    id: Option<Uuid>,
    status: Option<String>,
}

/// `GET /api/v1/admin/dashboard` — Return headline KPIs and recent activity for the dashboard.
///
/// **Caller**: Axum router / admin dashboard home page on load.
/// **Why**: Aggregates open inquiry count, draft offer count, today's bookings, total customers,
/// the 10 most recent offer events, and dates in the next 30 days where bookings exceed
/// capacity — all in one query round-trip for the dashboard overview card.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, config for `calendar.default_capacity`)
/// - `_claims` — JWT claims injected by middleware (unused; auth check performed by middleware)
///
/// # Returns
/// `200 OK` with `DashboardResponse` JSON.
async fn dashboard(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<DashboardResponse>, ApiError> {
    let open_quotes = admin_repo::count_open_inquiries(&state.db).await?;
    let pending_offers = admin_repo::count_pending_offers(&state.db).await?;
    let today = Utc::now().date_naive();
    let todays_bookings = admin_repo::count_todays_bookings(&state.db, today).await?;
    let total_customers = admin_repo::count_total_customers(&state.db).await?;

    let recent_activity_rows = admin_repo::fetch_recent_activity(&state.db)
        .await
        .unwrap_or_default();
    let recent_activity: Vec<ActivityItem> = recent_activity_rows
        .into_iter()
        .map(|r| ActivityItem {
            activity_type: r.activity_type,
            description: r.description,
            created_at: r.created_at,
            id: r.id,
            status: r.status,
        })
        .collect();

    // Find dates in the next 30 days where bookings >= capacity
    let from_date = today;
    let to_date = today + chrono::Days::new(30);
    let default_capacity = state.config.calendar.default_capacity;

    let conflict_rows = admin_repo::fetch_conflict_dates(&state.db, from_date, to_date, default_capacity)
        .await
        .unwrap_or_default();

    let mut conflict_dates = Vec::new();
    for row in conflict_rows {
        let cap = admin_repo::fetch_capacity_override(&state.db, row.booking_date)
            .await
            .unwrap_or(None);

        conflict_dates.push(ConflictDate {
            date: row.booking_date,
            booked: row.booking_count,
            capacity: cap.unwrap_or(default_capacity),
        });
    }

    Ok(Json(DashboardResponse {
        open_quotes,
        pending_offers,
        todays_bookings,
        total_customers,
        recent_activity,
        conflict_dates,
    }))
}

// --- Orders (Auftraege) ---

#[derive(Debug, Deserialize)]
struct ListOrdersQuery {
    status: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct OrderListItem {
    id: Uuid,
    customer_name: Option<String>,
    customer_email: String,
    origin_city: Option<String>,
    destination_city: Option<String>,
    #[serde(rename = "volume_m3")]
    estimated_volume_m3: Option<f64>,
    status: String,
    preferred_date: Option<DateTime<Utc>>,
    offer_price_brutto: Option<i64>,
    booking_date: Option<NaiveDate>,
    created_at: DateTime<Utc>,
    employees_assigned: i64,
    employees_quoted: Option<i32>,
}

#[derive(Debug, Serialize)]
struct OrdersListResponse {
    orders: Vec<OrderListItem>,
    total: i64,
}

/// `GET /api/v1/admin/orders` — List confirmed orders (inquiries in accepted/done/paid status).
///
/// **Caller**: Axum router / admin dashboard "Auftraege" tab.
/// **Why**: Orders are inquiries that have been accepted. This endpoint filters by the three
/// order-phase statuses and joins booking dates and the latest offer's brutto price for
/// the order management table. Results are sorted by `preferred_date` (moving date) ascending.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `status` (single order status filter or all), `search`, `limit`, `offset`
///
/// # Returns
/// `200 OK` with `OrdersListResponse` containing `orders` and `total`.
async fn list_orders(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListOrdersQuery>,
) -> Result<Json<OrdersListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    // Filter by specific sub-status within orders, or show all order statuses
    let status_filter = query.status.as_deref();
    let statuses: &[&str] = match status_filter {
        Some(s) if matches!(s, "accepted" | "scheduled" | "completed" | "invoiced" | "paid") => &[],
        _ => &["accepted", "scheduled", "completed", "invoiced", "paid"],
    };

    let (repo_orders, total) = if statuses.is_empty() {
        let rows = admin_repo::list_orders_single_status(&state.db, status_filter.unwrap(), &search, limit, offset).await?;
        let cnt = admin_repo::count_orders_single_status(&state.db, status_filter.unwrap(), &search).await?;
        (rows, cnt)
    } else {
        let rows = admin_repo::list_orders_all_statuses(&state.db, &search, limit, offset).await?;
        let cnt = admin_repo::count_orders_all_statuses(&state.db, &search).await?;
        (rows, cnt)
    };

    let orders: Vec<OrderListItem> = repo_orders
        .into_iter()
        .map(|r| OrderListItem {
            id: r.id, customer_name: r.customer_name, customer_email: r.customer_email,
            origin_city: r.origin_city, destination_city: r.destination_city,
            estimated_volume_m3: r.estimated_volume_m3, status: r.status,
            preferred_date: r.preferred_date, offer_price_brutto: r.offer_price_brutto,
            booking_date: r.booking_date, created_at: r.created_at,
            employees_assigned: r.employees_assigned, employees_quoted: r.employees_quoted,
        })
        .collect();

    Ok(Json(OrdersListResponse { orders, total }))
}

// --- Users ---

#[derive(Debug, Serialize)]
struct UserListItem {
    id: Uuid,
    email: String,
    name: String,
    role: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct UserListResponse {
    users: Vec<UserListItem>,
}

/// `GET /api/v1/admin/users` — List all admin users.
///
/// **Caller**: Axum router / admin dashboard settings → user management page.
/// **Why**: Shows all registered admin accounts ordered by creation date.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
///
/// # Returns
/// `200 OK` with `UserListResponse` containing all users (id, email, name, role, created_at).
async fn list_users(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<UserListResponse>, ApiError> {
    require_admin(&claims)?;
    let repo_users = admin_repo::list_users(&state.db).await?;
    let users: Vec<UserListItem> = repo_users
        .into_iter()
        .map(|u| UserListItem {
            id: u.id, email: u.email, name: u.name, role: u.role, created_at: u.created_at,
        })
        .collect();

    Ok(Json(UserListResponse { users }))
}

/// `POST /api/v1/admin/users/{id}/delete` — Delete an admin user account.
///
/// **Caller**: Axum router / admin dashboard user management page.
/// **Why**: Hard-deletes the user record. Prevents self-deletion (a user cannot delete
/// their own account) to avoid lockout.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `claims` — JWT claims of the currently authenticated user (used for self-deletion check)
/// - `id` — user UUID path parameter
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `400` if the user tries to delete their own account
/// - `404` if user not found
async fn delete_user(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
    if claims.sub == id {
        return Err(ApiError::Validation(
            "Sie koennen sich nicht selbst loeschen".into(),
        ));
    }

    let rows = admin_repo::delete_user(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(format!("Benutzer {id} nicht gefunden")));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Permission helpers ---

/// Guard that allows only Admin-role users to proceed.
///
/// **Caller**: Sensitive admin handlers (deletes, user management) in admin.rs,
/// admin_customers.rs, and admin_emails.rs.
/// **Why**: Buerokraft users share the admin JWT middleware but should not be able to
///          delete records or access user management.
pub(super) fn require_admin(claims: &TokenClaims) -> Result<(), ApiError> {
    if !claims.role.is_admin() {
        return Err(ApiError::Forbidden(
            "Diese Aktion erfordert Administrator-Berechtigungen".into(),
        ));
    }
    Ok(())
}

// --- Employees ---

#[derive(Debug, Deserialize)]
struct ListEmployeesQuery {
    search: Option<String>,
    active: Option<bool>,
    month: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}


#[derive(Debug, Serialize)]
struct EmployeeListItem {
    id: Uuid,
    salutation: Option<String>,
    first_name: String,
    last_name: String,
    email: String,
    phone: Option<String>,
    monthly_hours_target: f64,
    active: bool,
    planned_hours_month: Option<f64>,
    actual_hours_month: Option<f64>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct EmployeeListResponse {
    employees: Vec<EmployeeListItem>,
    total: i64,
}

/// `GET /api/v1/admin/employees` — List employees with optional search and month filter.
///
/// **Caller**: Admin employees list page.
/// **Why**: Paginated employee listing with monthly hours aggregation.
///
/// # Parameters
/// - `search` — ILIKE on first_name, last_name, email
/// - `active` — filter by active status
/// - `month` — YYYY-MM format; when present, includes planned/actual hours for that month
/// - `limit`, `offset` — pagination
///
/// # Returns
/// `200 OK` with `EmployeeListResponse`.
async fn list_employees(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListEmployeesQuery>,
) -> Result<Json<EmployeeListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query.search.map(|s| format!("%{s}%"));
    let active_filter = query.active;

    let repo_rows = employee_repo::list(&state.db, &search, active_filter, limit, offset).await?;
    let total = employee_repo::count(&state.db, &search, active_filter).await?;

    // Parse month range for hours aggregation
    let month_range = query.month.as_ref().and_then(|m| parse_month_range(m));

    let mut employees = Vec::with_capacity(repo_rows.len());
    for row in repo_rows {
        let (planned, actual) = if let Some((from, to)) = &month_range {
            fetch_employee_month_hours(&state.db, row.id, *from, *to).await?
        } else {
            (None, None)
        };

        employees.push(EmployeeListItem {
            id: row.id,
            salutation: row.salutation,
            first_name: row.first_name,
            last_name: row.last_name,
            email: row.email,
            phone: row.phone,
            monthly_hours_target: row.monthly_hours_target,
            active: row.active,
            planned_hours_month: planned,
            actual_hours_month: actual,
            created_at: row.created_at,
        });
    }

    Ok(Json(EmployeeListResponse { employees, total }))
}

/// `POST /api/v1/admin/employees` — Create a new employee.
///
/// **Caller**: Admin employees page create form.
/// **Why**: Registers a new employee for assignment tracking.
///
/// # Returns
/// `201 Created` with the new `Employee` JSON.
async fn create_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(body): Json<aust_core::models::CreateEmployee>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), ApiError> {
    let target = body.monthly_hours_target.unwrap_or(160.0);
    let id = uuid::Uuid::now_v7();

    employee_repo::create(
        &state.db, id,
        body.salutation.as_deref(), &body.first_name, &body.last_name,
        &body.email, body.phone.as_deref(), target,
    )
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("employees_email_key") {
                return ApiError::Conflict("Ein Mitarbeiter mit dieser E-Mail existiert bereits.".into());
            }
        }
        ApiError::from(e)
    })?;

    let employee = fetch_employee_json(&state.db, id).await?;
    Ok((axum::http::StatusCode::CREATED, Json(employee)))
}

/// `GET /api/v1/admin/employees/{id}` — Get employee detail with recent assignments.
///
/// **Caller**: Admin employee detail page.
/// **Why**: Returns profile + recent inquiry assignments for the employee.
///
/// # Returns
/// `200 OK` with employee profile and assignments array.
async fn get_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let employee = fetch_employee_json(&state.db, id).await?;

    let assignments = employee_repo::fetch_admin_assignments(&state.db, id).await?;

    let assignments_json: Vec<serde_json::Value> = assignments
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "inquiry_id": a.inquiry_id,
                "customer_name": a.customer_name,
                "origin_city": a.origin_city,
                "destination_city": a.destination_city,
                "booking_date": a.booking_date,
                "planned_hours": a.planned_hours,
                "actual_hours": a.actual_hours,
                "notes": a.notes,
                "status": a.inquiry_status,
            })
        })
        .collect();


    let mut result = employee;
    result["assignments"] = serde_json::Value::Array(assignments_json);
    Ok(Json(result))
}

/// `PATCH /api/v1/admin/employees/{id}` — Update employee fields.
///
/// **Caller**: Admin employee detail page save button.
/// **Why**: Partial update of employee profile.
///
/// # Returns
/// `200 OK` with updated employee JSON.
async fn update_employee(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<aust_core::models::UpdateEmployee>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Verify exists
    if !employee_repo::exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into()));
    }

    if let Some(ref sal) = body.salutation {
        if !["Herr", "Frau", "D"].contains(&sal.as_str()) {
            return Err(ApiError::BadRequest("Ungueltige Anrede".into()));
        }
    }

    employee_repo::update(
        &state.db, id,
        body.salutation.as_deref(), body.first_name.as_deref(), body.last_name.as_deref(),
        body.email.as_deref(), body.phone.as_deref(),
        body.monthly_hours_target, body.active,
    )
    .await?;

    let employee = fetch_employee_json(&state.db, id).await?;
    Ok(Json(employee))
}

/// `POST /api/v1/admin/employees/{id}/delete` — Soft-delete employee (set active=false).
///
/// **Caller**: Admin employee list/detail delete button.
/// **Why**: Preserves assignment history while removing from active pool.
///
/// # Returns
/// `204 No Content`.
async fn delete_employee(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_admin(&claims)?;
    let rows = employee_repo::soft_delete(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into()));
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// `GET /api/v1/admin/employees/{id}/hours` — Hours summary for a date range.
///
/// **Caller**: Admin employee detail page hours card.
/// **Why**: Aggregates planned/actual hours for either a 7-day rolling window or a calendar
/// month, with per-assignment breakdown. Employees were confused by the strict monthly view
/// near month boundaries — the 7-day default solves that.
///
/// # Query Parameters
/// - `from` + `to` (YYYY-MM-DD): explicit date range (used for 7-day view)
/// - `month` (YYYY-MM): calendar month; used when `from`/`to` are absent
/// If none are provided, defaults to the current calendar month.
///
/// # Returns
/// `200 OK` with `from`, `to`, `target_hours`, `planned_hours`, `actual_hours`,
/// `assignment_count`, `assignments`, `calendar_items`.
async fn employee_hours_summary(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (from_date, to_date) = if let (Some(f), Some(t)) = (query.get("from"), query.get("to")) {
        let from = NaiveDate::parse_from_str(f, "%Y-%m-%d")
            .map_err(|_| ApiError::BadRequest("Ungueltiges Datumsformat fuer 'from'. Erwartet: YYYY-MM-DD".into()))?;
        let to = NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .map_err(|_| ApiError::BadRequest("Ungueltiges Datumsformat fuer 'to'. Erwartet: YYYY-MM-DD".into()))?;
        (from, to)
    } else {
        let month_str = query.get("month").cloned().unwrap_or_else(|| {
            Utc::now().format("%Y-%m").to_string()
        });
        parse_month_range(&month_str)
            .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat. Erwartet: YYYY-MM".into()))?
    };

    // Fetch employee target
    let target = employee_repo::fetch_hours_target(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    let rows = employee_repo::fetch_admin_hours(&state.db, id, from_date, to_date).await?;

    // Also fetch calendar item assignments for this employee in the same month.
    let item_rows = employee_repo::fetch_admin_calendar_item_hours(&state.db, id, from_date, to_date).await?;

    let mut planned_sum = 0.0_f64;
    let mut actual_sum = 0.0_f64;

    let assignments: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            planned_sum += r.planned_hours;
            if let Some(av) = r.actual_hours {
                actual_sum += av;
            }
            serde_json::json!({
                "inquiry_id": r.inquiry_id,
                "customer_name": r.customer_name,
                "origin_city": r.origin_city,
                "destination_city": r.destination_city,
                "booking_date": r.booking_date,
                "planned_hours": r.planned_hours,
                "clock_in": r.clock_in,
                "clock_out": r.clock_out,
                "actual_hours": r.actual_hours,
                "status": r.inquiry_status,
            })
        })
        .collect();

    let calendar_items: Vec<serde_json::Value> = item_rows
        .into_iter()
        .map(|r| {
            planned_sum += r.planned_hours;
            if let Some(av) = r.actual_hours {
                actual_sum += av;
            }
            serde_json::json!({
                "calendar_item_id": r.calendar_item_id,
                "title": r.title,
                "category": r.category,
                "location": r.location,
                "scheduled_date": r.scheduled_date,
                "planned_hours": r.planned_hours,
                "clock_in": r.clock_in,
                "clock_out": r.clock_out,
                "actual_hours": r.actual_hours,
                "status": r.status,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "from": from_date.to_string(),
        "to": to_date.to_string(),
        "target_hours": target,
        "planned_hours": planned_sum,
        "actual_hours": actual_sum,
        "assignment_count": assignments.len() + calendar_items.len(),
        "assignments": assignments,
        "calendar_items": calendar_items,
    })))
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

/// Fetch planned/actual hours totals for an employee in a date range.
async fn fetch_employee_month_hours(
    pool: &sqlx::PgPool,
    employee_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(Option<f64>, Option<f64>), ApiError> {
    let sums = employee_repo::fetch_month_hours(pool, employee_id, from, to).await?;
    Ok((sums.planned, sums.actual))
}

/// Fetch a single employee as JSON.
async fn fetch_employee_json(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<serde_json::Value, ApiError> {
    let row = employee_repo::fetch_by_id(pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    Ok(serde_json::json!({
        "id": row.id,
        "salutation": row.salutation,
        "first_name": row.first_name,
        "last_name": row.last_name,
        "email": row.email,
        "phone": row.phone,
        "monthly_hours_target": row.monthly_hours_target,
        "active": row.active,
        "arbeitsvertrag_key": row.arbeitsvertrag_key,
        "mitarbeiterfragebogen_key": row.mitarbeiterfragebogen_key,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    }))
}

// --- Employee Documents ---

/// Validate and return the DB column name for a document type path segment.
///
/// **Caller**: upload/download/delete employee document handlers
/// **Why**: Centralises the allow-list so that only valid document types reach the DB.
fn resolve_doc_column(doc_type: &str) -> Option<&'static str> {
    match doc_type {
        "arbeitsvertrag" => Some("arbeitsvertrag_key"),
        "mitarbeiterfragebogen" => Some("mitarbeiterfragebogen_key"),
        _ => None,
    }
}

/// Derive a best-effort MIME type from an S3 key's file extension.
fn doc_content_type(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "pdf"  => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc"  => "application/msword",
        "png"  => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _      => "application/octet-stream",
    }
}

/// `POST /api/v1/admin/employees/{id}/documents/{doc_type}` — Upload an employee document.
///
/// **Caller**: Admin employee detail page document card.
/// **Why**: Stores Arbeitsvertrag or Mitarbeiterfragebogen in S3 and saves the key in the DB.
///
/// # Parameters
/// - `id`       — Employee UUID
/// - `doc_type` — `"arbeitsvertrag"` or `"mitarbeiterfragebogen"`
///
/// Expects a `multipart/form-data` body with a single `"file"` part.
///
/// # Returns
/// `200 OK` with updated employee JSON on success.
async fn upload_employee_document(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, doc_type)): Path<(Uuid, String)>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let col = resolve_doc_column(&doc_type)
        .ok_or_else(|| ApiError::BadRequest("Unbekannter Dokumenttyp".into()))?;

    // Verify employee exists
    if !employee_repo::exists(&state.db, id).await? {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into()));
    }

    // Extract the "file" part from the multipart body
    let mut file_bytes: Option<Bytes> = None;
    let mut file_ext = String::from("pdf");
    let mut content_type_str = String::from("application/pdf");

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::BadRequest(format!("Fehler beim Lesen der Datei: {e}"))
    })? {
        if field.name() == Some("file") {
            // Derive extension from original filename
            if let Some(fname) = field.file_name() {
                if let Some(ext) = fname.rsplit('.').next().filter(|e| !e.is_empty()) {
                    file_ext = ext.to_lowercase();
                }
            }
            if let Some(ct) = field.content_type() {
                content_type_str = ct.to_string();
            }
            file_bytes = Some(
                field.bytes().await.map_err(|e| {
                    ApiError::BadRequest(format!("Fehler beim Lesen der Dateidaten: {e}"))
                })?
            );
            break;
        }
    }

    let data = file_bytes.ok_or_else(|| ApiError::BadRequest("Kein Dateifeld gefunden".into()))?;

    // Upload to S3
    let key = format!("employees/{}/{}.{}", id, doc_type, file_ext);
    state.storage.upload(&key, data, &content_type_str).await.map_err(|e| {
        tracing::error!("S3 upload error for employee document: {e}");
        ApiError::Internal("Datei-Upload fehlgeschlagen".into())
    })?;

    // Persist key in DB (safe: col is from the allow-list above, not user input)
    employee_repo::set_document_key(&state.db, id, col, &key).await?;

    tracing::info!("Employee {id}: uploaded {doc_type} → {key}");
    let employee = fetch_employee_json(&state.db, id).await?;
    Ok(Json(employee))
}

/// `GET /api/v1/admin/employees/{id}/documents/{doc_type}` — Download an employee document.
///
/// **Caller**: Admin employee detail page document card download button.
/// **Why**: Proxies the S3 object through the API so the JWT-protected endpoint can gate access.
///
/// # Parameters
/// - `id`       — Employee UUID
/// - `doc_type` — `"arbeitsvertrag"` or `"mitarbeiterfragebogen"`
///
/// # Returns
/// Raw file bytes with appropriate `Content-Type` and `Content-Disposition: attachment` header.
async fn download_employee_document(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, doc_type)): Path<(Uuid, String)>,
) -> Result<Response, ApiError> {
    resolve_doc_column(&doc_type)
        .ok_or_else(|| ApiError::BadRequest("Unbekannter Dokumenttyp".into()))?;

    // Fetch the stored S3 key
    let col = &format!("{}_key", doc_type.replace('-', "_"));
    let key = employee_repo::fetch_document_key(&state.db, id, col)
        .await?
        .ok_or_else(|| ApiError::NotFound("Dokument nicht vorhanden".into()))?;

    let data = state.storage.download(&key).await.map_err(|e| {
        tracing::error!("S3 download error for employee document: {e}");
        ApiError::NotFound("Dokument nicht abrufbar".into())
    })?;

    let ct = doc_content_type(&key);
    let filename = key.rsplit('/').next().unwrap_or("document");

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(axum::body::Body::from(data))
        .unwrap())
}

/// `DELETE /api/v1/admin/employees/{id}/documents/{doc_type}` — Remove an employee document.
///
/// **Caller**: Admin employee detail page document card delete button.
/// **Why**: Deletes the file from S3 and clears the DB key so the slot appears empty again.
///
/// # Parameters
/// - `id`       — Employee UUID
/// - `doc_type` — `"arbeitsvertrag"` or `"mitarbeiterfragebogen"`
///
/// # Returns
/// `200 OK` with updated employee JSON.
async fn delete_employee_document(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, doc_type)): Path<(Uuid, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let col = resolve_doc_column(&doc_type)
        .ok_or_else(|| ApiError::BadRequest("Unbekannter Dokumenttyp".into()))?;

    // Fetch stored key
    let key = employee_repo::fetch_document_key(&state.db, id, col).await?;

    if let Some(ref k) = key {
        // Best-effort S3 delete — log but don't fail if the object is already gone
        if let Err(e) = state.storage.delete(k).await {
            tracing::warn!("S3 delete for employee document {k} failed (ignoring): {e}");
        }
    }

    employee_repo::clear_document_key(&state.db, id, col).await?;

    tracing::info!("Employee {id}: deleted {doc_type}");
    let employee = fetch_employee_json(&state.db, id).await?;
    Ok(Json(employee))
}

// --- Notes (Notepad) ---

#[derive(Debug, Serialize)]
struct NoteRow {
    id: Uuid,
    title: String,
    content: String,
    color: String,
    pinned: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// `GET /api/v1/admin/notes` — List all notes, pinned first, then by most recent.
///
/// **Caller**: Admin notepad panel.
/// **Why**: Returns all notes for the floating notepad widget.
///
/// # Returns
/// `200 OK` with `{ notes: [...] }`.
async fn list_notes(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repo_notes = admin_repo::list_notes(&state.db).await?;
    let notes: Vec<NoteRow> = repo_notes
        .into_iter()
        .map(|n| NoteRow {
            id: n.id, title: n.title, content: n.content, color: n.color,
            pinned: n.pinned, created_at: n.created_at, updated_at: n.updated_at,
        })
        .collect();

    Ok(Json(serde_json::json!({ "notes": notes })))
}

/// `POST /api/v1/admin/notes` — Create a new note.
///
/// **Caller**: Admin notepad panel "new note" action.
/// **Why**: Persists a freeform note created by an admin user.
///
/// # Returns
/// `201 Created` with the new `NoteRow` JSON.
async fn create_note(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(body): Json<aust_core::models::CreateNote>,
) -> Result<(axum::http::StatusCode, Json<NoteRow>), ApiError> {
    let id = uuid::Uuid::now_v7();
    let title = body.title.unwrap_or_default();
    let content = body.content.unwrap_or_default();
    let color = body.color.unwrap_or_else(|| "default".into());
    let pinned = body.pinned.unwrap_or(false);

    let repo_note = admin_repo::create_note(&state.db, id, &title, &content, &color, pinned).await?;
    let note = NoteRow {
        id: repo_note.id, title: repo_note.title, content: repo_note.content,
        color: repo_note.color, pinned: repo_note.pinned,
        created_at: repo_note.created_at, updated_at: repo_note.updated_at,
    };

    Ok((axum::http::StatusCode::CREATED, Json(note)))
}

/// `PATCH /api/v1/admin/notes/{id}` — Update an existing note.
///
/// **Caller**: Admin notepad panel inline editing.
/// **Why**: Saves changes to note title, content, color, or pin state.
///
/// # Returns
/// `200 OK` with the updated `NoteRow` JSON.
async fn update_note(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<aust_core::models::UpdateNote>,
) -> Result<Json<NoteRow>, ApiError> {
    let repo_note = admin_repo::update_note(
        &state.db, id,
        body.title.as_deref(), body.content.as_deref(), body.color.as_deref(), body.pinned,
    )
    .await?
    .ok_or_else(|| ApiError::NotFound("Notiz nicht gefunden.".into()))?;

    Ok(Json(NoteRow {
        id: repo_note.id, title: repo_note.title, content: repo_note.content,
        color: repo_note.color, pinned: repo_note.pinned,
        created_at: repo_note.created_at, updated_at: repo_note.updated_at,
    }))
}

/// `DELETE /api/v1/admin/notes/{id}` — Delete a note.
///
/// **Caller**: Admin notepad panel delete action.
/// **Why**: Permanently removes a note.
///
/// # Returns
/// `204 No Content` on success.
async fn delete_note(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    let rows = admin_repo::delete_note(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Notiz nicht gefunden.".into()));
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ─── Feedback Reports ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FeedbackListQuery {
    status: Option<String>,
    #[serde(rename = "type")]
    report_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PatchFeedbackBody {
    status: String,
}

/// `POST /api/v1/admin/feedback` — Create a new feedback report with optional file attachments.
///
/// **Caller**: Admin feedback form (multipart submit).
/// **Why**: Stores bug reports and feature requests submitted from the admin dashboard.
///
/// Accepts `multipart/form-data` with text fields `type`, `priority`, `title`,
/// `description`, `location` and any number of file fields named `attachments`.
/// Files are uploaded to S3 under `feedback/{tmp_id}/{index}.{ext}`.
///
/// # Returns
/// `201 Created` with the created `FeedbackReport` JSON.
async fn create_feedback(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    mut multipart: Multipart,
) -> Result<(axum::http::StatusCode, Json<feedback_repo::FeedbackReport>), ApiError> {
    let mut report_type = String::new();
    let mut priority = String::from("medium");
    let mut title = String::new();
    let mut description: Option<String> = None;
    let mut location: Option<String> = None;
    let mut attachment_keys: Vec<String> = Vec::new();

    // Temporary ID for S3 key prefix (re-used as report ID in DB via DEFAULT gen_random_uuid).
    // We upload first, then insert — keys reference this prefix permanently.
    let tmp_id = Uuid::now_v7();
    let mut file_index: usize = 0;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::BadRequest(format!("Multipart read error: {e}"))
    })? {
        match field.name() {
            Some("type") => {
                report_type = field.text().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
            }
            Some("priority") => {
                priority = field.text().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
            }
            Some("title") => {
                title = field.text().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
            }
            Some("description") => {
                let v = field.text().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
                if !v.is_empty() { description = Some(v); }
            }
            Some("location") => {
                let v = field.text().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
                if !v.is_empty() { location = Some(v); }
            }
            Some("attachments") => {
                let ext = field
                    .file_name()
                    .and_then(|n| n.rsplit('.').next())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_else(|| "bin".into());
                let ct = field
                    .content_type()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "application/octet-stream".into());
                let data = field.bytes().await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
                if !data.is_empty() {
                    let key = format!("feedback/{tmp_id}/{file_index}.{ext}");
                    state.storage.upload(&key, data, &ct).await.map_err(|e| {
                        tracing::error!("S3 upload for feedback attachment: {e}");
                        ApiError::Internal("Datei-Upload fehlgeschlagen.".into())
                    })?;
                    attachment_keys.push(key);
                    file_index += 1;
                }
            }
            _ => {}
        }
    }

    if title.is_empty() {
        return Err(ApiError::BadRequest("Titel darf nicht leer sein.".into()));
    }
    if report_type != "bug" && report_type != "feature" {
        return Err(ApiError::BadRequest("Ungültiger Typ (bug oder feature).".into()));
    }

    let report = feedback_repo::create_report(
        &state.db,
        &report_type,
        &priority,
        &title,
        description.as_deref(),
        location.as_deref(),
        &attachment_keys,
    )
    .await?;

    Ok((axum::http::StatusCode::CREATED, Json(report)))
}

/// `GET /api/v1/admin/feedback` — List all feedback reports (admin only), newest first.
///
/// **Caller**: Admin `/admin/reports` page on load.
/// **Why**: Provides the admin with a complete, filterable list of submitted reports.
///
/// # Query Parameters
/// - `status` — filter by status ("open", "in_progress", "resolved")
/// - `type`   — filter by type ("bug", "feature")
///
/// # Returns
/// JSON array of `FeedbackReport`.
async fn list_feedback(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<FeedbackListQuery>,
) -> Result<Json<Vec<feedback_repo::FeedbackReport>>, ApiError> {
    require_admin(&claims)?;
    let reports = feedback_repo::list_reports(
        &state.db,
        q.status.as_deref(),
        q.report_type.as_deref(),
    )
    .await?;
    Ok(Json(reports))
}

/// `GET /api/v1/admin/feedback/{id}` — Get a single feedback report by ID (admin only).
///
/// **Caller**: Admin `/admin/reports` detail panel.
/// **Why**: Returns the full report including attachment keys for the download view.
///
/// # Returns
/// `200 OK` with `FeedbackReport` JSON, or `404` if not found.
async fn get_feedback(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<feedback_repo::FeedbackReport>, ApiError> {
    require_admin(&claims)?;
    let report = feedback_repo::get_report(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Report nicht gefunden.".into()))?;
    Ok(Json(report))
}

/// `PATCH /api/v1/admin/feedback/{id}` — Update the status of a feedback report (admin only).
///
/// **Caller**: Admin `/admin/reports` status dropdown.
/// **Why**: Lets the admin track progress: open → in_progress → resolved.
///
/// # Body
/// `{ "status": "in_progress" }`
///
/// # Returns
/// `200 OK` with updated `FeedbackReport`.
async fn patch_feedback(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchFeedbackBody>,
) -> Result<Json<feedback_repo::FeedbackReport>, ApiError> {
    require_admin(&claims)?;
    let valid = ["open", "in_progress", "resolved"];
    if !valid.contains(&body.status.as_str()) {
        return Err(ApiError::BadRequest("Ungültiger Status.".into()));
    }
    let report = feedback_repo::update_status(&state.db, id, &body.status).await?;
    Ok(Json(report))
}

/// `GET /api/v1/admin/feedback/{id}/attachments/{idx}` — Download one attachment by index (admin only).
///
/// **Caller**: Admin reports detail view download button.
/// **Why**: Proxies the attachment from S3 with the correct content-disposition header.
///
/// # Path Parameters
/// - `id`  — report UUID
/// - `idx` — zero-based attachment index
///
/// # Returns
/// Binary response with `Content-Disposition: attachment` header, or `404`.
async fn download_feedback_attachment(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path((id, idx)): Path<(Uuid, usize)>,
) -> Result<Response, ApiError> {
    require_admin(&claims)?;
    let report = feedback_repo::get_report(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Report nicht gefunden.".into()))?;

    let key = report.attachment_keys.get(idx)
        .ok_or_else(|| ApiError::NotFound("Anhang nicht gefunden.".into()))?;

    let data = state.storage.download(key).await.map_err(|e| {
        tracing::error!("S3 download for feedback attachment {key}: {e}");
        ApiError::NotFound("Anhang konnte nicht abgerufen werden.".into())
    })?;

    let filename = key.rsplit('/').next().unwrap_or("attachment");
    let ext = filename.rsplit('.').next().unwrap_or("bin");
    let ct = mime_from_ext(ext);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(data))
        .unwrap())
}

/// Map a file extension to a MIME type string for feedback attachments.
///
/// **Caller**: `download_feedback_attachment`
/// **Why**: Sets the Content-Type header so browsers handle downloads correctly.
///
/// # Parameters
/// - `ext` — lowercase file extension without dot
///
/// # Returns
/// MIME type string; falls back to `"application/octet-stream"` for unknowns.
fn mime_from_ext(ext: &str) -> &'static str {
    match ext {
        "png"  => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif"  => "image/gif",
        "webp" => "image/webp",
        "pdf"  => "application/pdf",
        "mp4"  => "video/mp4",
        _      => "application/octet-stream",
    }
}
