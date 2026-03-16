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
        "SELECT COUNT(*) FROM inquiries WHERE COALESCE(scheduled_date, preferred_date::date) = $1 AND status NOT IN ('cancelled', 'rejected', 'expired')",
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

    // Unified recent activity: recent inquiry updates, unanswered emails, upcoming Termine.
    let recent_offers: Vec<ActivityItem> = sqlx::query_as(
        r#"
        SELECT activity_type, description, created_at, id, status
        FROM (
            -- Recently updated inquiries (not terminal)
            SELECT
                'inquiry' AS activity_type,
                COALESCE(c.name, c.email) || ' — ' || i.status AS description,
                i.updated_at AS created_at,
                i.id AS id,
                i.status AS status
            FROM inquiries i
            JOIN customers c ON i.customer_id = c.id
            WHERE i.status NOT IN ('cancelled', 'rejected', 'expired', 'paid')

            UNION ALL

            -- Recent offers (link goes to inquiry)
            SELECT
                'offer_' || o.status AS activity_type,
                COALESCE(c.name, c.email) || ' — ' || round((o.price_cents::numeric / 100 * 1.19), 2)::text || ' € brutto' AS description,
                o.created_at AS created_at,
                q.id AS id,
                o.status AS status
            FROM offers o
            JOIN inquiries q ON o.inquiry_id = q.id
            JOIN customers c ON q.customer_id = c.id

            UNION ALL

            -- Email threads awaiting reply (last message is inbound)
            SELECT
                'email' AS activity_type,
                COALESCE(c.name, c.email) || ': ' || COALESCE(et.subject, '(kein Betreff)') AS description,
                et.updated_at AS created_at,
                et.id AS id,
                'unanswered' AS status
            FROM email_threads et
            JOIN customers c ON et.customer_id = c.id
            WHERE (
                SELECT direction FROM email_messages
                WHERE thread_id = et.id
                ORDER BY created_at DESC LIMIT 1
            ) = 'inbound'

            UNION ALL

            -- Upcoming / recently created calendar items
            SELECT
                'calendar_item' AS activity_type,
                ci.title || COALESCE(' @ ' || ci.location, '') AS description,
                ci.created_at AS created_at,
                ci.id AS id,
                ci.status AS status
            FROM calendar_items ci
            WHERE ci.status = 'scheduled'
              AND (ci.scheduled_date IS NULL OR ci.scheduled_date >= CURRENT_DATE)
        ) combined
        ORDER BY created_at DESC
        LIMIT 15
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
        SELECT COALESCE(scheduled_date, preferred_date::date) AS booking_date, COUNT(*) AS booking_count
        FROM inquiries
        WHERE COALESCE(scheduled_date, preferred_date::date) BETWEEN $1 AND $2
          AND status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY COALESCE(scheduled_date, preferred_date::date)
        HAVING COUNT(*) > COALESCE(
            (SELECT capacity FROM calendar_capacity_overrides WHERE override_date = COALESCE(scheduled_date, preferred_date::date)),
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

    let customers: Vec<CustomerListItem> = sqlx::query_as(
        r#"
        SELECT id, email, name, salutation, first_name, last_name, phone, created_at
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
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    created_at: DateTime<Utc>,
    quotes: Vec<CustomerQuote>,
    offers: Vec<CustomerOffer>,
    termine: Vec<CustomerTermin>,
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

#[derive(Debug, Serialize, FromRow)]
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
    let customer: Option<CustomerListItem> = sqlx::query_as(
        "SELECT id, email, name, salutation, first_name, last_name, phone, created_at FROM customers WHERE id = $1",
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

    let termine: Vec<CustomerTermin> = sqlx::query_as(
        r#"
        SELECT id, title, category, scheduled_date, status
        FROM calendar_items WHERE customer_id = $1
        ORDER BY scheduled_date DESC NULLS LAST
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(CustomerDetailResponse {
        id: customer.id,
        email: customer.email,
        name: customer.name,
        salutation: customer.salutation,
        first_name: customer.first_name,
        last_name: customer.last_name,
        phone: customer.phone,
        created_at: customer.created_at,
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
    let row: Option<CustomerListItem> = sqlx::query_as(
        r#"
        UPDATE customers SET
            name = COALESCE($2, name),
            salutation = COALESCE($3, salutation),
            first_name = COALESCE($4, first_name),
            last_name = COALESCE($5, last_name),
            phone = COALESCE($6, phone),
            email = COALESCE($7, email)
        WHERE id = $1
        RETURNING id, email, name, salutation, first_name, last_name, phone, created_at
        "#,
    )
    .bind(id)
    .bind(&request.name)
    .bind(&request.salutation)
    .bind(&request.first_name)
    .bind(&request.last_name)
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

    let row: Option<CustomerListItem> = sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        RETURNING id, email, name, salutation, first_name, last_name, phone, created_at
        "#,
    )
    .bind(id)
    .bind(&request.email)
    .bind(&request.name)
    .bind(&request.salutation)
    .bind(&request.first_name)
    .bind(&request.last_name)
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
                   q.scheduled_date AS booking_date,
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
                   q.scheduled_date AS booking_date,
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
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<UserListResponse>, ApiError> {
    require_admin(&claims)?;
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
    require_admin(&claims)?;
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
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
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
    // Fetch draft + customer email + optional offer PDF key (when thread belongs to an inquiry with an active offer)
    let row: Option<(Option<String>, Option<String>, String, Option<String>, Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as(
            r#"
            SELECT em.subject, em.body_text, c.email,
                   o.pdf_storage_key, o.id AS offer_id, et.inquiry_id
            FROM email_messages em
            JOIN email_threads et ON em.thread_id = et.id
            JOIN customers c ON et.customer_id = c.id
            LEFT JOIN offers o ON o.inquiry_id = et.inquiry_id
                AND o.status NOT IN ('rejected', 'cancelled')
            WHERE em.id = $1 AND em.status = 'draft'
            "#,
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;

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
        sqlx::query("UPDATE offers SET status = 'sent', sent_at = $1 WHERE id = $2")
            .bind(now)
            .bind(oid)
            .execute(&state.db)
            .await?;

        sqlx::query("UPDATE inquiries SET status = 'offer_sent', updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(iid)
            .execute(&state.db)
            .await?;
    } else {
        // Plain email — no offer PDF attached (e.g. general inquiry reply)
        send_plain_email(&state.config.email, &customer_email, &subject, &body)
            .await
            .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;
    }

    // Mark draft as sent + fix to_address
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

#[derive(Debug, Serialize, FromRow)]
struct EmployeeDbRow {
    id: Uuid,
    salutation: Option<String>,
    first_name: String,
    last_name: String,
    email: String,
    phone: Option<String>,
    monthly_hours_target: f64,
    active: bool,
    arbeitsvertrag_key: Option<String>,
    mitarbeiterfragebogen_key: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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

    let rows: Vec<EmployeeDbRow> = sqlx::query_as(
        r#"
        SELECT id, salutation, first_name, last_name, email, phone,
               monthly_hours_target::float8 AS monthly_hours_target,
               active, created_at, updated_at,
               arbeitsvertrag_key, mitarbeiterfragebogen_key
        FROM employees
        WHERE ($1::text IS NULL OR first_name ILIKE $1 OR last_name ILIKE $1 OR email ILIKE $1)
          AND ($2::bool IS NULL OR active = $2)
        ORDER BY last_name, first_name
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(&search)
    .bind(active_filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM employees
        WHERE ($1::text IS NULL OR first_name ILIKE $1 OR last_name ILIKE $1 OR email ILIKE $1)
          AND ($2::bool IS NULL OR active = $2)
        "#,
    )
    .bind(&search)
    .bind(active_filter)
    .fetch_one(&state.db)
    .await?;

    // Parse month range for hours aggregation
    let month_range = query.month.as_ref().and_then(|m| parse_month_range(m));

    let mut employees = Vec::with_capacity(rows.len());
    for row in rows {
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

    sqlx::query(
        r#"
        INSERT INTO employees (id, salutation, first_name, last_name, email, phone, monthly_hours_target)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(&body.salutation)
    .bind(&body.first_name)
    .bind(&body.last_name)
    .bind(&body.email)
    .bind(&body.phone)
    .bind(target)
    .execute(&state.db)
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

    #[derive(Debug, Serialize, FromRow)]
    struct AssignmentRow {
        inquiry_id: Uuid,
        customer_name: Option<String>,
        origin_city: Option<String>,
        destination_city: Option<String>,
        booking_date: Option<NaiveDate>,
        planned_hours: f64,
        actual_hours: Option<f64>,
        notes: Option<String>,
        inquiry_status: String,
    }

    let assignments: Vec<AssignmentRow> = sqlx::query_as(
        r#"
        SELECT ie.inquiry_id,
               COALESCE(c.first_name || ' ' || c.last_name, c.name) AS customer_name,
               oa.city AS origin_city,
               da.city AS destination_city,
               COALESCE(i.scheduled_date, i.preferred_date::date) AS booking_date,
               ie.planned_hours::float8 AS planned_hours,
               ie.actual_hours::float8 AS actual_hours,
               ie.notes,
               i.status AS inquiry_status
        FROM inquiry_employees ie
        JOIN inquiries i ON ie.inquiry_id = i.id
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ie.employee_id = $1
        ORDER BY COALESCE(i.scheduled_date, i.preferred_date::date) DESC NULLS LAST
        LIMIT 50
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

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
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM employees WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into()));
    }

    if let Some(ref sal) = body.salutation {
        if !["Herr", "Frau", "D"].contains(&sal.as_str()) {
            return Err(ApiError::BadRequest("Ungueltige Anrede".into()));
        }
    }

    sqlx::query(
        r#"
        UPDATE employees SET
            salutation = COALESCE($2, salutation),
            first_name = COALESCE($3, first_name),
            last_name = COALESCE($4, last_name),
            email = COALESCE($5, email),
            phone = COALESCE($6, phone),
            monthly_hours_target = COALESCE($7, monthly_hours_target),
            active = COALESCE($8, active)
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(&body.salutation)
    .bind(&body.first_name)
    .bind(&body.last_name)
    .bind(&body.email)
    .bind(&body.phone)
    .bind(body.monthly_hours_target)
    .bind(body.active)
    .execute(&state.db)
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
    let result = sqlx::query("UPDATE employees SET active = false WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
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
    #[derive(FromRow)]
    struct TargetRow {
        monthly_hours_target: f64,
    }
    let target_row: TargetRow = sqlx::query_as(
        "SELECT monthly_hours_target::float8 AS monthly_hours_target FROM employees WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Mitarbeiter nicht gefunden".into()))?;

    #[derive(Debug, Serialize, FromRow)]
    struct HoursRow {
        inquiry_id: Uuid,
        customer_name: Option<String>,
        origin_city: Option<String>,
        destination_city: Option<String>,
        booking_date: Option<NaiveDate>,
        planned_hours: f64,
        actual_hours: Option<f64>,
        inquiry_status: String,
    }

    let rows: Vec<HoursRow> = sqlx::query_as(
        r#"
        SELECT ie.inquiry_id,
               COALESCE(c.first_name || ' ' || c.last_name, c.name) AS customer_name,
               oa.city AS origin_city,
               da.city AS destination_city,
               COALESCE(i.scheduled_date, i.preferred_date::date) AS booking_date,
               ie.planned_hours::float8 AS planned_hours,
               ie.actual_hours::float8 AS actual_hours,
               i.status AS inquiry_status
        FROM inquiry_employees ie
        JOIN inquiries i ON ie.inquiry_id = i.id
        JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ie.employee_id = $1
          AND COALESCE(i.scheduled_date, i.preferred_date::date, ie.created_at::date) BETWEEN $2 AND $3
        ORDER BY COALESCE(i.scheduled_date, i.preferred_date::date, ie.created_at::date)
        "#,
    )
    .bind(id)
    .bind(from_date)
    .bind(to_date)
    .fetch_all(&state.db)
    .await?;

    // Also fetch calendar item assignments for this employee in the same month.
    #[derive(Debug, Serialize, FromRow)]
    struct CalendarItemHoursRow {
        calendar_item_id: Uuid,
        title: String,
        category: String,
        location: Option<String>,
        scheduled_date: Option<NaiveDate>,
        planned_hours: f64,
        actual_hours: Option<f64>,
        status: String,
    }

    let item_rows: Vec<CalendarItemHoursRow> = sqlx::query_as(
        r#"
        SELECT cie.calendar_item_id,
               ci.title,
               ci.category,
               ci.location,
               ci.scheduled_date,
               cie.planned_hours::float8 AS planned_hours,
               cie.actual_hours::float8 AS actual_hours,
               ci.status
        FROM calendar_item_employees cie
        JOIN calendar_items ci ON ci.id = cie.calendar_item_id
        WHERE cie.employee_id = $1
          AND ci.scheduled_date BETWEEN $2 AND $3
        ORDER BY ci.scheduled_date
        "#,
    )
    .bind(id)
    .bind(from_date)
    .bind(to_date)
    .fetch_all(&state.db)
    .await?;

    let target = target_row.monthly_hours_target;
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
    #[derive(FromRow)]
    struct HoursSums {
        planned: Option<f64>,
        actual: Option<f64>,
    }

    let sums: HoursSums = sqlx::query_as(
        r#"
        SELECT
            COALESCE(SUM(planned_hours), 0.0)::float8 AS planned,
            COALESCE(SUM(COALESCE(actual_hours, planned_hours)), 0.0)::float8 AS actual
        FROM (
            SELECT ie.planned_hours::float8, ie.actual_hours::float8
            FROM inquiry_employees ie
            JOIN inquiries i ON i.id = ie.inquiry_id
            WHERE ie.employee_id = $1
              AND COALESCE(i.scheduled_date, i.preferred_date::date, ie.created_at::date)
                  BETWEEN $2 AND $3
            UNION ALL
            SELECT cie.planned_hours::float8, cie.actual_hours::float8
            FROM calendar_item_employees cie
            JOIN calendar_items ci ON ci.id = cie.calendar_item_id
            WHERE cie.employee_id = $1
              AND ci.scheduled_date BETWEEN $2 AND $3
        ) combined
        "#,
    )
    .bind(employee_id)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await?;

    Ok((sums.planned, sums.actual))
}

/// Fetch a single employee as JSON.
async fn fetch_employee_json(
    pool: &sqlx::PgPool,
    id: Uuid,
) -> Result<serde_json::Value, ApiError> {
    let row: EmployeeDbRow = sqlx::query_as(
        r#"
        SELECT id, salutation, first_name, last_name, email, phone,
               monthly_hours_target::float8 AS monthly_hours_target,
               active, arbeitsvertrag_key, mitarbeiterfragebogen_key,
               created_at, updated_at
        FROM employees WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
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
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM employees WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
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
    sqlx::query(&format!("UPDATE employees SET {col} = $2 WHERE id = $1"))
        .bind(id)
        .bind(&key)
        .execute(&state.db)
        .await?;

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
    let key: Option<String> = sqlx::query_scalar(&format!(
        "SELECT {}_key FROM employees WHERE id = $1",
        doc_type.replace('-', "_")
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let key = key.ok_or_else(|| ApiError::NotFound("Dokument nicht vorhanden".into()))?;

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
    let key: Option<String> = sqlx::query_scalar(&format!(
        "SELECT {col} FROM employees WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    if let Some(ref k) = key {
        // Best-effort S3 delete — log but don't fail if the object is already gone
        if let Err(e) = state.storage.delete(k).await {
            tracing::warn!("S3 delete for employee document {k} failed (ignoring): {e}");
        }
    }

    sqlx::query(&format!("UPDATE employees SET {col} = NULL WHERE id = $1"))
        .bind(id)
        .execute(&state.db)
        .await?;

    tracing::info!("Employee {id}: deleted {doc_type}");
    let employee = fetch_employee_json(&state.db, id).await?;
    Ok(Json(employee))
}
