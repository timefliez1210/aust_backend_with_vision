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
use crate::repositories::{admin_repo, employee_repo};
use crate::{ApiError, AppState};

/// Register all admin-panel routes (protected under JWT middleware).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly, nested under the admin
/// JWT authentication middleware.
/// **Why**: Consolidates dashboard, customer, address, email, user, and order endpoints
/// into a single router mounted at `/api/v1/admin`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(dashboard))
        .route("/customers", get(list_customers).post(create_customer))
        .route("/customers/{id}", get(get_customer).patch(update_customer))
        .route("/customers/{id}/delete", post(delete_customer))
        .route("/addresses/{id}", patch(update_address))
        .route("/emails", get(list_email_threads))
        .route("/emails/{id}", get(get_email_thread))
        .route("/emails/messages/{id}", patch(update_draft_email))
        .route("/emails/messages/{id}/send", post(send_draft_email))
        .route("/emails/messages/{id}/discard", post(discard_draft_email))
        .route("/emails/{id}/reply", post(reply_to_thread))
        .route("/emails/compose", post(compose_email))
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

// --- Customers ---

#[derive(Debug, Deserialize)]
struct ListCustomersQuery {
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct CustomerListItem {
    id: Uuid,
    email: String,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct CustomerListResponse {
    customers: Vec<CustomerListItem>,
    total: i64,
}

/// `GET /api/v1/admin/customers` — List customers with optional full-text search.
///
/// **Caller**: Axum router / admin dashboard customers list page.
/// **Why**: Paginated, ILIKE-searchable customer listing for the admin panel.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `search` (matched against name and email), `limit`, `offset`
///
/// # Returns
/// `200 OK` with `CustomerListResponse` containing `customers` array and `total` count.
async fn list_customers(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListCustomersQuery>,
) -> Result<Json<CustomerListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    let repo_customers = admin_repo::list_customers(&state.db, &search, limit, offset).await?;
    let customers: Vec<CustomerListItem> = repo_customers
        .into_iter()
        .map(|c| CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone, created_at: c.created_at,
        })
        .collect();

    let total = admin_repo::count_customers(&state.db, &search).await?;

    Ok(Json(CustomerListResponse { customers, total }))
}

#[derive(Debug, Serialize)]
struct CustomerDetailResponse {
    id: Uuid,
    email: String,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    created_at: DateTime<Utc>,
    quotes: Vec<CustomerQuote>,
    offers: Vec<CustomerOffer>,
    termine: Vec<CustomerTermin>,
}

#[derive(Debug, Serialize)]
struct CustomerQuote {
    id: Uuid,
    status: String,
    estimated_volume_m3: Option<f64>,
    preferred_date: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct CustomerOffer {
    id: Uuid,
    inquiry_id: Uuid,
    price_cents: i64,
    status: String,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct CustomerTermin {
    id: Uuid,
    title: String,
    category: String,
    scheduled_date: Option<chrono::NaiveDate>,
    status: String,
}

/// `GET /api/v1/admin/customers/{id}` — Retrieve a customer with their quotes and offers.
///
/// **Caller**: Axum router / admin dashboard customer detail page.
/// **Why**: Returns customer contact info plus all associated inquiries and offers,
/// ordered newest-first, for the admin CRM view.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
///
/// # Returns
/// `200 OK` with `CustomerDetailResponse`.
///
/// # Errors
/// - `404` if customer not found
async fn get_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<CustomerDetailResponse>, ApiError> {
    let repo_customer = admin_repo::fetch_customer(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))?;

    let repo_quotes = admin_repo::fetch_customer_quotes(&state.db, id).await?;
    let quotes: Vec<CustomerQuote> = repo_quotes
        .into_iter()
        .map(|q| CustomerQuote {
            id: q.id, status: q.status, estimated_volume_m3: q.estimated_volume_m3,
            preferred_date: q.preferred_date, created_at: q.created_at,
        })
        .collect();

    let repo_offers = admin_repo::fetch_customer_offers(&state.db, id).await?;
    let offers: Vec<CustomerOffer> = repo_offers
        .into_iter()
        .map(|o| CustomerOffer {
            id: o.id, inquiry_id: o.inquiry_id, price_cents: o.price_cents,
            status: o.status, created_at: o.created_at, sent_at: o.sent_at,
        })
        .collect();

    let repo_termine = admin_repo::fetch_customer_termine(&state.db, id).await?;
    let termine: Vec<CustomerTermin> = repo_termine
        .into_iter()
        .map(|t| CustomerTermin {
            id: t.id, title: t.title, category: t.category,
            scheduled_date: t.scheduled_date, status: t.status,
        })
        .collect();

    Ok(Json(CustomerDetailResponse {
        id: repo_customer.id,
        email: repo_customer.email,
        name: repo_customer.name,
        salutation: repo_customer.salutation,
        first_name: repo_customer.first_name,
        last_name: repo_customer.last_name,
        phone: repo_customer.phone,
        created_at: repo_customer.created_at,
        quotes,
        offers,
        termine,
    }))
}

#[derive(Debug, Deserialize)]
struct UpdateCustomerRequest {
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    email: Option<String>,
}

/// `PATCH /api/v1/admin/customers/{id}` — Partially update a customer's contact fields.
///
/// **Caller**: Axum router / admin dashboard customer edit form.
/// **Why**: Allows correcting a customer's name, phone, or email without touching other
/// fields (COALESCE-based partial update).
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
/// - `request` — optional `name`, `phone`, `email` fields
///
/// # Returns
/// `200 OK` with updated `CustomerListItem`.
///
/// # Errors
/// - `404` if customer not found
async fn update_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateCustomerRequest>,
) -> Result<Json<CustomerListItem>, ApiError> {
    let repo_row = admin_repo::update_customer(
        &state.db, id,
        request.name.as_deref(), request.salutation.as_deref(),
        request.first_name.as_deref(), request.last_name.as_deref(),
        request.phone.as_deref(), request.email.as_deref(),
    )
    .await?;

    repo_row
        .map(|c| Json(CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone, created_at: c.created_at,
        }))
        .ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))
}

// --- Create Customer ---

#[derive(Debug, Deserialize)]
struct CreateCustomerRequest {
    email: String,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
}

/// `POST /api/v1/admin/customers` — Create a new customer record.
///
/// **Caller**: Axum router / admin dashboard "Neuer Kunde" form.
/// **Why**: Allows manually creating a customer before creating an inquiry for walk-in or
/// phone inquiries that bypass the email pipeline.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `request` — JSON body with `email` (required), optional `name` and `phone`
///
/// # Returns
/// `201 Created` with the new `CustomerListItem` JSON.
///
/// # Errors
/// - `400` if a customer with the same email already exists
/// - `500` on other DB failures
async fn create_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(request): Json<CreateCustomerRequest>,
) -> Result<(axum::http::StatusCode, Json<CustomerListItem>), ApiError> {
    let id = Uuid::now_v7();
    let now = Utc::now();

    let repo_row = admin_repo::create_customer(
        &state.db, id, &request.email,
        request.name.as_deref(), request.salutation.as_deref(),
        request.first_name.as_deref(), request.last_name.as_deref(),
        request.phone.as_deref(), now,
    )
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("customers_email_key") {
                return ApiError::Validation("E-Mail-Adresse existiert bereits".into());
            }
        }
        ApiError::Database(e)
    })?;

    repo_row
        .map(|c| (axum::http::StatusCode::CREATED, Json(CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone, created_at: c.created_at,
        })))
        .ok_or_else(|| ApiError::Internal("Kunde konnte nicht erstellt werden".into()))
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

// --- Addresses ---

#[derive(Debug, Deserialize)]
struct UpdateAddressRequest {
    street: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AddressResponse {
    id: Uuid,
    street: String,
    city: String,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

/// `PATCH /api/v1/admin/addresses/{id}` — Partially update an address record.
///
/// **Caller**: Axum router / admin dashboard address edit form on the inquiry detail page.
/// **Why**: Allows correcting street, city, postal code, floor, or elevator without
/// creating a new address record. After editing, the admin typically uses the regenerate
/// endpoint to refresh the distance and regenerate the offer.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — address UUID path parameter
/// - `request` — partial update fields; omitted fields are unchanged
///
/// # Returns
/// `200 OK` with updated `AddressResponse`.
///
/// # Errors
/// - `404` if address not found
async fn update_address(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateAddressRequest>,
) -> Result<Json<AddressResponse>, ApiError> {
    let repo_row = admin_repo::update_address(
        &state.db, id,
        request.street.as_deref(), request.city.as_deref(),
        request.postal_code.as_deref(), request.floor.as_deref(),
        request.elevator,
    )
    .await?;

    repo_row
        .map(|a| Json(AddressResponse {
            id: a.id, street: a.street, city: a.city,
            postal_code: a.postal_code, floor: a.floor, elevator: a.elevator,
        }))
        .ok_or_else(|| ApiError::NotFound(format!("Adresse {id} nicht gefunden")))
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

// --- Delete Customer ---

/// `POST /api/v1/admin/customers/{id}/delete` — Hard-delete a customer and all their data.
///
/// **Caller**: Axum router / admin dashboard customer delete action.
/// **Why**: Cascades via FK to inquiries, offers, volume_estimations, email_threads, and
/// email_messages. Use for GDPR erasure requests. S3 objects (PDFs, images) are orphaned
/// and must be cleaned separately.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if customer not found
async fn delete_customer(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
    // Cascades: inquiries, offers, volume_estimations, email_threads, email_messages
    let rows = admin_repo::delete_customer(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(format!("Kunde {id} nicht gefunden")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Email Threads ---

#[derive(Debug, Deserialize)]
struct ListEmailThreadsQuery {
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct EmailThreadListItem {
    id: Uuid,
    customer_id: Uuid,
    customer_email: String,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    message_count: i64,
    last_message_at: Option<DateTime<Utc>>,
    last_direction: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct EmailThreadListResponse {
    threads: Vec<EmailThreadListItem>,
    total: i64,
}

/// `GET /api/v1/admin/emails` — List email threads with customer info and last-message metadata.
///
/// **Caller**: Axum router / admin dashboard "E-Mails" tab.
/// **Why**: Provides an inbox-style view of all email threads: customer name/email,
/// message count, last message direction, and timestamp. Supports full-text search on
/// customer name, email, and thread subject.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `search`, `limit`, `offset`
///
/// # Returns
/// `200 OK` with `EmailThreadListResponse` containing `threads` and `total`.
async fn list_email_threads(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListEmailThreadsQuery>,
) -> Result<Json<EmailThreadListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    let repo_threads = admin_repo::list_email_threads(&state.db, &search, limit, offset).await?;
    let threads: Vec<EmailThreadListItem> = repo_threads
        .into_iter()
        .map(|t| EmailThreadListItem {
            id: t.id, customer_id: t.customer_id, customer_email: t.customer_email,
            customer_name: t.customer_name, inquiry_id: t.inquiry_id, subject: t.subject,
            message_count: t.message_count, last_message_at: t.last_message_at,
            last_direction: t.last_direction, created_at: t.created_at,
        })
        .collect();

    let total = admin_repo::count_email_threads(&state.db, &search).await?;

    Ok(Json(EmailThreadListResponse { threads, total }))
}

#[derive(Debug, Serialize)]
struct EmailThreadDetailResponse {
    thread: EmailThreadDetail,
    messages: Vec<EmailMessageItem>,
}

#[derive(Debug, Serialize)]
struct EmailThreadDetail {
    id: Uuid,
    customer_id: Uuid,
    customer_email: String,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct EmailMessageItem {
    id: Uuid,
    direction: String,
    from_address: String,
    to_address: String,
    subject: Option<String>,
    body_text: Option<String>,
    llm_generated: bool,
    status: String,
    created_at: DateTime<Utc>,
}

/// `GET /api/v1/admin/emails/{id}` — Return an email thread with all its messages.
///
/// **Caller**: Axum router / admin dashboard email thread detail page.
/// **Why**: Returns the thread header and all non-discarded messages in chronological order.
/// Draft messages are included so the admin can review before sending.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — thread UUID path parameter
///
/// # Returns
/// `200 OK` with `EmailThreadDetailResponse` (thread + messages array).
///
/// # Errors
/// - `404` if thread not found
async fn get_email_thread(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<EmailThreadDetailResponse>, ApiError> {
    let repo_thread = admin_repo::fetch_email_thread(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("E-Mail-Thread {id} nicht gefunden")))?;

    let thread = EmailThreadDetail {
        id: repo_thread.id, customer_id: repo_thread.customer_id,
        customer_email: repo_thread.customer_email, customer_name: repo_thread.customer_name,
        inquiry_id: repo_thread.inquiry_id, subject: repo_thread.subject, created_at: repo_thread.created_at,
    };

    let repo_messages = admin_repo::fetch_thread_messages(&state.db, id).await?;
    let messages: Vec<EmailMessageItem> = repo_messages
        .into_iter()
        .map(|m| EmailMessageItem {
            id: m.id, direction: m.direction, from_address: m.from_address,
            to_address: m.to_address, subject: m.subject, body_text: m.body_text,
            llm_generated: m.llm_generated, status: m.status, created_at: m.created_at,
        })
        .collect();

    Ok(Json(EmailThreadDetailResponse { thread, messages }))
}

/// `POST /api/v1/admin/emails/messages/{id}/send` — Send a draft email via SMTP.
///
/// **Caller**: Axum router / admin dashboard "Senden" button in the email thread view.
/// **Why**: Fetches the draft message body and the customer's real email (via the thread →
/// customer join), sends via SMTP, and marks the message as `sent`. The `to_address` is
/// corrected to the real customer email (overriding whatever placeholder was stored).
///
/// # Parameters
/// - `state` — shared AppState (DB pool, SMTP config)
/// - `id` — email_message UUID path parameter (must have `status = 'draft'`)
///
/// # Returns
/// `200 OK` with `{"message": "E-Mail an <email> gesendet"}`.
///
/// # Errors
/// - `404` if the draft message does not exist or is not in draft status
/// - `500` on SMTP failures
async fn send_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Fetch draft + customer email + optional offer PDF key (when thread belongs to an inquiry with an active offer)
    let row = admin_repo::fetch_draft_for_send(&state.db, id).await?;

    let (subject, body_text, customer_email, pdf_key, offer_id, inquiry_id) =
        row.ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;

    let subject = subject.unwrap_or_else(|| "Ihr Umzugsangebot — AUST Umzüge".into());
    let body = body_text.unwrap_or_default();

    // If the thread is tied to an inquiry with a PDF offer, send with attachment
    if let (Some(key), Some(oid), Some(iid)) = (&pdf_key, offer_id, inquiry_id) {
        use crate::services::email::{build_email_with_attachment, send_email};

        let pdf_bytes = state
            .storage
            .download(key)
            .await
            .map_err(|e| ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")))?;

        let email_cfg = &state.config.email;
        let message = build_email_with_attachment(
            &email_cfg.from_address,
            &email_cfg.from_name,
            &customer_email,
            &subject,
            &body,
            &pdf_bytes,
            &format!("Angebot-{oid}.pdf"),
            "application/pdf",
        )
        .map_err(|e| ApiError::Internal(format!("E-Mail-Aufbau fehlgeschlagen: {e}")))?;

        send_email(
            &email_cfg.smtp_host,
            email_cfg.smtp_port,
            &email_cfg.username,
            &email_cfg.password,
            message,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

        let now = chrono::Utc::now();

        // Update offer and inquiry status
        admin_repo::mark_offer_sent(&state.db, oid, now).await?;
        admin_repo::mark_inquiry_offer_sent(&state.db, iid, now).await?;
    } else {
        // Plain email — no offer PDF attached (e.g. general inquiry reply)
        send_plain_email(&state.config.email, &customer_email, &subject, &body)
            .await
            .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;
    }

    // Mark draft as sent + fix to_address
    admin_repo::mark_message_sent(&state.db, id, &customer_email).await?;

    Ok(Json(serde_json::json!({
        "message": format!("E-Mail an {customer_email} gesendet"),
    })))
}

/// `POST /api/v1/admin/emails/messages/{id}/discard` — Discard a draft email.
///
/// **Caller**: Axum router / admin dashboard "Verwerfen" button in the email thread view.
/// **Why**: Sets `email_messages.status = 'discarded'` so the draft is excluded from the
/// thread view without being physically deleted. Prevents accidental sends of stale drafts.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — email_message UUID path parameter (must have `status = 'draft'`)
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if draft not found or already processed
async fn discard_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = admin_repo::discard_draft(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Entwurf nicht gefunden oder bereits verarbeitet".into()));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Edit Draft Content ---

#[derive(Debug, Deserialize)]
struct UpdateDraftRequest {
    subject: Option<String>,
    body_text: Option<String>,
}

/// `PATCH /api/v1/admin/emails/messages/{id}` — Edit the subject or body of a draft email.
///
/// **Caller**: Axum router / admin dashboard email draft editor.
/// **Why**: Allows Alex to tweak the LLM-generated draft before sending. Only drafts can
/// be edited (status check via `WHERE status = 'draft'`).
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — email_message UUID path parameter
/// - `request` — optional `subject` and/or `body_text` fields to overwrite
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if draft not found or already sent/discarded
async fn update_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateDraftRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = admin_repo::update_draft(&state.db, id, request.subject.as_deref(), request.body_text.as_deref()).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(
            "Entwurf nicht gefunden oder bereits gesendet".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Reply to Thread ---

#[derive(Debug, Deserialize)]
struct ReplyRequest {
    subject: Option<String>,
    body_text: String,
}

/// `POST /api/v1/admin/emails/{id}/reply` — Create a new draft reply in an existing thread.
///
/// **Caller**: Axum router / admin dashboard thread reply composer.
/// **Why**: Inserts a new outbound `email_messages` row in `draft` status tied to the
/// existing thread, without sending it immediately. The admin then uses `send_draft_email`
/// to approve and send.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config for `from_address`)
/// - `thread_id` — thread UUID path parameter
/// - `request` — `body_text` (required) and optional `subject` override
///
/// # Returns
/// `201 Created` with `{"id": ..., "status": "draft"}`.
///
/// # Errors
/// - `404` if thread not found
async fn reply_to_thread(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(thread_id): Path<Uuid>,
    Json(request): Json<ReplyRequest>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), ApiError> {
    let row = admin_repo::fetch_thread_for_reply(&state.db, thread_id).await?;
    let (_customer_id, customer_email, thread_subject) = row.ok_or_else(|| {
        ApiError::NotFound(format!("E-Mail-Thread {thread_id} nicht gefunden"))
    })?;

    let subject = request.subject.or(thread_subject);
    let from_address = &state.config.email.from_address;
    let id = Uuid::now_v7();
    let now = Utc::now();

    admin_repo::insert_reply_draft(
        &state.db, id, thread_id, from_address, &customer_email,
        subject.as_deref(), &request.body_text, now,
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "status": "draft",
        })),
    ))
}

// --- Compose New Email ---

#[derive(Debug, Deserialize)]
struct ComposeEmailRequest {
    customer_email: String,
    subject: String,
    body_text: String,
}

/// `POST /api/v1/admin/emails/compose` — Compose a new outbound email to any address.
///
/// **Caller**: Axum router / admin dashboard "Neue E-Mail" compose button.
/// **Why**: Creates a new thread (upserts the customer by email) and a draft message in
/// one operation, allowing the admin to initiate contact with a customer not yet in the
/// system. The draft is saved and can be reviewed before sending via `send_draft_email`.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config for `from_address`)
/// - `request` — `customer_email`, `subject`, `body_text` (all required)
///
/// # Returns
/// `201 Created` with `{"thread_id": ..., "message_id": ...}`.
async fn compose_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(request): Json<ComposeEmailRequest>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), ApiError> {
    let now = Utc::now();

    // Upsert customer by email
    let customer_id = admin_repo::upsert_customer_for_compose(&state.db, &request.customer_email, now).await?;

    // Create thread
    let thread_id = Uuid::now_v7();
    admin_repo::create_compose_thread(&state.db, thread_id, customer_id, &request.subject, now).await?;

    // Create draft message
    let message_id = Uuid::now_v7();
    let from_address = &state.config.email.from_address;
    admin_repo::insert_compose_draft(
        &state.db, message_id, thread_id, from_address,
        &request.customer_email, &request.subject, &request.body_text, now,
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "thread_id": thread_id,
            "message_id": message_id,
        })),
    ))
}

/// Send a plain-text email via SMTP using the configured outbound email credentials.
///
/// **Caller**: `send_draft_email` — the only SMTP send path in the admin module.
/// **Why**: Thin wrapper around `services::email::{build_plain_email, send_email}` so the
/// SMTP credentials from `Config.email` stay out of individual route handlers.
///
/// # Parameters
/// - `email_config` — SMTP host/port/credentials and from_address/from_name
/// - `to` — recipient email address
/// - `subject` — email subject line
/// - `body` — plain-text body
///
/// # Errors
/// Returns `Err(String)` describing the failure if building the message or the SMTP
/// transmission fails.
async fn send_plain_email(
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

// --- Permission helpers ---

/// Guard that allows only Admin-role users to proceed.
///
/// **Caller**: Sensitive admin handlers (deletes, user management).
/// **Why**: Buerokraft users share the admin JWT middleware but should not be able to
///          delete records or access user management.
fn require_admin(claims: &TokenClaims) -> Result<(), ApiError> {
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

/// `GET /api/v1/admin/employees/{id}/hours?month=YYYY-MM` — Monthly hours summary.
///
/// **Caller**: Admin employee detail page hours card.
/// **Why**: Aggregates planned/actual hours for a given month with per-assignment breakdown.
///
/// # Returns
/// `200 OK` with target, planned, actual totals and assignment breakdown.
async fn employee_hours_summary(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let month_str = query.get("month").cloned().unwrap_or_else(|| {
        Utc::now().format("%Y-%m").to_string()
    });

    let (from_date, to_date) = parse_month_range(&month_str)
        .ok_or_else(|| ApiError::BadRequest("Ungueltiges Monatsformat. Erwartet: YYYY-MM".into()))?;

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
        "month": month_str,
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
