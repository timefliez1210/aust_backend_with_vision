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
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use aust_offer_generator::OfferLineItem;
use crate::orchestrator::parse_items_list_text;
use crate::routes::offers::{build_offer_with_overrides, parse_detected_items, OfferOverrides, VolumeEstimationRow};
use crate::services::db::insert_estimation_no_return;
use crate::services::status_sync;
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(dashboard))
        .route("/customers", get(list_customers).post(create_customer))
        .route("/customers/{id}", get(get_customer).patch(update_customer))
        .route("/quotes", get(list_admin_quotes).post(create_quote))
        .route("/quotes/{id}", get(get_quote_detail))
        .route("/offers", get(list_offers))
        .route("/offers/{id}", get(get_offer_detail).patch(update_offer))
        .route("/offers/{id}/regenerate", post(regenerate_offer))
        .route("/offers/{id}/re-estimate", post(re_estimate_offer))
        .route("/offers/{id}/send", post(send_offer))
        .route("/offers/{id}/reject", post(reject_offer))
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
        .route("/offers/{id}/delete", post(delete_offer))
        .route("/quotes/{id}/delete", post(delete_quote))
        .route("/quotes/{id}/status", post(set_quote_status))
        .route("/customers/{id}/delete", post(delete_customer))
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

async fn dashboard(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<DashboardResponse>, ApiError> {
    let (open_quotes,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM quotes WHERE status IN ('pending', 'info_requested', 'volume_estimated')",
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
        JOIN quotes q ON o.quote_id = q.id
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
    quote_id: Uuid,
    price_cents: i64,
    status: String,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
}

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
        FROM quotes WHERE customer_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let offers: Vec<CustomerOffer> = sqlx::query_as(
        r#"
        SELECT o.id, o.quote_id, o.price_cents, o.status, o.created_at, o.sent_at
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
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

// --- Create Quote ---

#[derive(Debug, Deserialize)]
struct CreateQuoteAddress {
    street: String,
    city: String,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateQuoteRequest {
    customer_id: Uuid,
    origin: CreateQuoteAddress,
    destination: CreateQuoteAddress,
    preferred_date: Option<NaiveDate>,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    notes: Option<String>,
    items_list: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateQuoteResponse {
    id: Uuid,
    origin_address_id: Uuid,
    destination_address_id: Uuid,
}

async fn create_quote(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(request): Json<CreateQuoteRequest>,
) -> Result<(axum::http::StatusCode, Json<CreateQuoteResponse>), ApiError> {
    let now = Utc::now();

    // Verify customer exists
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM customers WHERE id = $1")
            .bind(request.customer_id)
            .fetch_optional(&state.db)
            .await?;

    if exists.is_none() {
        return Err(ApiError::NotFound(format!(
            "Kunde {} nicht gefunden",
            request.customer_id
        )));
    }

    // Create origin address
    let origin_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(origin_id)
    .bind(&request.origin.street)
    .bind(&request.origin.city)
    .bind(&request.origin.postal_code)
    .bind(&request.origin.floor)
    .bind(request.origin.elevator)
    .execute(&state.db)
    .await?;

    // Create destination address
    let dest_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(dest_id)
    .bind(&request.destination.street)
    .bind(&request.destination.city)
    .bind(&request.destination.postal_code)
    .bind(&request.destination.floor)
    .bind(request.destination.elevator)
    .execute(&state.db)
    .await?;

    // Parse items and compute volume if items_list provided
    let has_items = request.items_list.as_ref().is_some_and(|s| !s.trim().is_empty());
    let volume_m3 = if has_items {
        let items = parse_items_list_text(request.items_list.as_deref().unwrap());
        let computed: f64 = items.iter().map(|i| i.quantity as f64 * i.volume_m3).sum();
        if computed > 0.0 { Some(computed) } else { request.estimated_volume_m3 }
    } else {
        request.estimated_volume_m3
    };

    let status = if has_items || volume_m3.is_some() {
        "volume_estimated"
    } else {
        "pending"
    };

    // Create quote
    let quote_id = Uuid::now_v7();
    let preferred_date_ts = request
        .preferred_date
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));

    sqlx::query(
        r#"
        INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id,
                           status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
        "#,
    )
    .bind(quote_id)
    .bind(request.customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .bind(volume_m3)
    .bind(request.distance_km)
    .bind(preferred_date_ts)
    .bind(&request.notes)
    .bind(now)
    .execute(&state.db)
    .await?;

    // If items_list provided, create volume_estimation record
    if has_items {
        let items = parse_items_list_text(request.items_list.as_deref().unwrap());
        let result_data = serde_json::to_value(&items).ok();
        let source_data = serde_json::json!({"source": "admin_manual"});
        let total_vol = volume_m3.unwrap_or(0.0);

        insert_estimation_no_return(
            &state.db,
            Uuid::now_v7(),
            quote_id,
            "manual",
            &source_data,
            result_data.as_ref(),
            total_vol,
            0.8,
            now,
        )
        .await?;
    }

    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreateQuoteResponse {
            id: quote_id,
            origin_address_id: origin_id,
            destination_address_id: dest_id,
        }),
    ))
}

// --- Admin Quotes List ---

#[derive(Debug, Deserialize)]
struct ListAdminQuotesQuery {
    status: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
struct AdminQuoteListItem {
    id: Uuid,
    customer_name: Option<String>,
    customer_email: String,
    origin_city: Option<String>,
    destination_city: Option<String>,
    #[serde(rename = "volume_m3")]
    estimated_volume_m3: Option<f64>,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct AdminQuotesListResponse {
    quotes: Vec<AdminQuoteListItem>,
    total: i64,
}

async fn list_admin_quotes(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListAdminQuotesQuery>,
) -> Result<Json<AdminQuotesListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    let quotes: Vec<AdminQuoteListItem> = sqlx::query_as(
        r#"
        SELECT q.id,
               c.name AS customer_name,
               c.email AS customer_email,
               oa.city AS origin_city,
               da.city AS destination_city,
               q.estimated_volume_m3,
               q.status,
               q.created_at
        FROM quotes q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE ($1::text IS NULL OR q.status = $1)
          AND (c.name ILIKE $2 OR c.email ILIKE $2)
        ORDER BY q.created_at DESC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(&query.status)
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM quotes q
        JOIN customers c ON q.customer_id = c.id
        WHERE ($1::text IS NULL OR q.status = $1)
          AND (c.name ILIKE $2 OR c.email ILIKE $2)
        "#,
    )
    .bind(&query.status)
    .bind(&search)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(AdminQuotesListResponse { quotes, total }))
}

// --- Quote Detail (enriched with latest offer overlay) ---

#[derive(Debug, Serialize)]
struct QuoteDetailAddress {
    street: String,
    city: String,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Serialize)]
struct QuoteDetailOffer {
    offer_id: Uuid,
    offer_number: Option<String>,
    offer_status: String,
    persons: i32,
    hours: f64,
    rate_cents: i64,
    total_netto_cents: i64,
    total_brutto_cents: i64,
    line_items: Vec<OfferDetailLineItem>,
    valid_until: Option<NaiveDate>,
    pdf_url: Option<String>,
    created_at: DateTime<Utc>,
}

/// One estimation batch — used by the frontend to render per-batch delete buttons.
#[derive(Debug, Serialize)]
struct EstimationSummary {
    id: Uuid,
    method: String,
    status: String,
    total_volume_m3: Option<f64>,
    item_count: usize,
    created_at: DateTime<Utc>,
    /// Present for video estimations.
    source_video_url: Option<String>,
    /// Present for vision / depth-sensor estimations.
    source_image_urls: Vec<String>,
}

/// Full estimation row fetched for admin views.
#[derive(Debug, sqlx::FromRow)]
struct AdminEstimationRow {
    id: Uuid,
    method: String,
    status: String,
    result_data: Option<serde_json::Value>,
    source_data: serde_json::Value,
    total_volume_m3: Option<f64>,
    created_at: DateTime<Utc>,
}

impl AdminEstimationRow {
    fn to_summary(&self) -> EstimationSummary {
        let source_video_url = self.source_data.get("video_s3_key")
            .and_then(|v| v.as_str())
            .map(|k| format!("/api/v1/estimates/images/{k}"));

        let source_image_urls: Vec<String> = self.source_data.get("s3_keys")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|k| format!("/api/v1/estimates/images/{k}")).collect())
            .unwrap_or_default();

        let item_count = self.result_data.as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);

        EstimationSummary {
            id: self.id,
            method: self.method.clone(),
            status: self.status.clone(),
            total_volume_m3: self.total_volume_m3,
            item_count,
            created_at: self.created_at,
            source_video_url,
            source_image_urls,
        }
    }

    /// Convert to the subset that `parse_detected_items()` expects.
    fn as_vol_estimation_row(&self) -> VolumeEstimationRow {
        VolumeEstimationRow {
            result_data: self.result_data.clone(),
            source_data: Some(self.source_data.clone()),
            total_volume_m3: self.total_volume_m3,
            method: self.method.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct QuoteDetailResponse {
    id: Uuid,
    status: String,
    created_at: DateTime<Utc>,
    customer_name: String,
    customer_email: String,
    customer_phone: Option<String>,
    origin: Option<QuoteDetailAddress>,
    destination: Option<QuoteDetailAddress>,
    volume_m3: f64,
    distance_km: f64,
    preferred_date: Option<String>,
    notes: Option<String>,
    offer: Option<QuoteDetailOffer>,
    estimations: Vec<EstimationSummary>,
    items: Vec<OfferDetailItem>,
}

#[derive(Debug, FromRow)]
struct QuoteDetailRow {
    id: Uuid,
    status: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    preferred_date: Option<DateTime<Utc>>,
    notes: Option<String>,
    created_at: DateTime<Utc>,
    customer_name: Option<String>,
    customer_email: String,
    customer_phone: Option<String>,
    origin_street: Option<String>,
    origin_city: Option<String>,
    origin_postal: Option<String>,
    origin_floor: Option<String>,
    origin_elevator: Option<bool>,
    dest_street: Option<String>,
    dest_city: Option<String>,
    dest_postal: Option<String>,
    dest_floor: Option<String>,
    dest_elevator: Option<bool>,
    // Latest offer (nullable)
    offer_id: Option<Uuid>,
    offer_number: Option<String>,
    offer_status: Option<String>,
    offer_persons: Option<i32>,
    offer_hours: Option<f64>,
    offer_rate_cents: Option<i64>,
    offer_price_cents: Option<i64>,
    offer_line_items_json: Option<serde_json::Value>,
    offer_valid_until: Option<NaiveDate>,
    offer_pdf_key: Option<String>,
    offer_created_at: Option<DateTime<Utc>>,
}

async fn get_quote_detail(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<QuoteDetailResponse>, ApiError> {
    let row: Option<QuoteDetailRow> = sqlx::query_as(
        r#"
        SELECT q.id, q.status, q.estimated_volume_m3, q.distance_km, q.preferred_date, q.notes, q.created_at,
               COALESCE(c.name, c.email) AS customer_name, c.email AS customer_email, c.phone AS customer_phone,
               oa.street AS origin_street, oa.city AS origin_city, oa.postal_code AS origin_postal,
               oa.floor AS origin_floor, oa.elevator AS origin_elevator,
               da.street AS dest_street, da.city AS dest_city, da.postal_code AS dest_postal,
               da.floor AS dest_floor, da.elevator AS dest_elevator,
               lo.id AS offer_id, lo.offer_number, lo.status AS offer_status,
               lo.persons AS offer_persons, lo.hours_estimated AS offer_hours,
               lo.rate_per_hour_cents AS offer_rate_cents, lo.price_cents AS offer_price_cents,
               lo.line_items_json AS offer_line_items_json, lo.valid_until AS offer_valid_until,
               lo.pdf_storage_key AS offer_pdf_key, lo.created_at AS offer_created_at
        FROM quotes q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        LEFT JOIN LATERAL (
            SELECT * FROM offers WHERE quote_id = q.id ORDER BY created_at DESC LIMIT 1
        ) lo ON true
        WHERE q.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Anfrage {id} nicht gefunden")))?;

    let origin = match (&row.origin_street, &row.origin_city) {
        (Some(s), Some(c)) => Some(QuoteDetailAddress {
            street: s.clone(),
            city: c.clone(),
            postal_code: row.origin_postal.clone(),
            floor: row.origin_floor.clone(),
            elevator: row.origin_elevator,
        }),
        _ => None,
    };
    let destination = match (&row.dest_street, &row.dest_city) {
        (Some(s), Some(c)) => Some(QuoteDetailAddress {
            street: s.clone(),
            city: c.clone(),
            postal_code: row.dest_postal.clone(),
            floor: row.dest_floor.clone(),
            elevator: row.dest_elevator,
        }),
        _ => None,
    };

    let preferred_date = row.preferred_date.map(|d| d.format("%d.%m.%Y").to_string());

    // Build offer overlay if a latest offer exists
    let offer = if let Some(offer_id) = row.offer_id {
        let persons = row.offer_persons.unwrap_or(2);
        let netto = row.offer_price_cents.unwrap_or(0);
        let brutto = (netto as f64 * 1.19).round() as i64;

        let line_items: Vec<OfferDetailLineItem> = row
            .offer_line_items_json
            .as_ref()
            .and_then(|json| serde_json::from_value::<Vec<serde_json::Value>>(json.clone()).ok())
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let label = item.get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("Sonstiges")
                            .to_string();
                        let remark = item.get("remark").and_then(|v| v.as_str()).map(String::from);
                        let is_labor = item.get("is_labor").and_then(|b| b.as_bool()).unwrap_or(false);
                        let quantity = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(1.0);
                        let unit_price = item.get("unit_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
                        let unit_price_cents = (unit_price * 100.0).round() as i64;
                        let flat_total = item.get("flat_total").and_then(|v| v.as_f64());
                        let total_cents = if let Some(ft) = flat_total {
                            (ft * 100.0).round() as i64
                        } else if is_labor {
                            (quantity * unit_price * persons as f64 * 100.0).round() as i64
                        } else {
                            (quantity * unit_price * 100.0).round() as i64
                        };
                        OfferDetailLineItem { label, remark, quantity, unit_price_cents, total_cents, is_labor }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let pdf_url = row.offer_pdf_key.as_ref().map(|_| format!("/api/v1/offers/{offer_id}/pdf"));

        Some(QuoteDetailOffer {
            offer_id,
            offer_number: row.offer_number,
            offer_status: row.offer_status.unwrap_or_default(),
            persons,
            hours: row.offer_hours.unwrap_or(0.0),
            rate_cents: row.offer_rate_cents.unwrap_or(3000),
            total_netto_cents: netto,
            total_brutto_cents: brutto,
            line_items,
            valid_until: row.offer_valid_until,
            pdf_url,
            created_at: row.offer_created_at.unwrap_or(row.created_at),
        })
    } else {
        None
    };

    // Fetch all volume estimations (all statuses — frontend needs to see processing/failed too)
    let est_rows: Vec<AdminEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, result_data, source_data, total_volume_m3, method, status, created_at
        FROM volume_estimations
        WHERE quote_id = $1
        ORDER BY created_at
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let estimations: Vec<EstimationSummary> = est_rows.iter().map(|e| e.to_summary()).collect();

    let mut items: Vec<OfferDetailItem> = Vec::new();
    for est in &est_rows {
        if est.status != "completed" { continue; }
        let vol_row = est.as_vol_estimation_row();
        let detected = parse_detected_items(Some(&vol_row));
        let source_s3_keys: Vec<String> = est.source_data.get("s3_keys")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        for d in &detected {
            let crop_url = d.crop_s3_key.as_ref().map(|k| format!("/api/v1/estimates/images/{k}"));
            let source_image_url = d.bbox_image_index
                .and_then(|idx| source_s3_keys.get(idx))
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            items.push(OfferDetailItem {
                name: d.german_name.clone().unwrap_or_else(|| d.name.clone()),
                volume_m3: d.volume_m3,
                quantity: 1,
                crop_url,
                source_image_url,
                bbox: d.bbox.clone(),
            });
        }
    }

    Ok(Json(QuoteDetailResponse {
        id: row.id,
        status: row.status,
        created_at: row.created_at,
        customer_name: row.customer_name.unwrap_or_default(),
        customer_email: row.customer_email,
        customer_phone: row.customer_phone,
        origin,
        destination,
        volume_m3: row.estimated_volume_m3.unwrap_or(0.0),
        distance_km: row.distance_km.unwrap_or(0.0),
        preferred_date,
        notes: row.notes,
        offer,
        estimations,
        items,
    }))
}

// --- Offers ---

#[derive(Debug, Deserialize)]
struct ListOffersQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
struct OfferListItem {
    id: Uuid,
    offer_number: Option<String>,
    customer_name: Option<String>,
    total_brutto_cents: Option<i64>,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct OfferListResponse {
    offers: Vec<OfferListItem>,
    total: i64,
}

async fn list_offers(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListOffersQuery>,
) -> Result<Json<OfferListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);

    let offers: Vec<OfferListItem> = sqlx::query_as(
        r#"
        SELECT o.id,
               o.offer_number,
               COALESCE(c.name, c.email) AS customer_name,
               CAST(ROUND(o.price_cents * 1.19) AS BIGINT) AS total_brutto_cents,
               o.status,
               o.created_at
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE ($1::text IS NULL OR o.status = $1)
        ORDER BY o.created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&query.status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM offers WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(&query.status)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(OfferListResponse { offers, total }))
}

// --- Offer Detail (enriched) ---

#[derive(Debug, Serialize)]
struct OfferDetailResponse {
    id: Uuid,
    offer_number: Option<String>,
    quote_id: Uuid,
    customer_name: String,
    customer_email: String,
    origin_address: String,
    destination_address: String,
    volume_m3: f64,
    distance_km: f64,
    persons: i32,
    hours: f64,
    rate_cents: i64,
    total_netto_cents: i64,
    total_brutto_cents: i64,
    line_items: Vec<OfferDetailLineItem>,
    status: String,
    valid_until: Option<NaiveDate>,
    pdf_url: Option<String>,
    created_at: DateTime<Utc>,
    estimations: Vec<EstimationSummary>,
    items: Vec<OfferDetailItem>,
    email_subject: String,
    email_body: String,
}

#[derive(Debug, Serialize)]
struct OfferDetailLineItem {
    label: String,
    remark: Option<String>,
    quantity: f64,
    unit_price_cents: i64,
    total_cents: i64,
    is_labor: bool,
}

#[derive(Debug, Serialize)]
struct OfferDetailItem {
    name: String,
    volume_m3: f64,
    quantity: u32,
    crop_url: Option<String>,
    source_image_url: Option<String>,
    bbox: Option<Vec<f64>>,
}

#[derive(Debug, FromRow)]
struct OfferDetailRow {
    id: Uuid,
    offer_number: Option<String>,
    quote_id: Uuid,
    price_cents: i64,
    status: String,
    valid_until: Option<NaiveDate>,
    pdf_storage_key: Option<String>,
    created_at: DateTime<Utc>,
    persons: Option<i32>,
    hours_estimated: Option<f64>,
    rate_per_hour_cents: Option<i64>,
    line_items_json: Option<serde_json::Value>,
    // Joined fields
    customer_name: Option<String>,
    customer_email: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    origin_street: Option<String>,
    origin_city: Option<String>,
    origin_postal: Option<String>,
    dest_street: Option<String>,
    dest_city: Option<String>,
    dest_postal: Option<String>,
}


async fn get_offer_detail(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<OfferDetailResponse>, ApiError> {
    let row: Option<OfferDetailRow> = sqlx::query_as(
        r#"
        SELECT o.id, o.offer_number, o.quote_id, o.price_cents, o.status, o.valid_until,
               o.pdf_storage_key, o.created_at, o.persons, o.hours_estimated,
               o.rate_per_hour_cents, o.line_items_json,
               COALESCE(c.name, c.email) AS customer_name,
               c.email AS customer_email,
               q.estimated_volume_m3, q.distance_km,
               oa.street AS origin_street, oa.city AS origin_city, oa.postal_code AS origin_postal,
               da.street AS dest_street, da.city AS dest_city, da.postal_code AS dest_postal
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE o.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Angebot {id} nicht gefunden")))?;

    // Parse line items from JSON
    let persons = row.persons.unwrap_or(2);
    let line_items: Vec<OfferDetailLineItem> = row
        .line_items_json
        .as_ref()
        .and_then(|json| {
            serde_json::from_value::<Vec<serde_json::Value>>(json.clone()).ok()
        })
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let label = item.get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Sonstiges")
                        .to_string();
                    let is_labor = item.get("is_labor").and_then(|b| b.as_bool()).unwrap_or(false);
                    let remark = item.get("remark").and_then(|v| v.as_str()).map(String::from);
                    let quantity = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(1.0);
                    let unit_price = item.get("unit_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    let unit_price_cents = (unit_price * 100.0).round() as i64;
                    let total_cents = if is_labor {
                        (quantity * unit_price * persons as f64 * 100.0).round() as i64
                    } else {
                        (quantity * unit_price * 100.0).round() as i64
                    };
                    OfferDetailLineItem {
                        label,
                        remark,
                        quantity,
                        unit_price_cents,
                        total_cents,
                        is_labor,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Fetch all volume estimations for this quote (all statuses)
    let est_rows: Vec<AdminEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, result_data, source_data, total_volume_m3, method, status, created_at
        FROM volume_estimations
        WHERE quote_id = $1
        ORDER BY created_at
        "#,
    )
    .bind(row.quote_id)
    .fetch_all(&state.db)
    .await?;

    let estimations: Vec<EstimationSummary> = est_rows.iter().map(|e| e.to_summary()).collect();

    let mut items: Vec<OfferDetailItem> = Vec::new();
    for est in &est_rows {
        if est.status != "completed" { continue; }
        let vol_row = est.as_vol_estimation_row();
        let detected = parse_detected_items(Some(&vol_row));
        let source_s3_keys: Vec<String> = est.source_data.get("s3_keys")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        for d in &detected {
            let crop_url = d.crop_s3_key.as_ref().map(|k| format!("/api/v1/estimates/images/{k}"));
            let source_image_url = d.bbox_image_index
                .and_then(|idx| source_s3_keys.get(idx))
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            items.push(OfferDetailItem {
                name: d.german_name.clone().unwrap_or_else(|| d.name.clone()),
                volume_m3: d.volume_m3,
                quantity: 1,
                crop_url,
                source_image_url,
                bbox: d.bbox.clone(),
            });
        }
    }

    let netto = row.price_cents;
    let brutto = (netto as f64 * 1.19).round() as i64;

    let origin_addr = format_address(
        row.origin_street.as_deref(),
        row.origin_postal.as_deref(),
        row.origin_city.as_deref(),
    );
    let dest_addr = format_address(
        row.dest_street.as_deref(),
        row.dest_postal.as_deref(),
        row.dest_city.as_deref(),
    );

    let pdf_url = row
        .pdf_storage_key
        .as_ref()
        .map(|_| format!("/api/v1/offers/{}/pdf", row.id));

    // Build email draft
    let customer_name_str = row.customer_name.clone().unwrap_or_default();
    let email_subject = "Ihr Umzugsangebot".to_string();
    let email_body = build_email_draft(&customer_name_str);

    Ok(Json(OfferDetailResponse {
        id: row.id,
        offer_number: row.offer_number,
        quote_id: row.quote_id,
        customer_name: customer_name_str,
        customer_email: row.customer_email,
        origin_address: origin_addr,
        destination_address: dest_addr,
        volume_m3: row.estimated_volume_m3.unwrap_or(0.0),
        distance_km: row.distance_km.unwrap_or(0.0),
        persons,
        hours: row.hours_estimated.unwrap_or(4.0),
        rate_cents: row.rate_per_hour_cents.unwrap_or(3000),
        total_netto_cents: netto,
        total_brutto_cents: brutto,
        line_items,
        status: row.status,
        valid_until: row.valid_until,
        pdf_url,
        created_at: row.created_at,
        estimations,
        items,
        email_subject,
        email_body,
    }))
}

fn format_address(street: Option<&str>, postal: Option<&str>, city: Option<&str>) -> String {
    match (street, city) {
        (Some(s), Some(c)) => {
            let pc = postal.map(|p| format!("{p} ")).unwrap_or_default();
            format!("{s}, {pc}{c}")
        }
        _ => String::new(),
    }
}

fn build_email_draft(customer_name: &str) -> String {
    let greeting = crate::routes::offers::greeting_for_name(customer_name);
    format!(
        "{greeting}\n\n\
        anbei erhalten Sie unser Angebot für Ihren Umzug.\n\n\
        Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
        Mit freundlichen Grüßen,\n\
        Ihr Umzugsteam\n\
        AUST Umzüge"
    )
}

// --- Update Offer ---

#[derive(Debug, Deserialize)]
struct UpdateOfferRequest {
    price_netto_cents: Option<i64>,
    persons: Option<i32>,
    hours: Option<f64>,
    rate_per_hour_cents: Option<i64>,
    valid_until: Option<NaiveDate>,
    status: Option<String>,
    line_items_json: Option<serde_json::Value>,
}

async fn update_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateOfferRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE offers SET
            price_cents = COALESCE($2, price_cents),
            persons = COALESCE($3, persons),
            hours_estimated = COALESCE($4, hours_estimated),
            rate_per_hour_cents = COALESCE($5, rate_per_hour_cents),
            valid_until = COALESCE($6, valid_until),
            status = COALESCE($7, status),
            line_items_json = COALESCE($8, line_items_json)
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(request.price_netto_cents)
    .bind(request.persons)
    .bind(request.hours)
    .bind(request.rate_per_hour_cents)
    .bind(request.valid_until)
    .bind(&request.status)
    .bind(&request.line_items_json)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("Angebot {id} nicht gefunden")));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Regenerate Offer ---

#[derive(Debug, Deserialize)]
struct RegenerateRequest {
    price_cents: Option<i64>,
    persons: Option<u32>,
    hours: Option<f64>,
    rate: Option<f64>,
    /// Custom non-labor line items (description, quantity, unit_price in EUR).
    #[serde(default)]
    line_items: Option<Vec<RegenerateLineItem>>,
}

#[derive(Debug, Deserialize)]
struct RegenerateLineItem {
    description: String,
    quantity: f64,
    unit_price: f64,
    #[serde(default)]
    remark: Option<String>,
}

async fn regenerate_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<RegenerateRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT quote_id FROM offers WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let (quote_id,) =
        row.ok_or_else(|| ApiError::NotFound(format!("Angebot {id} nicht gefunden")))?;

    let overrides = OfferOverrides {
        price_cents: request.price_cents,
        persons: request.persons,
        hours: request.hours,
        rate: request.rate,
        line_items: request.line_items.map(|items| {
            items
                .into_iter()
                .map(|li| OfferLineItem {
                    description: li.description,
                    quantity: li.quantity,
                    unit_price: li.unit_price,
                    remark: li.remark,
                    ..Default::default()
                })
                .collect()
        }),
        existing_offer_id: Some(id),
    };

    let generated =
        build_offer_with_overrides(&state.db, &*state.storage, &state.config, quote_id, Some(30), &overrides)
            .await?;

    Ok(Json(serde_json::json!({
        "id": generated.offer.id,
        "quote_id": generated.offer.quote_id,
        "price_cents": generated.offer.price_cents,
        "status": "draft",
        "created_at": generated.offer.created_at,
    })))
}

/// Re-estimate an offer: refresh distance from ORS, recalculate pricing, and regenerate the PDF
/// while keeping the same offer ID and offer number.
async fn re_estimate_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 1. Fetch offer → quote_id
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT quote_id FROM offers WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let (quote_id,) =
        row.ok_or_else(|| ApiError::NotFound(format!("Angebot {id} nicht gefunden")))?;

    // 2. Fetch quote origin, destination and stop addresses for distance recalculation
    #[derive(sqlx::FromRow)]
    struct QuoteAddrIds {
        origin_address_id: Option<Uuid>,
        destination_address_id: Option<Uuid>,
        stop_address_id: Option<Uuid>,
    }
    let addr_row: Option<QuoteAddrIds> = sqlx::query_as(
        "SELECT origin_address_id, destination_address_id, stop_address_id FROM quotes WHERE id = $1",
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await?;

    if let Some(QuoteAddrIds { origin_address_id: Some(origin_id), destination_address_id: Some(dest_id), stop_address_id }) = addr_row {
        #[derive(sqlx::FromRow)]
        struct AddrRow { street: String, city: String, postal_code: Option<String> }
        let fmt = |a: &AddrRow| -> String {
            format!(
                "{}, {}{}",
                a.street,
                a.postal_code.as_deref().map(|p| format!("{p} ")).unwrap_or_default(),
                a.city
            )
        };

        let origin: Option<AddrRow> = sqlx::query_as(
            "SELECT street, city, postal_code FROM addresses WHERE id = $1",
        )
        .bind(origin_id)
        .fetch_optional(&state.db)
        .await?;

        let dest: Option<AddrRow> = sqlx::query_as(
            "SELECT street, city, postal_code FROM addresses WHERE id = $1",
        )
        .bind(dest_id)
        .fetch_optional(&state.db)
        .await?;

        if let (Some(o), Some(d)) = (origin, dest) {
            let mut route_addresses = vec![fmt(&o)];
            if let Some(stop_id) = stop_address_id {
                let stop: Option<AddrRow> = sqlx::query_as(
                    "SELECT street, city, postal_code FROM addresses WHERE id = $1",
                )
                .bind(stop_id)
                .fetch_optional(&state.db)
                .await?;
                if let Some(s) = stop {
                    route_addresses.push(fmt(&s));
                }
            }
            route_addresses.push(fmt(&d));

            let calculator = RouteCalculator::new(state.config.maps.api_key.clone());
            match calculator.calculate(&RouteRequest { addresses: route_addresses }).await {
                Ok(result) => {
                    sqlx::query("UPDATE quotes SET distance_km = $1, updated_at = $2 WHERE id = $3")
                        .bind(result.total_distance_km)
                        .bind(chrono::Utc::now())
                        .bind(quote_id)
                        .execute(&state.db)
                        .await?;
                }
                Err(e) => {
                    tracing::warn!(quote_id = %quote_id, error = %e, "re-estimate: distance calculation failed, keeping existing distance");
                }
            }
        }
    }

    // 3. Regenerate offer in-place (keeps same ID, offer_number, recalculates everything else)
    let overrides = OfferOverrides {
        existing_offer_id: Some(id),
        ..Default::default()
    };

    let generated =
        build_offer_with_overrides(&state.db, &*state.storage, &state.config, quote_id, Some(30), &overrides)
            .await?;

    Ok(Json(serde_json::json!({
        "id": generated.offer.id,
        "quote_id": generated.offer.quote_id,
        "price_cents": generated.offer.price_cents,
        "status": "draft",
        "offer_number": generated.offer.offer_number,
    })))
}

// --- Send / Reject ---

#[derive(Debug, Deserialize, Default)]
struct SendOfferRequest {
    #[serde(default)]
    email_subject: Option<String>,
    #[serde(default)]
    email_body: Option<String>,
}

async fn send_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<SendOfferRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row: Option<(String, Option<String>, Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT c.email, o.pdf_storage_key, o.quote_id, COALESCE(c.name, c.email)
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (customer_email, storage_key, quote_id, customer_name) =
        row.ok_or_else(|| ApiError::NotFound(format!("Angebot {id} nicht gefunden")))?;

    let storage_key = storage_key
        .ok_or_else(|| ApiError::BadRequest("Angebot hat kein PDF".into()))?;

    let pdf_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")))?;

    let subject = request.email_subject.unwrap_or_else(|| "Ihr Umzugsangebot".to_string());
    let body = request.email_body.unwrap_or_else(|| build_email_draft(&customer_name.unwrap_or_default()));

    crate::orchestrator::send_offer_email_custom(&state, &customer_email, &pdf_bytes, id, &subject, &body)
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

    let now = Utc::now();
    sqlx::query("UPDATE offers SET status = 'sent', sent_at = $1 WHERE id = $2")
        .bind(now)
        .bind(id)
        .execute(&state.db)
        .await?;

    // Also update quote status to offer_sent
    sqlx::query("UPDATE quotes SET status = 'offer_sent', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(quote_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({
        "message": format!("Angebot an {customer_email} gesendet"),
        "sent_at": now,
    })))
}

async fn reject_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Fetch quote_id before updating
    let quote_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT quote_id FROM offers WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let result = sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("Angebot {id} nicht gefunden")));
    }

    // Also update quote status to rejected
    if let Some((quote_id,)) = quote_row {
        let now = Utc::now();
        let _ = sqlx::query("UPDATE quotes SET status = 'rejected', updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(quote_id)
            .execute(&state.db)
            .await;
    }

    Ok(Json(serde_json::json!({
        "message": "Angebot verworfen",
        "id": id,
    })))
}

// --- Lifecycle Transitions ---

#[derive(Debug, Deserialize)]
struct SetQuoteStatusRequest {
    status: String,
}

async fn set_quote_status(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetQuoteStatusRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let valid_statuses = [
        "pending",
        "info_requested",
        "volume_estimated",
        "offer_generated",
        "offer_sent",
        "accepted",
        "rejected",
        "done",
        "paid",
        "cancelled",
    ];

    if !valid_statuses.contains(&body.status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Ungueltiger Status: '{}'. Erlaubt: {}",
            body.status,
            valid_statuses.join(", ")
        )));
    }

    // Verify quote exists
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM quotes WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let (_current_status,) =
        row.ok_or_else(|| ApiError::NotFound(format!("Anfrage {id} nicht gefunden")))?;

    let now = Utc::now();

    sqlx::query("UPDATE quotes SET status = $1, updated_at = $2 WHERE id = $3")
        .bind(&body.status)
        .bind(now)
        .bind(id)
        .execute(&state.db)
        .await?;

    // Sync linked booking and offer status
    match body.status.as_str() {
        "accepted" => {
            status_sync::sync_quote_accepted(&state.db, &state.calendar, id).await.ok();
        }
        "rejected" | "cancelled" => {
            status_sync::sync_quote_cancelled(&state.db, &state.calendar, id).await.ok();
        }
        "offer_generated" | "offer_sent" | "pending" | "volume_estimated" => {
            status_sync::sync_quote_downgraded(&state.db, id).await.ok();
        }
        _ => {}
    }

    Ok(Json(serde_json::json!({
        "message": format!("Status auf '{}' gesetzt", body.status),
        "status": body.status,
    })))
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
                   (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.quote_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
                   (SELECT cb.booking_date FROM calendar_bookings cb WHERE cb.quote_id = q.id AND cb.status <> 'cancelled' LIMIT 1) AS booking_date,
                   q.created_at
            FROM quotes q
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
                   (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.quote_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
                   (SELECT cb.booking_date FROM calendar_bookings cb WHERE cb.quote_id = q.id AND cb.status <> 'cancelled' LIMIT 1) AS booking_date,
                   q.created_at
            FROM quotes q
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
            FROM quotes q
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
            FROM quotes q
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

// --- Delete individual records ---

async fn delete_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = sqlx::query("DELETE FROM offers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("Angebot {id} nicht gefunden")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_quote(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Cascades: volume_estimations, offers
    let result = sqlx::query("DELETE FROM quotes WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("Anfrage {id} nicht gefunden")));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Cascades: quotes, offers, volume_estimations, email_threads, email_messages
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
    quote_id: Option<Uuid>,
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
            et.quote_id,
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
    quote_id: Option<Uuid>,
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

async fn get_email_thread(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<EmailThreadDetailResponse>, ApiError> {
    let thread: Option<EmailThreadDetail> = sqlx::query_as(
        r#"
        SELECT et.id, et.customer_id, c.email AS customer_email, c.name AS customer_name,
               et.quote_id, et.subject, et.created_at
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

/// Send a draft email via SMTP (approve from dashboard).
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

    let subject = subject.unwrap_or_else(|| "Ihre Anfrage — AUST Umzüge".into());
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

/// Discard a draft email (reject from dashboard).
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

/// Send a plain text email via SMTP.
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
