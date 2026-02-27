use axum::{
    extract::{Path, Query, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, AppState};
use crate::routes::offers::{parse_detected_items, VolumeEstimationRow};
use crate::routes::shared::QuoteRow;
use aust_core::models::{CreateQuote, Quote, QuoteStatus, UpdateQuote};

/// Register the quote CRUD routes.
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Exposes the full quote lifecycle — create, list, detail, update, delete, and
/// manual estimation-item editing.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(create_quote).get(list_quotes))
        .route("/{id}", get(get_quote).patch(update_quote).delete(soft_delete_quote))
        .route("/{id}/estimation-items", put(update_estimation_items))
}

#[derive(Debug, Deserialize)]
struct ListQuotesQuery {
    status: Option<String>,
    customer_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct QuoteListResponse {
    quotes: Vec<Quote>,
    total: i64,
    limit: i64,
    offset: i64,
}


/// `POST /api/v1/quotes` — Create a new quote with `pending` status.
///
/// **Caller**: Axum router / external API consumers or test scripts.
/// **Why**: Basic quote creation with customer and address references. No volume estimation
/// is performed here; clients call the estimate endpoints separately.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `request` — `CreateQuote` JSON body with `customer_id`, address IDs, and optional fields
///
/// # Returns
/// `200 OK` with the newly created `Quote` JSON (status = "pending").
///
/// # Errors
/// - `500` on DB constraint violations or connection failures
async fn create_quote(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateQuote>,
) -> Result<Json<Quote>, ApiError> {
    let id = Uuid::now_v7();
    let now = chrono::Utc::now();

    let row: QuoteRow = sqlx::query_as(
        r#"
        INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id, status, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        "#
    )
    .bind(id)
    .bind(request.customer_id)
    .bind(request.origin_address_id)
    .bind(request.destination_address_id)
    .bind(QuoteStatus::Pending.as_str())
    .bind(request.preferred_date)
    .bind(&request.notes)
    .bind(now)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(Quote::from(row)))
}

// Enriched quote response with customer + address + estimation + offers
#[derive(Debug, Serialize)]
struct EnrichedQuote {
    quote: QuoteInfo,
    customer: QuoteCustomer,
    origin_address: Option<QuoteAddress>,
    destination_address: Option<QuoteAddress>,
    estimation: Option<EstimationInfo>,
    offers: Vec<QuoteOffer>,
    latest_offer: Option<LatestOfferPricing>,
}

#[derive(Debug, Serialize)]
struct QuoteInfo {
    id: Uuid,
    #[serde(rename = "volume_m3")]
    estimated_volume_m3: Option<f64>,
    distance_km: f64,
    notes: Option<String>,
    status: String,
    customer_message: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, FromRow)]
struct QuoteCustomer {
    id: Uuid,
    email: String,
    name: Option<String>,
    phone: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct QuoteAddress {
    id: Uuid,
    street: String,
    city: String,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Serialize)]
struct EstimationInfo {
    id: Uuid,
    method: String,
    total_volume_m3: f64,
    items: Vec<EstimationItem>,
    source_images: Vec<String>,
    source_videos: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EstimationItem {
    name: String,
    volume_m3: f64,
    quantity: u32,
    confidence: f64,
    crop_url: Option<String>,
    source_image_url: Option<String>,
    bbox: Option<Vec<f64>>,
    // Fields needed for round-trip editing
    #[serde(skip_serializing_if = "Option::is_none")]
    crop_s3_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bbox_image_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seen_in_images: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, FromRow)]
struct QuoteOffer {
    id: Uuid,
    total_brutto_cents: Option<i64>,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
struct OfferLineItemDetail {
    label: String,
    remark: Option<String>,
    quantity: f64,
    unit_price_cents: i64,
    total_cents: i64,
    is_labor: bool,
}

/// Pricing data from the latest offer, used to overlay on the Anfrage view
#[derive(Debug, Serialize)]
struct LatestOfferPricing {
    offer_id: Uuid,
    persons: i32,
    hours: f64,
    rate_cents: i64,
    total_netto_cents: i64,
    total_brutto_cents: i64,
    line_items: Vec<OfferLineItemDetail>,
}

#[derive(Debug, FromRow)]
struct VolumeEstimationDbRow {
    id: Uuid,
    method: String,
    total_volume_m3: Option<f64>,
    result_data: Option<serde_json::Value>,
    source_data: Option<serde_json::Value>,
}

/// `GET /api/v1/quotes/{id}` — Fetch a fully enriched quote with customer, addresses,
/// estimation items, linked offers, and the latest offer pricing overlay.
///
/// **Caller**: Axum router / customer-facing webapp and admin dashboard quote detail.
/// **Why**: Aggregates all data a client needs for the quote detail view in a single
/// request: customer contact info, both addresses (with floor/elevator), all completed
/// volume estimations aggregated into one item list, and the full line-item breakdown
/// of the most recent offer.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, storage)
/// - `id` — quote UUID path parameter
///
/// # Returns
/// `200 OK` with `EnrichedQuote` JSON containing nested customer, addresses, estimation,
/// offers list, and `latest_offer` pricing overlay.
///
/// # Errors
/// - `404` if quote or customer not found
async fn get_quote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<EnrichedQuote>, ApiError> {
    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Quote {id} not found")))?;
    let quote = Quote::from(row);

    let customer: QuoteCustomer = sqlx::query_as(
        "SELECT id, email, name, phone FROM customers WHERE id = $1",
    )
    .bind(quote.customer_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Customer not found".into()))?;

    let origin_address: Option<QuoteAddress> = if let Some(addr_id) = quote.origin_address_id {
        sqlx::query_as(
            "SELECT id, street, city, postal_code, floor, elevator FROM addresses WHERE id = $1",
        )
        .bind(addr_id)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };

    let destination_address: Option<QuoteAddress> =
        if let Some(addr_id) = quote.destination_address_id {
            sqlx::query_as(
                "SELECT id, street, city, postal_code, floor, elevator FROM addresses WHERE id = $1",
            )
            .bind(addr_id)
            .fetch_optional(&state.db)
            .await?
        } else {
            None
        };

    // Fetch all completed volume estimations for this quote
    let est_rows: Vec<VolumeEstimationDbRow> = sqlx::query_as(
        r#"
        SELECT id, method, total_volume_m3, result_data, source_data
        FROM volume_estimations
        WHERE quote_id = $1 AND status = 'completed'
        ORDER BY created_at
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    // Fetch original video URLs from ALL estimations (including processing/failed)
    let all_video_rows: Vec<(Option<serde_json::Value>,)> = sqlx::query_as(
        "SELECT source_data FROM volume_estimations WHERE quote_id = $1 AND method = 'video' ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut all_source_videos: Vec<String> = Vec::new();
    for (sd,) in &all_video_rows {
        if let Some(key) = sd.as_ref().and_then(|sd| sd.get("video_s3_key")?.as_str()) {
            all_source_videos.push(format!("/api/v1/estimates/images/{key}"));
        }
    }

    let estimation = if est_rows.is_empty() && all_source_videos.is_empty() {
        None
    } else {
        let mut all_items: Vec<EstimationItem> = Vec::new();
        let mut all_source_images: Vec<String> = Vec::new();
        let mut total_volume = 0.0;
        let first_id = est_rows.first().map(|r| r.id).unwrap_or_default();
        let first_method = est_rows.first().map(|r| r.method.clone()).unwrap_or_else(|| "video".to_string());

        for est in &est_rows {
            let vol_est = VolumeEstimationRow {
                result_data: est.result_data.clone(),
                source_data: est.source_data.clone(),
                total_volume_m3: est.total_volume_m3,
                method: est.method.clone(),
            };
            let detected = parse_detected_items(Some(&vol_est));
            let raw_items: Vec<serde_json::Value> = est.result_data
                .as_ref()
                .and_then(|rd| serde_json::from_value::<Vec<serde_json::Value>>(rd.clone()).ok())
                .unwrap_or_default();
            let source_s3_keys: Vec<String> = est.source_data
                .as_ref()
                .and_then(|sd| sd.get("s3_keys")?.as_array().map(|arr| {
                    arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
                }))
                .unwrap_or_default();

            // Collect source images
            for k in &source_s3_keys {
                all_source_images.push(format!("/api/v1/estimates/images/{k}"));
            }

            // source_videos already collected above (all statuses)

            total_volume += est.total_volume_m3.unwrap_or(0.0);

            for (idx, d) in detected.iter().enumerate() {
                let crop_url = d.crop_s3_key.as_ref().map(|k| format!("/api/v1/estimates/images/{k}"));
                let source_image_url = d.bbox_image_index
                    .and_then(|i| source_s3_keys.get(i))
                    .map(|k| format!("/api/v1/estimates/images/{k}"));
                let raw = raw_items.get(idx);
                let seen_in_images = raw
                    .and_then(|r| r.get("seen_in_images")?.as_array().map(|arr| {
                        arr.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect()
                    }));
                let category = raw
                    .and_then(|r| r.get("category")?.as_str().map(String::from));
                let dimensions = raw
                    .and_then(|r| r.get("dimensions").cloned());
                all_items.push(EstimationItem {
                    name: d.german_name.clone().unwrap_or_else(|| d.name.clone()),
                    volume_m3: d.volume_m3,
                    quantity: 1,
                    confidence: d.confidence,
                    crop_url,
                    source_image_url,
                    bbox: d.bbox.clone(),
                    crop_s3_key: d.crop_s3_key.clone(),
                    bbox_image_index: d.bbox_image_index,
                    seen_in_images,
                    category,
                    dimensions,
                });
            }
        }

        Some(EstimationInfo {
            id: first_id,
            method: first_method,
            total_volume_m3: total_volume,
            items: all_items,
            source_images: all_source_images,
            source_videos: all_source_videos,
        })
    };

    // Fetch linked offers
    let offers: Vec<QuoteOffer> = sqlx::query_as(
        r#"
        SELECT id, CAST(ROUND(price_cents * 1.19) AS BIGINT) AS total_brutto_cents,
               status, created_at
        FROM offers WHERE quote_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    // Fetch latest offer's full pricing data for overlay
    let latest_offer: Option<LatestOfferPricing> = {
        #[derive(FromRow)]
        struct LatestOfferRow {
            id: Uuid,
            price_cents: i64,
            persons: Option<i32>,
            hours_estimated: Option<f64>,
            rate_per_hour_cents: Option<i64>,
            line_items_json: Option<serde_json::Value>,
        }
        let row: Option<LatestOfferRow> = sqlx::query_as(
            r#"
            SELECT id, price_cents, persons, hours_estimated, rate_per_hour_cents, line_items_json
            FROM offers WHERE quote_id = $1
            ORDER BY created_at DESC LIMIT 1
            "#,
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;

        row.map(|r| {
            let persons = r.persons.unwrap_or(2);
            let netto = r.price_cents;
            let brutto = (netto as f64 * 1.19).round() as i64;
            let line_items: Vec<OfferLineItemDetail> = r.line_items_json
                .and_then(|json| serde_json::from_value::<Vec<serde_json::Value>>(json).ok())
                .map(|items| {
                    items.iter().map(|item| map_offer_line_item_detail(item, persons)).collect()
                })
                .unwrap_or_default();
            LatestOfferPricing {
                offer_id: r.id,
                persons,
                hours: r.hours_estimated.unwrap_or(0.0),
                rate_cents: r.rate_per_hour_cents.unwrap_or(3000),
                total_netto_cents: netto,
                total_brutto_cents: brutto,
                line_items,
            }
        })
    };

    // Extract customer message from notes (non-service parts)
    let customer_message = extract_customer_message(quote.notes.as_deref());

    Ok(Json(EnrichedQuote {
        quote: QuoteInfo {
            id: quote.id,
            estimated_volume_m3: quote.estimated_volume_m3,
            distance_km: quote.distance_km.unwrap_or(0.0),
            notes: quote.notes,
            status: quote.status.as_str().to_string(),
            customer_message,
            created_at: quote.created_at,
        },
        customer,
        origin_address,
        destination_address,
        estimation,
        offers,
        latest_offer,
    }))
}

/// Extract free-text customer remarks from quote notes, stripping known service keywords.
///
/// **Caller**: `get_quote` — populates `QuoteInfo.customer_message` for display in the
/// frontend quote detail card.
/// **Why**: `quotes.notes` mixes service flags ("Halteverbot Auszug") with actual customer
/// remarks. This helper filters out the known service strings so only the human-readable
/// message is returned.
///
/// # Parameters
/// - `notes` — raw `quotes.notes` value; returns `None` when absent
///
/// # Returns
/// `Some(String)` with the joined non-service note parts, or `None` when nothing remains.
fn extract_customer_message(notes: Option<&str>) -> Option<String> {
    let notes = notes?;
    let known = [
        "halteverbot auszug", "halteverbot einzug", "verpackungsservice",
        "einpackservice", "montage", "demontage", "einlagerung", "entsorgung",
    ];
    let known_prefixes = ["auszug:", "einzug:"];

    let parts: Vec<&str> = notes
        .split(", ")
        .filter(|part| {
            let lower = part.trim().to_lowercase();
            !known.iter().any(|s| lower == *s)
                && !known_prefixes.iter().any(|p| lower.starts_with(p))
        })
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// `GET /api/v1/quotes` — List quotes with optional filtering by status or customer.
///
/// **Caller**: Axum router / external API consumers.
/// **Why**: Paginated quote listing for API consumers and simple integrations. Does not
/// include joined customer or address data (use `get_quote` for enriched detail).
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `status`, `customer_id`, `limit` (max 100, default 50), `offset`
///
/// # Returns
/// `200 OK` with `QuoteListResponse` containing `quotes`, `total`, `limit`, `offset`.
async fn list_quotes(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListQuotesQuery>,
) -> Result<Json<QuoteListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);

    let rows: Vec<QuoteRow> = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes
        WHERE ($1::uuid IS NULL OR customer_id = $1)
          AND ($2::text IS NULL OR status = $2)
        ORDER BY created_at DESC
        LIMIT $3 OFFSET $4
        "#
    )
    .bind(query.customer_id)
    .bind(&query.status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM quotes
        WHERE ($1::uuid IS NULL OR customer_id = $1)
          AND ($2::text IS NULL OR status = $2)
        "#,
    )
    .bind(query.customer_id)
    .bind(&query.status)
    .fetch_one(&state.db)
    .await?;

    let quotes: Vec<Quote> = rows.into_iter().map(Quote::from).collect();

    Ok(Json(QuoteListResponse {
        quotes,
        total: total.0,
        limit,
        offset,
    }))
}

/// `PATCH /api/v1/quotes/{id}` — Partially update a quote's fields.
///
/// **Caller**: Axum router / admin dashboard and external API consumers.
/// **Why**: Allows updating any subset of mutable quote fields (addresses, status,
/// volume, distance, date, notes) via a COALESCE-based partial update.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — quote UUID path parameter
/// - `request` — `UpdateQuote` JSON body with optional fields; omitted fields are unchanged
///
/// # Returns
/// `200 OK` with the updated `Quote` JSON.
///
/// # Errors
/// - `404` if no quote with the given ID exists
async fn update_quote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateQuote>,
) -> Result<Json<Quote>, ApiError> {
    let now = chrono::Utc::now();

    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        UPDATE quotes SET
            origin_address_id = COALESCE($2, origin_address_id),
            destination_address_id = COALESCE($3, destination_address_id),
            status = COALESCE($4, status),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            preferred_date = COALESCE($7, preferred_date),
            notes = COALESCE($8, notes),
            updated_at = $9
        WHERE id = $1
        RETURNING id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        "#
    )
    .bind(id)
    .bind(request.origin_address_id)
    .bind(request.destination_address_id)
    .bind(request.status.map(|s| s.as_str()))
    .bind(request.estimated_volume_m3)
    .bind(request.distance_km)
    .bind(request.preferred_date)
    .bind(&request.notes)
    .bind(now)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Quote {id} not found")))?;
    Ok(Json(Quote::from(row)))
}

/// `DELETE /api/v1/quotes/{id}` — Soft-delete a quote by setting its status to "cancelled".
///
/// **Caller**: Axum router / external API consumers.
/// **Why**: Quotes are not physically deleted so that linked offers, estimations, and email
/// threads remain auditable. Setting status to "cancelled" effectively removes the quote
/// from active pipelines.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — quote UUID path parameter
///
/// # Returns
/// `200 OK` with the updated `Quote` JSON (status = "cancelled").
///
/// # Errors
/// - `404` if no quote with the given ID exists
async fn soft_delete_quote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Quote>, ApiError> {
    let now = chrono::Utc::now();

    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        UPDATE quotes SET status = 'cancelled', updated_at = $2
        WHERE id = $1
        RETURNING id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(now)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Quote {id} not found")))?;
    Ok(Json(Quote::from(row)))
}

// --- Update Estimation Items ---

#[derive(Debug, Deserialize)]
struct UpdateEstimationItemsRequest {
    items: Vec<UpdateEstimationItem>,
}

#[derive(Debug, Deserialize, Serialize)]
struct UpdateEstimationItem {
    name: String,
    volume_m3: f64,
    quantity: u32,
    confidence: f64,
    // Preserved fields from original detection
    #[serde(default)]
    crop_s3_key: Option<String>,
    #[serde(default)]
    bbox: Option<Vec<f64>>,
    #[serde(default)]
    bbox_image_index: Option<usize>,
    #[serde(default)]
    seen_in_images: Option<Vec<usize>>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    dimensions: Option<serde_json::Value>,
}

/// `PUT /api/v1/quotes/{id}/estimation-items` — Replace the detected items on the latest
/// volume estimation and recalculate the quote's total volume.
///
/// **Caller**: Axum router / admin dashboard item editor (after the user edits item quantities
/// or removes false detections from the vision pipeline output).
/// **Why**: The ML/LLM pipeline may detect duplicate or incorrect items. This endpoint lets
/// Alex correct the item list before regenerating the offer. It overwrites `result_data` and
/// `total_volume_m3` on the most recent `volume_estimations` row, then updates
/// `quotes.estimated_volume_m3` with the new sum.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `quote_id` — quote UUID path parameter
/// - `request` — list of `UpdateEstimationItem` (name, volume_m3, quantity, confidence, +
///   preserved vision metadata fields for round-trip fidelity)
///
/// # Returns
/// `200 OK` with updated `EstimationInfo` including new `total_volume_m3` and item list.
///
/// # Errors
/// - `404` if no estimation exists for the given quote
/// - `500` on serialization or DB failures
async fn update_estimation_items(
    State(state): State<Arc<AppState>>,
    Path(quote_id): Path<Uuid>,
    Json(request): Json<UpdateEstimationItemsRequest>,
) -> Result<Json<EstimationInfo>, ApiError> {
    // Get latest estimation for this quote
    let est: Option<(Uuid, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT id, method, source_data FROM volume_estimations WHERE quote_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await?;

    let (estimation_id, estimation_method, est_source_data) =
        est.ok_or_else(|| ApiError::NotFound("Keine Schaetzung fuer diese Anfrage".into()))?;

    // Calculate new total volume
    let total_volume: f64 = request
        .items
        .iter()
        .map(|item| item.volume_m3 * item.quantity as f64)
        .sum();

    // Serialize items to JSON for result_data
    let result_data = serde_json::to_value(&request.items)
        .map_err(|e| ApiError::Internal(format!("Serialisierung fehlgeschlagen: {e}")))?;

    let now = chrono::Utc::now();

    // Update volume estimation
    sqlx::query(
        "UPDATE volume_estimations SET result_data = $1, total_volume_m3 = $2 WHERE id = $3",
    )
    .bind(&result_data)
    .bind(total_volume)
    .bind(estimation_id)
    .execute(&state.db)
    .await?;

    // Update quote volume
    sqlx::query("UPDATE quotes SET estimated_volume_m3 = $1, updated_at = $2 WHERE id = $3")
        .bind(total_volume)
        .bind(now)
        .bind(quote_id)
        .execute(&state.db)
        .await?;

    // Build response
    let items: Vec<EstimationItem> = request
        .items
        .iter()
        .map(|item| {
            let crop_url = item
                .crop_s3_key
                .as_ref()
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            EstimationItem {
                name: item.name.clone(),
                volume_m3: item.volume_m3,
                quantity: item.quantity,
                confidence: item.confidence,
                crop_url,
                source_image_url: None,
                bbox: item.bbox.clone(),
                crop_s3_key: item.crop_s3_key.clone(),
                bbox_image_index: item.bbox_image_index,
                seen_in_images: item.seen_in_images.clone(),
                category: item.category.clone(),
                dimensions: item.dimensions.clone(),
            }
        })
        .collect();

    let source_images: Vec<String> = est_source_data
        .as_ref()
        .and_then(|sd| sd.get("s3_keys")?.as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|k| format!("/api/v1/estimates/images/{k}")))
                .collect()
        }))
        .unwrap_or_default();

    Ok(Json(EstimationInfo {
        id: estimation_id,
        method: estimation_method,
        total_volume_m3: total_volume,
        items,
        source_images,
        source_videos: Vec::new(),
    }))
}

/// Convert a raw `line_items_json` entry from the DB into a typed `OfferLineItemDetail`.
///
/// **Caller**: `get_quote` (latest offer pricing overlay) and `admin::get_quote_detail`.
/// **Why**: `offers.line_items_json` stores the serialized `Vec<OfferLineItem>` from offer
/// generation. At read time we need typed structs for the frontend. Three pricing modes
/// are supported to match how the XLSX SUM(G31:G42) computes totals:
/// - `flat_total` key present → `total = flat_total` (Fahrkostenpauschale lump sum)
/// - `is_labor = true` → `total = quantity × unit_price × persons`
/// - otherwise → `total = quantity × unit_price`
///
/// # Parameters
/// - `item` — one element from the `line_items_json` array
/// - `persons` — worker count from `offers.persons`; used for labor total calculation
///
/// # Returns
/// `OfferLineItemDetail` with cents-based totals for frontend display.
pub(crate) fn map_offer_line_item_detail(item: &serde_json::Value, persons: i32) -> OfferLineItemDetail {
    let label = item.get("description").and_then(|d| d.as_str()).unwrap_or("Sonstiges").to_string();
    let remark = item.get("remark").and_then(|r| r.as_str()).map(String::from);
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
    OfferLineItemDetail { label, remark, quantity, unit_price_cents, total_cents, is_labor }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- map_offer_line_item_detail ---
    // These tests would have caught the flat_total bug: before the fix,
    // flat_total was ignored and Fahrkostenpauschale always showed total_cents = 0.

    #[test]
    fn flat_total_overrides_quantity_times_price() {
        // Fahrkostenpauschale: quantity=0, unit_price=0, flat_total=45.0 → total=€45
        let item = json!({
            "description": "Fahrkostenpauschale",
            "quantity": 0.0,
            "unit_price": 0.0,
            "is_labor": false,
            "flat_total": 45.0
        });
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.total_cents, 4500, "flat_total 45.0 must yield 4500 cents, not quantity*unit_price");
        assert_eq!(detail.unit_price_cents, 0, "unit_price_cents should remain 0 (raw storage)");
        assert!((detail.quantity - 0.0).abs() < 0.001, "quantity should remain 0 (raw storage)");
    }

    #[test]
    fn flat_total_zero_yields_zero_total_not_quantity_times_price() {
        // Nürnbergerversicherung: quantity=1, unit_price=100, flat_total=0 → total=€0
        // Before fix: total would have been 1 * 100 = €100 (wrong)
        let item = json!({
            "description": "Nürnbergerversicherung",
            "quantity": 1.0,
            "unit_price": 100.0,
            "is_labor": false,
            "flat_total": 0.0
        });
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.total_cents, 0, "flat_total 0.0 must be 0, not quantity*unit_price=10000");
    }

    #[test]
    fn labor_item_multiplied_by_persons() {
        // 3 workers × 8h × €35/h = €840
        let item = json!({
            "description": "3 Umzugshelfer",
            "quantity": 8.0,
            "unit_price": 35.0,
            "is_labor": true
        });
        let detail = map_offer_line_item_detail(&item, 3);
        assert_eq!(detail.total_cents, 84000, "labor: 8h × €35 × 3 persons = 84000 cents");
    }

    #[test]
    fn regular_item_ignores_persons_count() {
        // Halteverbotszone: 2 zones × €100 = €200, regardless of persons
        let item = json!({
            "description": "Halteverbotszone",
            "quantity": 2.0,
            "unit_price": 100.0,
            "is_labor": false
        });
        let detail = map_offer_line_item_detail(&item, 5); // 5 persons — must NOT multiply
        assert_eq!(detail.total_cents, 20000, "regular item: 2 × €100 = 20000, persons must not factor in");
    }

    #[test]
    fn remark_is_preserved() {
        let item = json!({
            "description": "Halteverbotszone",
            "quantity": 1.0,
            "unit_price": 100.0,
            "is_labor": false,
            "remark": "Beladestelle + Entladestelle"
        });
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.remark.as_deref(), Some("Beladestelle + Entladestelle"));
    }

    #[test]
    fn missing_remark_field_is_none() {
        let item = json!({"description": "Demontage", "quantity": 1.0, "unit_price": 50.0, "is_labor": false});
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.remark, None);
    }

    #[test]
    fn no_flat_total_key_falls_back_to_labor_formula() {
        // Ensure absence of flat_total key (not just null) uses the labor formula
        let item = json!({"description": "2 Umzugshelfer", "quantity": 4.0, "unit_price": 30.0, "is_labor": true});
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.total_cents, 24000, "4h × €30 × 2 persons = 24000 cents");
    }

    #[test]
    fn flat_total_rounding() {
        // 42.37 km × €1.00/km = €42.37 → 4237 cents (not 4236 due to float)
        let item = json!({
            "description": "Fahrkostenpauschale",
            "quantity": 0.0,
            "unit_price": 0.0,
            "is_labor": false,
            "flat_total": 42.37
        });
        let detail = map_offer_line_item_detail(&item, 2);
        assert_eq!(detail.total_cents, 4237);
    }
}
