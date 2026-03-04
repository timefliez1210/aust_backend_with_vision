use axum::{
    extract::{Path, Query, State},
    routing::{get, patch, post},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
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

#[derive(Debug, Serialize, FromRow)]
struct ActivityItem {
    #[serde(rename = "type")]
    activity_type: String,
    description: String,
    created_at: DateTime<Utc>,
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
    let (open_quotes,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM inquiries WHERE status IN ('pending', 'info_requested', 'estimated')",
    )
    .fetch_one(&state.db)
    .await?;

    let (pending_offers,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM offers WHERE status = 'draft'")
            .fetch_one(&state.db)
            .await?;

    let today = Utc::now().date_naive();
    let (todays_bookings,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM calendar_bookings WHERE booking_date = $1 AND status != 'cancelled'",
    )
    .bind(today)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(Some((0,)))
    .unwrap_or((0,));

    let (total_customers,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM customers")
            .fetch_one(&state.db)
            .await?;

    let recent_offers: Vec<ActivityItem> = sqlx::query_as(
        r#"
        SELECT
            'offer_' || o.status AS activity_type,
            COALESCE(c.name, c.email) || ' — ' || (o.price_cents::float / 100)::text || ' €' AS description,
            o.created_at
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        JOIN customers c ON q.customer_id = c.id
        ORDER BY o.created_at DESC
        LIMIT 10
        "#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Find dates in the next 30 days where bookings >= capacity
    let from_date = today;
    let to_date = today + chrono::Days::new(30);
    let default_capacity = state.config.calendar.default_capacity;

    #[derive(FromRow)]
    struct ConflictRow {
        booking_date: NaiveDate,
        booking_count: i64,
    }

    let conflict_rows: Vec<ConflictRow> = sqlx::query_as(
        r#"
        SELECT booking_date, COUNT(*) AS booking_count
        FROM calendar_bookings
        WHERE booking_date BETWEEN $1 AND $2
          AND status != 'cancelled'
        GROUP BY booking_date
        HAVING COUNT(*) > COALESCE(
            (SELECT capacity FROM calendar_capacity_overrides WHERE override_date = booking_date),
            $3
        )
        ORDER BY booking_date
        "#,
    )
    .bind(from_date)
    .bind(to_date)
    .bind(default_capacity)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut conflict_dates = Vec::new();
    for row in conflict_rows {
        // Fetch actual capacity for this date
        let cap: Option<(i32,)> = sqlx::query_as(
            "SELECT capacity FROM calendar_capacity_overrides WHERE override_date = $1",
        )
        .bind(row.booking_date)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

        conflict_dates.push(ConflictDate {
            date: row.booking_date,
            booked: row.booking_count,
            capacity: cap.map(|c| c.0).unwrap_or(default_capacity),
        });
    }

    Ok(Json(DashboardResponse {
        open_quotes,
        pending_offers,
        todays_bookings,
        total_customers,
        recent_activity: recent_offers,
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

#[derive(Debug, Serialize, FromRow)]
struct CustomerListItem {
    id: Uuid,
    email: String,
    name: Option<String>,
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

    let customers: Vec<CustomerListItem> = sqlx::query_as(
        r#"
        SELECT id, email, name, phone, created_at
        FROM customers
        WHERE name ILIKE $1 OR email ILIKE $1
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM customers WHERE name ILIKE $1 OR email ILIKE $1",
    )
    .bind(&search)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(CustomerListResponse { customers, total }))
}

#[derive(Debug, Serialize)]
struct CustomerDetailResponse {
    id: Uuid,
    email: String,
    name: Option<String>,
    phone: Option<String>,
    created_at: DateTime<Utc>,
    quotes: Vec<CustomerQuote>,
    offers: Vec<CustomerOffer>,
}

#[derive(Debug, Serialize, FromRow)]
struct CustomerQuote {
    id: Uuid,
    status: String,
    estimated_volume_m3: Option<f64>,
    preferred_date: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
struct CustomerOffer {
    id: Uuid,
    inquiry_id: Uuid,
    price_cents: i64,
    status: String,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
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
    let customer: Option<CustomerListItem> = sqlx::query_as(
        "SELECT id, email, name, phone, created_at FROM customers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let customer =
        customer.ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))?;

    let quotes: Vec<CustomerQuote> = sqlx::query_as(
        r#"
        SELECT id, status, estimated_volume_m3, preferred_date, created_at
        FROM inquiries WHERE customer_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let offers: Vec<CustomerOffer> = sqlx::query_as(
        r#"
        SELECT o.id, o.inquiry_id, o.price_cents, o.status, o.created_at, o.sent_at
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        WHERE q.customer_id = $1
        ORDER BY o.created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(CustomerDetailResponse {
        id: customer.id,
        email: customer.email,
        name: customer.name,
        phone: customer.phone,
        created_at: customer.created_at,
        quotes,
        offers,
    }))
}

#[derive(Debug, Deserialize)]
struct UpdateCustomerRequest {
    name: Option<String>,
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
    let row: Option<CustomerListItem> = sqlx::query_as(
        r#"
        UPDATE customers SET
            name = COALESCE($2, name),
            phone = COALESCE($3, phone),
            email = COALESCE($4, email)
        WHERE id = $1
        RETURNING id, email, name, phone, created_at
        "#,
    )
    .bind(id)
    .bind(&request.name)
    .bind(&request.phone)
    .bind(&request.email)
    .fetch_optional(&state.db)
    .await?;

    row.ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))
        .map(Json)
}

// --- Create Customer ---

#[derive(Debug, Deserialize)]
struct CreateCustomerRequest {
    email: String,
    name: Option<String>,
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

    let row: Option<CustomerListItem> = sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        RETURNING id, email, name, phone, created_at
        "#,
    )
    .bind(id)
    .bind(&request.email)
    .bind(&request.name)
    .bind(&request.phone)
    .bind(now)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("customers_email_key") {
                return ApiError::Validation("E-Mail-Adresse existiert bereits".into());
            }
        }
        ApiError::Database(e)
    })?;

    row.map(|c| (axum::http::StatusCode::CREATED, Json(c)))
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

#[derive(Debug, Serialize, FromRow)]
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
        Some(s) if s == "accepted" || s == "done" || s == "paid" => &[],
        _ => &["accepted", "done", "paid"],
    };

    let orders: Vec<OrderListItem> = if statuses.is_empty() {
        // Single status filter
        sqlx::query_as(
            r#"
            SELECT q.id,
                   c.name AS customer_name,
                   c.email AS customer_email,
                   oa.city AS origin_city,
                   da.city AS destination_city,
                   q.estimated_volume_m3,
                   q.status,
                   q.preferred_date,
                   (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
                   (SELECT cb.booking_date FROM calendar_bookings cb WHERE cb.inquiry_id = q.id AND cb.status <> 'cancelled' LIMIT 1) AS booking_date,
                   q.created_at
            FROM inquiries q
            JOIN customers c ON q.customer_id = c.id
            LEFT JOIN addresses oa ON q.origin_address_id = oa.id
            LEFT JOIN addresses da ON q.destination_address_id = da.id
            WHERE q.status = $1
              AND (c.name ILIKE $2 OR c.email ILIKE $2)
            ORDER BY COALESCE(q.preferred_date, q.created_at) ASC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(status_filter.unwrap())
        .bind(&search)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        // All order statuses
        sqlx::query_as(
            r#"
            SELECT q.id,
                   c.name AS customer_name,
                   c.email AS customer_email,
                   oa.city AS origin_city,
                   da.city AS destination_city,
                   q.estimated_volume_m3,
                   q.status,
                   q.preferred_date,
                   (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
                   (SELECT cb.booking_date FROM calendar_bookings cb WHERE cb.inquiry_id = q.id AND cb.status <> 'cancelled' LIMIT 1) AS booking_date,
                   q.created_at
            FROM inquiries q
            JOIN customers c ON q.customer_id = c.id
            LEFT JOIN addresses oa ON q.origin_address_id = oa.id
            LEFT JOIN addresses da ON q.destination_address_id = da.id
            WHERE q.status IN ('accepted', 'done', 'paid')
              AND (c.name ILIKE $1 OR c.email ILIKE $1)
            ORDER BY COALESCE(q.preferred_date, q.created_at) ASC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(&search)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    let total: (i64,) = if statuses.is_empty() {
        sqlx::query_as(
            r#"
            SELECT COUNT(*)
            FROM inquiries q
            JOIN customers c ON q.customer_id = c.id
            WHERE q.status = $1
              AND (c.name ILIKE $2 OR c.email ILIKE $2)
            "#,
        )
        .bind(status_filter.unwrap())
        .bind(&search)
        .fetch_one(&state.db)
        .await?
    } else {
        sqlx::query_as(
            r#"
            SELECT COUNT(*)
            FROM inquiries q
            JOIN customers c ON q.customer_id = c.id
            WHERE q.status IN ('accepted', 'done', 'paid')
              AND (c.name ILIKE $1 OR c.email ILIKE $1)
            "#,
        )
        .bind(&search)
        .fetch_one(&state.db)
        .await?
    };

    Ok(Json(OrdersListResponse {
        orders,
        total: total.0,
    }))
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

#[derive(Debug, Serialize, FromRow)]
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
    let row: Option<AddressResponse> = sqlx::query_as(
        r#"
        UPDATE addresses SET
            street = COALESCE($2, street),
            city = COALESCE($3, city),
            postal_code = COALESCE($4, postal_code),
            floor = COALESCE($5, floor),
            elevator = COALESCE($6, elevator)
        WHERE id = $1
        RETURNING id, street, city, postal_code, floor, elevator
        "#,
    )
    .bind(id)
    .bind(&request.street)
    .bind(&request.city)
    .bind(&request.postal_code)
    .bind(&request.floor)
    .bind(request.elevator)
    .fetch_optional(&state.db)
    .await?;

    row.ok_or_else(|| ApiError::NotFound(format!("Adresse {id} nicht gefunden")))
        .map(Json)
}

// --- Users ---

#[derive(Debug, Serialize, FromRow)]
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
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<UserListResponse>, ApiError> {
    let users: Vec<UserListItem> = sqlx::query_as(
        "SELECT id, email, name, role, created_at FROM users ORDER BY created_at ASC",
    )
    .fetch_all(&state.db)
    .await?;

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
    if claims.sub == id {
        return Err(ApiError::Validation(
            "Sie koennen sich nicht selbst loeschen".into(),
        ));
    }

    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
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
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Cascades: inquiries, offers, volume_estimations, email_threads, email_messages
    let result = sqlx::query("DELETE FROM customers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
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

#[derive(Debug, Serialize, FromRow)]
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

    let threads: Vec<EmailThreadListItem> = sqlx::query_as(
        r#"
        SELECT
            et.id,
            et.customer_id,
            c.email AS customer_email,
            c.name AS customer_name,
            et.inquiry_id,
            et.subject,
            COUNT(em.id) AS message_count,
            MAX(em.created_at) AS last_message_at,
            (SELECT direction FROM email_messages
             WHERE thread_id = et.id ORDER BY created_at DESC LIMIT 1) AS last_direction,
            et.created_at
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        LEFT JOIN email_messages em ON em.thread_id = et.id
        WHERE c.name ILIKE $1 OR c.email ILIKE $1 OR et.subject ILIKE $1
        GROUP BY et.id, c.email, c.name
        ORDER BY MAX(em.created_at) DESC NULLS LAST
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(DISTINCT et.id)
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE c.name ILIKE $1 OR c.email ILIKE $1 OR et.subject ILIKE $1
        "#,
    )
    .bind(&search)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(EmailThreadListResponse { threads, total }))
}

#[derive(Debug, Serialize)]
struct EmailThreadDetailResponse {
    thread: EmailThreadDetail,
    messages: Vec<EmailMessageItem>,
}

#[derive(Debug, Serialize, FromRow)]
struct EmailThreadDetail {
    id: Uuid,
    customer_id: Uuid,
    customer_email: String,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
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
    let thread: Option<EmailThreadDetail> = sqlx::query_as(
        r#"
        SELECT et.id, et.customer_id, c.email AS customer_email, c.name AS customer_name,
               et.inquiry_id, et.subject, et.created_at
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE et.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let thread =
        thread.ok_or_else(|| ApiError::NotFound(format!("E-Mail-Thread {id} nicht gefunden")))?;

    let messages: Vec<EmailMessageItem> = sqlx::query_as(
        r#"
        SELECT id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at
        FROM email_messages
        WHERE thread_id = $1 AND status != 'discarded'
        ORDER BY created_at ASC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

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
    // Fetch draft message + real customer email from thread→customer join
    let row: Option<(Option<String>, Option<String>, String)> = sqlx::query_as(
        r#"
        SELECT em.subject, em.body_text, c.email AS customer_email
        FROM email_messages em
        JOIN email_threads et ON em.thread_id = et.id
        JOIN customers c ON et.customer_id = c.id
        WHERE em.id = $1 AND em.status = 'draft'
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (subject, body_text, customer_email) =
        row.ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;

    let subject = subject.unwrap_or_else(|| "Ihre Anfrage — AUST Umzuege".into());
    let body = body_text.unwrap_or_default();

    // Send via SMTP to the actual customer email
    send_plain_email(&state.config.email, &customer_email, &subject, &body)
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

    // Update status + fix to_address to the real customer email
    sqlx::query("UPDATE email_messages SET status = 'sent', to_address = $2 WHERE id = $1")
        .bind(id)
        .bind(&customer_email)
        .execute(&state.db)
        .await?;

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
    let result = sqlx::query(
        "UPDATE email_messages SET status = 'discarded' WHERE id = $1 AND status = 'draft'",
    )
    .bind(id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
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
    let result = sqlx::query(
        "UPDATE email_messages SET subject = COALESCE($2, subject), body_text = COALESCE($3, body_text) WHERE id = $1 AND status = 'draft'",
    )
    .bind(id)
    .bind(&request.subject)
    .bind(&request.body_text)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
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
    let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT et.customer_id, c.email, et.subject
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE et.id = $1
        "#,
    )
    .bind(thread_id)
    .fetch_optional(&state.db)
    .await?;

    let (_customer_id, customer_email, thread_subject) = row.ok_or_else(|| {
        ApiError::NotFound(format!("E-Mail-Thread {thread_id} nicht gefunden"))
    })?;

    let subject = request.subject.or(thread_subject);
    let from_address = &state.config.email.from_address;
    let id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        r#"
        INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, 'draft', $7)
        "#,
    )
    .bind(id)
    .bind(thread_id)
    .bind(from_address)
    .bind(&customer_email)
    .bind(&subject)
    .bind(&request.body_text)
    .bind(now)
    .execute(&state.db)
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
    let customer_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO customers (id, email, created_at, updated_at)
        VALUES ($1, $2, $3, $3)
        ON CONFLICT (email) DO UPDATE SET updated_at = $3
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&request.customer_email)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Create thread
    let thread_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO email_threads (id, customer_id, subject, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(thread_id)
    .bind(customer_id)
    .bind(&request.subject)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Create draft message
    let message_id = Uuid::now_v7();
    let from_address = &state.config.email.from_address;
    sqlx::query(
        r#"
        INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, 'draft', $7)
        "#,
    )
    .bind(message_id)
    .bind(thread_id)
    .bind(from_address)
    .bind(&request.customer_email)
    .bind(&request.subject)
    .bind(&request.body_text)
    .bind(now)
    .execute(&state.db)
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
