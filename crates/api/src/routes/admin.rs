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

use crate::orchestrator::parse_items_list_text;
use crate::routes::offers::{build_offer_with_overrides, parse_detected_items, OfferOverrides, VolumeEstimationRow};
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(dashboard))
        .route("/customers", get(list_customers).post(create_customer))
        .route("/customers/{id}", get(get_customer).patch(update_customer))
        .route("/quotes", get(list_admin_quotes).post(create_quote))
        .route("/offers", get(list_offers))
        .route("/offers/{id}", get(get_offer_detail).patch(update_offer))
        .route("/offers/{id}/regenerate", post(regenerate_offer))
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
        .route("/customers/{id}/delete", post(delete_customer))
}

// --- Dashboard ---

#[derive(Debug, Serialize)]
struct DashboardResponse {
    open_quotes: i64,
    pending_offers: i64,
    todays_bookings: i64,
    total_customers: i64,
    recent_activity: Vec<ActivityItem>,
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
        "SELECT COUNT(*) FROM calendar_bookings WHERE moving_date = $1 AND status != 'cancelled'",
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

    Ok(Json(DashboardResponse {
        open_quotes,
        pending_offers,
        todays_bookings,
        total_customers,
        recent_activity: recent_offers,
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

        sqlx::query(
            r#"
            INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(quote_id)
        .bind("manual")
        .bind(source_data)
        .bind(result_data)
        .bind(total_vol)
        .bind(0.8f64)
        .bind(now)
        .execute(&state.db)
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
    items: Vec<OfferDetailItem>,
}

#[derive(Debug, Serialize)]
struct OfferDetailLineItem {
    label: String,
    quantity: f64,
    unit_price_cents: i64,
    total_cents: i64,
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

/// Row label for XLSX template line items
fn row_label(row: u32) -> &'static str {
    match row {
        31 => "De/Montage",
        32 => "Halteverbotszone",
        33 => "Umzugsmaterial",
        39 => "Transporter",
        42 => "Anfahrt/Abfahrt",
        _ => "Sonstiges",
    }
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
                    let xlsx_row = item.get("row").and_then(|r| r.as_u64()).unwrap_or(0) as u32;
                    let quantity = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(1.0);
                    let unit_price = item.get("unit_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    let unit_price_cents = (unit_price * 100.0).round() as i64;
                    let total_cents = (quantity * unit_price * 100.0).round() as i64;
                    OfferDetailLineItem {
                        label: row_label(xlsx_row).to_string(),
                        quantity,
                        unit_price_cents,
                        total_cents,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Fetch detected items from volume estimation
    let estimation: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT result_data, source_data, total_volume_m3, method
        FROM volume_estimations
        WHERE quote_id = $1
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(row.quote_id)
    .fetch_optional(&state.db)
    .await?;

    let detected = parse_detected_items(estimation.as_ref());
    let source_s3_keys: Vec<String> = estimation
        .as_ref()
        .and_then(|e| {
            e.source_data.as_ref()?.get("s3_keys")?.as_array().map(|arr| {
                arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            })
        })
        .unwrap_or_default();
    let items: Vec<OfferDetailItem> = detected
        .iter()
        .map(|d| {
            let crop_url = d.crop_s3_key.as_ref().map(|k| format!("/api/v1/estimates/images/{k}"));
            let source_image_url = d.bbox_image_index
                .and_then(|idx| source_s3_keys.get(idx))
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            OfferDetailItem {
                name: d.german_name.clone().unwrap_or_else(|| d.name.clone()),
                volume_m3: d.volume_m3,
                quantity: 1,
                crop_url,
                source_image_url,
                bbox: d.bbox.clone(),
            }
        })
        .collect();

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

    Ok(Json(OfferDetailResponse {
        id: row.id,
        offer_number: row.offer_number,
        quote_id: row.quote_id,
        customer_name: row.customer_name.unwrap_or_default(),
        customer_email: row.customer_email,
        origin_address: origin_addr,
        destination_address: dest_addr,
        volume_m3: row.estimated_volume_m3.unwrap_or(0.0),
        distance_km: row.distance_km.unwrap_or(0.0),
        persons: row.persons.unwrap_or(2),
        hours: row.hours_estimated.unwrap_or(4.0),
        rate_cents: row.rate_per_hour_cents.unwrap_or(3000),
        total_netto_cents: netto,
        total_brutto_cents: brutto,
        line_items,
        status: row.status,
        valid_until: row.valid_until,
        pdf_url,
        created_at: row.created_at,
        items,
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

    sqlx::query("DELETE FROM offers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    sqlx::query("UPDATE quotes SET status = 'volume_estimated' WHERE id = $1")
        .bind(quote_id)
        .execute(&state.db)
        .await?;

    let overrides = OfferOverrides {
        price_cents: request.price_cents,
        persons: request.persons,
        hours: request.hours,
        rate: request.rate,
    };

    let generated =
        build_offer_with_overrides(&state.db, &*state.storage, quote_id, Some(30), &overrides)
            .await?;

    Ok(Json(serde_json::json!({
        "id": generated.offer.id,
        "quote_id": generated.offer.quote_id,
        "price_cents": generated.offer.price_cents,
        "status": "draft",
        "created_at": generated.offer.created_at,
    })))
}

// --- Send / Reject ---

async fn send_offer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row: Option<(String, Option<String>, Uuid)> = sqlx::query_as(
        r#"
        SELECT c.email, o.pdf_storage_key, o.quote_id
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (customer_email, storage_key, _quote_id) =
        row.ok_or_else(|| ApiError::NotFound(format!("Angebot {id} nicht gefunden")))?;

    let storage_key = storage_key
        .ok_or_else(|| ApiError::BadRequest("Angebot hat kein PDF".into()))?;

    let pdf_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")))?;

    crate::orchestrator::send_offer_email(&state, &customer_email, &pdf_bytes, id)
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

    let now = Utc::now();
    sqlx::query("UPDATE offers SET status = 'sent', sent_at = $1 WHERE id = $2")
        .bind(now)
        .bind(id)
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
    let result = sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("Angebot {id} nicht gefunden")));
    }

    Ok(Json(serde_json::json!({
        "message": "Angebot verworfen",
        "id": id,
    })))
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
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let from_mailbox: lettre::message::Mailbox = format!(
        "{} <{}>",
        email_config.from_name, email_config.from_address
    )
    .parse()
    .map_err(|e| format!("Invalid from address: {e}"))?;

    let to_mailbox: lettre::message::Mailbox =
        to.parse().map_err(|e| format!("Invalid to address: {e}"))?;

    let message = Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject(subject)
        .body(body.to_string())
        .map_err(|e| format!("Failed to build email: {e}"))?;

    let creds = Credentials::new(
        email_config.username.clone(),
        email_config.password.clone(),
    );

    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&email_config.smtp_host)
        .map_err(|e| format!("SMTP relay setup failed: {e}"))?
        .port(email_config.smtp_port)
        .credentials(creds)
        .build();

    mailer
        .send(message)
        .await
        .map_err(|e| format!("SMTP send failed: {e}"))?;

    Ok(())
}
