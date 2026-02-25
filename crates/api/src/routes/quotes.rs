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
use aust_core::models::{CreateQuote, Quote, QuoteStatus, UpdateQuote};

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

#[derive(Debug, FromRow)]
struct QuoteRow {
    id: Uuid,
    customer_id: Uuid,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
    status: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    preferred_date: Option<chrono::DateTime<chrono::Utc>>,
    notes: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<QuoteRow> for Quote {
    fn from(row: QuoteRow) -> Self {
        let status = match row.status.as_str() {
            "pending" => QuoteStatus::Pending,
            "info_requested" => QuoteStatus::InfoRequested,
            "volume_estimated" => QuoteStatus::VolumeEstimated,
            "offer_generated" => QuoteStatus::OfferGenerated,
            "offer_sent" => QuoteStatus::OfferSent,
            "accepted" => QuoteStatus::Accepted,
            "rejected" => QuoteStatus::Rejected,
            "expired" => QuoteStatus::Expired,
            "cancelled" => QuoteStatus::Cancelled,
            "done" => QuoteStatus::Done,
            "paid" => QuoteStatus::Paid,
            _ => QuoteStatus::Pending,
        };

        Quote {
            id: row.id,
            customer_id: row.customer_id,
            origin_address_id: row.origin_address_id,
            destination_address_id: row.destination_address_id,
            status,
            estimated_volume_m3: row.estimated_volume_m3,
            distance_km: row.distance_km,
            preferred_date: row.preferred_date,
            notes: row.notes,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

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

#[derive(Debug, FromRow)]
struct VolumeEstimationDbRow {
    id: Uuid,
    method: String,
    total_volume_m3: Option<f64>,
    result_data: Option<serde_json::Value>,
    source_data: Option<serde_json::Value>,
}

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

    let estimation = if est_rows.is_empty() {
        None
    } else {
        let mut all_items: Vec<EstimationItem> = Vec::new();
        let mut all_source_images: Vec<String> = Vec::new();
        let mut all_source_videos: Vec<String> = Vec::new();
        let mut total_volume = 0.0;
        let first_id = est_rows[0].id;
        let first_method = est_rows[0].method.clone();

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

            // Collect source videos
            if let Some(video_key) = est.source_data.as_ref().and_then(|sd| sd.get("video_s3_key")?.as_str()) {
                all_source_videos.push(format!("/api/v1/estimates/images/{video_key}"));
            }

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
    }))
}

/// Extract non-service text from notes as "customer message"
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

async fn update_estimation_items(
    State(state): State<Arc<AppState>>,
    Path(quote_id): Path<Uuid>,
    Json(request): Json<UpdateEstimationItemsRequest>,
) -> Result<Json<EstimationInfo>, ApiError> {
    // Get latest estimation for this quote
    let est: Option<(Uuid, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT id, source_data FROM volume_estimations WHERE quote_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await?;

    let (estimation_id, est_source_data) =
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
        method: "depth_sensor".to_string(),
        total_volume_m3: total_volume,
        items,
        source_images,
        source_videos: Vec::new(),
    }))
}
