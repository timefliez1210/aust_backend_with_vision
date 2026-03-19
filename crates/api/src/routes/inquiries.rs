//! Inquiry endpoints — main CRUD resource for the unified inquiry lifecycle.
//!
//! Also includes submission endpoints for photo webapp (Source C) and mobile app (Source D).

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Extension, Json, Router,
};
use bytes::Bytes;
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::routes::offers::{
    build_offer_with_overrides, OfferOverrides,
};
use crate::services::db::{insert_estimation_no_return, update_quote_volume};
use crate::services::inquiry_builder;
use crate::{orchestrator, services, ApiError, AppState};
use aust_core::models::{
    EstimationMethod, InquiryListItem, InquiryResponse as InquiryResponseModel,
    InquiryStatus, Offer, Services, TokenClaims,
};
use aust_llm_providers::LlmMessage;
use aust_offer_generator::OfferLineItem;
use aust_storage::StorageProvider;

// ---------------------------------------------------------------------------
// Router constructors
// ---------------------------------------------------------------------------

/// Protected inquiry routes (require admin JWT).
///
/// **Caller**: `crates/api/src/routes/mod.rs` protected route tree.
/// **Why**: Main CRUD resource for the inquiry lifecycle — create, list, detail,
///          update, delete, estimation, offer generation, PDF download, and emails.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(create_inquiry).get(list_inquiries))
        .route(
            "/{id}",
            get(get_inquiry).patch(update_inquiry).delete(delete_inquiry),
        )
        .route("/{id}/pdf", get(get_inquiry_pdf))
        .route("/{id}/items", put(update_inquiry_items))
        .route("/{id}/estimate/depth", post(trigger_estimate_upload))
        .route("/{id}/estimate/video", post(trigger_video_upload))
        .route("/{id}/estimate/{method}", post(trigger_estimate))
        .route("/{id}/generate-offer", post(generate_inquiry_offer))
        .route("/{id}/emails", get(get_inquiry_emails))
        .route(
            "/{id}/employees",
            get(list_inquiry_employees).post(assign_employee),
        )
        .route(
            "/{id}/employees/{emp_id}",
            axum::routing::patch(update_assignment)
                .delete(remove_assignment),
        )
        .merge(super::invoices::router())
}

/// Public submission routes (no auth required).
///
/// **Caller**: `crates/api/src/routes/mod.rs` public route tree.
/// **Why**: Photo webapp and mobile app endpoints accept multipart uploads from
///          unauthenticated end-users.
pub fn submit_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/photo", post(photo_inquiry))
        .route("/mobile", post(mobile_inquiry))
        .route("/video", post(video_inquiry))
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AddressInput {
    street: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateInquiryRequest {
    customer_id: Uuid,
    origin: Option<AddressInput>,
    destination: Option<AddressInput>,
    services: Option<Services>,
    notes: Option<String>,
    preferred_date: Option<String>,
    distance_km: Option<f64>,
    estimated_volume_m3: Option<f64>,
    items_list: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListInquiriesQuery {
    status: Option<String>,
    search: Option<String>,
    has_offer: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct InquiryListResponse {
    inquiries: Vec<InquiryListItem>,
    total: i64,
    limit: i64,
    offset: i64,
}

#[derive(Debug, Deserialize)]
struct UpdateInquiryRequest {
    status: Option<String>,
    notes: Option<String>,
    services: Option<Services>,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    preferred_date: Option<String>,
    scheduled_date: Option<String>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct GenerateOfferRequest {
    valid_days: Option<i64>,
    #[serde(default)]
    price_cents_netto: Option<i64>,
    #[serde(default)]
    persons: Option<u32>,
    #[serde(default)]
    hours: Option<f64>,
    #[serde(default)]
    rate: Option<f64>,
    #[serde(default)]
    line_items: Option<Vec<GenerateLineItem>>,
    /// Explicit Fahrkostenpauschale flat total in €. When set, overrides ORS calculation and
    /// is persisted so future regenerations also use it. Send `null` to clear a stored override.
    #[serde(default)]
    fahrt_flat_total: Option<f64>,
    /// When true, clears any stored Fahrkostenpauschale override so ORS recalculates it.
    #[serde(default)]
    fahrt_reset: bool,
}

#[derive(Debug, Deserialize)]
struct GenerateLineItem {
    description: String,
    quantity: f64,
    unit_price: f64,
    #[serde(default)]
    remark: Option<String>,
}

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
    #[serde(default = "default_true")]
    is_moveable: bool,
    #[serde(default)]
    packs_into_boxes: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct EstimationDetail {
    id: Uuid,
    method: String,
    total_volume_m3: f64,
    items: Vec<EstimationItemResponse>,
    source_images: Vec<String>,
    source_videos: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EstimationItemResponse {
    name: String,
    volume_m3: f64,
    quantity: u32,
    confidence: f64,
    crop_url: Option<String>,
    source_image_url: Option<String>,
    bbox: Option<Vec<f64>>,
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
    is_moveable: bool,
    packs_into_boxes: bool,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
struct EmailThreadSummary {
    id: Uuid,
    subject: Option<String>,
    last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    message_count: i64,
}

// ---------------------------------------------------------------------------
// Protected CRUD handlers
// ---------------------------------------------------------------------------

/// `POST /api/v1/inquiries` -- Create a new inquiry from JSON body.
///
/// **Caller**: Admin dashboard, external API consumers.
/// **Why**: Manual inquiry creation with customer upsert by email, address creation,
///          and optional services JSONB.
async fn create_inquiry(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateInquiryRequest>,
) -> Result<(StatusCode, Json<InquiryResponseModel>), ApiError> {
    let now = chrono::Utc::now();

    // Verify customer exists
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM customers WHERE id = $1)")
        .bind(request.customer_id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::Validation("Kunde nicht gefunden".into()));
    }

    // Create origin address if provided
    let origin_id = if let Some(ref addr) = request.origin {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&addr.postal_code)
            .bind(&addr.floor)
            .bind(addr.elevator)
            .fetch_one(&state.db)
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Create destination address if provided
    let dest_id = if let Some(ref addr) = request.destination {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&addr.postal_code)
            .bind(&addr.floor)
            .bind(addr.elevator)
            .fetch_one(&state.db)
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    let preferred_date = request
        .preferred_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    let services_json = serde_json::to_value(request.services.unwrap_or_default())
        .unwrap_or(serde_json::json!({}));

    // If volume is provided manually, start as estimated; otherwise pending
    let initial_status = if request.estimated_volume_m3.is_some() {
        InquiryStatus::Estimated
    } else {
        InquiryStatus::Pending
    };

    let inquiry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, estimated_volume_m3, preferred_date, notes, services,
                           distance_km, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)
        "#,
    )
    .bind(inquiry_id)
    .bind(request.customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(initial_status.as_str())
    .bind(request.estimated_volume_m3)
    .bind(preferred_date)
    .bind(&request.notes)
    .bind(&services_json)
    .bind(request.distance_km)
    .bind("admin_dashboard")
    .bind(now)
    .execute(&state.db)
    .await?;

    // Store manual volume estimation if provided
    if let Some(volume) = request.estimated_volume_m3 {
        let estimation_id = Uuid::now_v7();
        let source_data = serde_json::json!({"source": "admin_dashboard"});
        let items_text = request.items_list.as_deref().unwrap_or("");
        let result_data = serde_json::json!({"items_text": items_text});
        insert_estimation_no_return(
            &state.db,
            estimation_id,
            inquiry_id,
            "inventory",
            &source_data,
            Some(&result_data),
            volume,
            0.9,
            now,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Estimation insert failed: {e}")))?;
    }

    let response = inquiry_builder::build_inquiry_response(&state.db, inquiry_id).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/v1/inquiries` -- List inquiries with optional filters.
///
/// **Caller**: Admin dashboard inquiry list page.
/// **Why**: Paginated list with search, status filter, and offer filter.
async fn list_inquiries(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListInquiriesQuery>,
) -> Result<Json<InquiryListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);

    let (inquiries, total) = inquiry_builder::build_inquiry_list(
        &state.db,
        query.status.as_deref(),
        query.search.as_deref(),
        query.has_offer,
        limit,
        offset,
    )
    .await?;

    Ok(Json(InquiryListResponse {
        inquiries,
        total,
        limit,
        offset,
    }))
}

/// `GET /api/v1/inquiries/{id}` -- Get enriched inquiry detail.
///
/// **Caller**: Admin dashboard inquiry detail page.
/// **Why**: Single source of truth for inquiry detail via inquiry_builder.
async fn get_inquiry(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InquiryResponseModel>, ApiError> {
    let response = inquiry_builder::build_inquiry_response(&state.db, id).await?;
    Ok(Json(response))
}

/// `PATCH /api/v1/inquiries/{id}` -- Update inquiry fields and/or transition status.
///
/// **Caller**: Admin dashboard, API consumers.
/// **Why**: Partial update with status transition validation via `can_transition_to()`.
async fn update_inquiry(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateInquiryRequest>,
) -> Result<Json<InquiryResponseModel>, ApiError> {
    let now = chrono::Utc::now();

    // If status transition requested, validate it
    if let Some(ref new_status_str) = request.status {
        let new_status: InquiryStatus = new_status_str
            .parse()
            .map_err(|e: String| ApiError::BadRequest(e))?;

        let (current_status_str,): (String,) =
            sqlx::query_as("SELECT status FROM inquiries WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(|| ApiError::NotFound(format!("Inquiry {id} not found")))?;

        let current_status: InquiryStatus = current_status_str.parse().unwrap_or_default();

        if !current_status.can_transition_to(&new_status) {
            return Err(ApiError::BadRequest(format!(
                "Ungültiger Statusübergang: {} -> {}",
                current_status, new_status
            )));
        }
    }

    // Parse preferred date if provided
    let preferred_date = request.preferred_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(10, 0, 0))
            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
    });

    // Serialize services if provided
    let services_json = request
        .services
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());

    let scheduled_date = request.scheduled_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    });

    sqlx::query(
        r#"
        UPDATE inquiries SET
            status = COALESCE($2, status),
            notes = COALESCE($3, notes),
            services = COALESCE($4, services),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            preferred_date = COALESCE($7, preferred_date),
            scheduled_date = CASE WHEN $7 IS NOT NULL THEN NULL WHEN $11 IS NOT NULL THEN $11 ELSE scheduled_date END,
            start_time = COALESCE($8, start_time),
            end_time = COALESCE($9, end_time),
            origin_address_id = COALESCE($10, origin_address_id),
            destination_address_id = COALESCE($12, destination_address_id),
            updated_at = $13
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(request.status.as_deref())
    .bind(&request.notes)
    .bind(&services_json)
    .bind(request.estimated_volume_m3)
    .bind(request.distance_km)
    .bind(preferred_date)
    .bind(request.start_time)
    .bind(request.end_time)
    .bind(request.origin_address_id)
    .bind(scheduled_date)
    .bind(request.destination_address_id)
    .bind(now)
    .execute(&state.db)
    .await?;

    let response = inquiry_builder::build_inquiry_response(&state.db, id).await?;
    Ok(Json(response))
}

/// `DELETE /api/v1/inquiries/{id}` -- Hard-delete inquiry and all related records.
///
/// **Caller**: Admin dashboard trash-bin button.
/// **Why**: Permanently removes the inquiry. FK constraints with CASCADE DELETE
/// handle offers, estimations, items, email threads, and bookings automatically.
async fn delete_inquiry(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if !claims.role.is_admin() {
        return Err(ApiError::Forbidden(
            "Diese Aktion erfordert Administrator-Berechtigungen".into(),
        ));
    }
    let rows_affected = sqlx::query("DELETE FROM inquiries WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();

    if rows_affected == 0 {
        return Err(ApiError::NotFound(format!("Inquiry {id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/inquiries/{id}/pdf` -- Download latest active offer PDF.
///
/// **Caller**: Admin dashboard PDF download button.
/// **Why**: Convenience endpoint so clients don't need to know the offer ID.
async fn get_inquiry_pdf(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    // Find latest active offer for this inquiry
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, pdf_storage_key FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (offer_id, storage_key) =
        row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    let storage_key = storage_key
        .ok_or_else(|| ApiError::NotFound("Angebot hat keine generierte Datei".into()))?;

    let file_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| ApiError::Internal(format!("Download fehlgeschlagen: {e}")))?;

    let (content_type, ext) = if storage_key.ends_with(".pdf") {
        ("application/pdf", "pdf")
    } else {
        (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        )
    };
    let filename = format!("Angebot-{}.{ext}", offer_id);

    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        file_bytes,
    ))
}

/// `PUT /api/v1/inquiries/{id}/items` -- Replace detected items on latest estimation.
///
/// **Caller**: Admin dashboard item editor.
/// **Why**: ML/LLM pipeline may produce duplicates or errors. This lets the admin
///          correct items before regenerating the offer.
async fn update_inquiry_items(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
    Json(request): Json<UpdateEstimationItemsRequest>,
) -> Result<Json<EstimationDetail>, ApiError> {
    // Get latest estimation for this inquiry
    let est: Option<(Uuid, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT id, method, source_data FROM volume_estimations WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
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

    // Update inquiry volume
    sqlx::query("UPDATE inquiries SET estimated_volume_m3 = $1, updated_at = $2 WHERE id = $3")
        .bind(total_volume)
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    // Build response
    let items: Vec<EstimationItemResponse> = request
        .items
        .iter()
        .map(|item| {
            let crop_url = item
                .crop_s3_key
                .as_ref()
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            EstimationItemResponse {
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
                is_moveable: item.is_moveable,
                packs_into_boxes: item.packs_into_boxes,
            }
        })
        .collect();

    let source_images: Vec<String> = est_source_data
        .as_ref()
        .and_then(|sd| {
            sd.get("s3_keys")?.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|k| format!("/api/v1/estimates/images/{k}")))
                    .collect()
            })
        })
        .unwrap_or_default();

    Ok(Json(EstimationDetail {
        id: estimation_id,
        method: estimation_method,
        total_volume_m3: total_volume,
        items,
        source_images,
        source_videos: Vec::new(),
    }))
}

/// `POST /api/v1/inquiries/{id}/estimate/{method}` -- Trigger estimation.
///
/// **Caller**: Admin dashboard re-estimation buttons.
/// **Why**: Allows triggering vision/inventory/depth/video estimation from the
///          inquiry detail page without going through separate estimate endpoints.
async fn trigger_estimate(
    State(state): State<Arc<AppState>>,
    Path((inquiry_id, method)): Path<(Uuid, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Verify inquiry exists
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM inquiries WHERE id = $1")
            .bind(inquiry_id)
            .fetch_optional(&state.db)
            .await?;

    if exists.is_none() {
        return Err(ApiError::NotFound(format!(
            "Inquiry {inquiry_id} not found"
        )));
    }

    match method.as_str() {
        "inventory" => {
            let body = body.ok_or_else(|| {
                ApiError::BadRequest("JSON body with inventory data required".into())
            })?;
            let items = body
                .get("items")
                .and_then(|v| v.as_array())
                .ok_or_else(|| ApiError::BadRequest("items array required".into()))?;

            let total_volume: f64 = items
                .iter()
                .map(|item| {
                    let qty = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(1.0);
                    let vol = item.get("volume_m3").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    qty * vol
                })
                .sum();

            let estimation_id = Uuid::now_v7();
            let now = chrono::Utc::now();
            let source_data = serde_json::json!({"source": "admin_dashboard"});

            insert_estimation_no_return(
                &state.db,
                estimation_id,
                inquiry_id,
                "inventory",
                &source_data,
                Some(&serde_json::Value::Array(items.clone())),
                total_volume,
                0.9,
                now,
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Estimation insert failed: {e}")))?;

            update_quote_volume(&state.db, inquiry_id, total_volume, "estimated", now)
                .await
                .map_err(|e| ApiError::Internal(format!("Volume update failed: {e}")))?;

            Ok((
                StatusCode::OK,
                Json(serde_json::json!({
                    "estimation_id": estimation_id,
                    "method": "inventory",
                    "total_volume_m3": total_volume,
                    "status": "completed"
                })),
            ))
        }
        "vision" | "depth" | "video" => {
            // These methods need multipart image data — return guidance
            Err(ApiError::BadRequest(format!(
                "Methode '{method}' erfordert Multipart-Upload. Verwenden Sie POST /api/v1/submit/photo oder POST /api/v1/submit/mobile.",
                method = method
            )))
        }
        _ => Err(ApiError::BadRequest(format!(
            "Unbekannte Methode: {method}. Erlaubt: vision, depth, video, inventory"
        ))),
    }
}

/// `POST /api/v1/inquiries/{id}/generate-offer` -- Generate/regenerate offer.
///
/// **Caller**: Admin dashboard "Angebot erstellen" button.
/// **Why**: Central offer generation entry point from the inquiry detail page.
///          Reuses existing active offer (UPDATE in-place) to avoid unique constraint violation.
///          Also spawns a background task to generate a personalised LLM email draft so the
///          admin can send the offer with one click from the email thread section.
async fn generate_inquiry_offer(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
    body: Option<Json<GenerateOfferRequest>>,
) -> Result<Json<Offer>, ApiError> {
    let request = body.map(|b| b.0).unwrap_or(GenerateOfferRequest {
        valid_days: None,
        price_cents_netto: None,
        persons: None,
        hours: None,
        rate: None,
        line_items: None,
        fahrt_flat_total: None,
        fahrt_reset: false,
    });

    // Reuse any existing active offer so we UPDATE in-place
    let existing_offer_id: Option<Uuid> = sqlx::query_as(
        "SELECT id FROM offers WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled') LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?
    .map(|(id,): (Uuid,)| id);

    // fahrt_flat_total and fahrt_reset are passed straight through to build_offer_with_overrides,
    // which is now the single place responsible for the full resolution order:
    // new admin value → line_items value → stored DB override → ORS calculation.
    let overrides = OfferOverrides {
        price_cents: request.price_cents_netto,
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
        existing_offer_id,
        fahrt_flat_total: request.fahrt_flat_total,
        fahrt_reset: request.fahrt_reset,
    };

    let result = build_offer_with_overrides(
        &state.db,
        &*state.storage,
        &state.config,
        inquiry_id,
        request.valid_days,
        &overrides,
    )
    .await?;

    // Generate personalised email draft in the background (non-blocking)
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            generate_offer_email_draft(&state, inquiry_id).await;
        });
    }

    Ok(Json(result.offer))
}

/// Generate a personalised LLM offer email draft and store it as a `draft` `email_message`.
///
/// **Caller**: `generate_inquiry_offer` — spawned as a background task after PDF generation.
/// **Why**: Prepares a ready-to-send email body so Alex can review and dispatch with one click
///          via the existing draft send mechanism. Re-runs on every offer regeneration,
///          discarding any previous LLM draft for the same thread to avoid stale copies.
///
/// # Parameters
/// - `state` — shared AppState (DB, LLM, email config)
/// - `inquiry_id` — the inquiry whose offer was just generated
async fn generate_offer_email_draft(state: &AppState, inquiry_id: Uuid) {
    // Fetch customer name, email, origin/destination city for the LLM prompt
    let row: Option<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT c.name, c.email, a_orig.city, a_dest.city
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses a_orig ON q.origin_address_id = a_orig.id
        LEFT JOIN addresses a_dest ON q.destination_address_id = a_dest.id
        WHERE q.id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let Some((name, Some(email), origin_city, dest_city)) = row else {
        return;
    };

    let origin = origin_city.as_deref().unwrap_or("dem Abholort");
    let dest = dest_city.as_deref().unwrap_or("dem Zielort");

    // Ask LLM for a personalised German email body; fall back to a static template on error
    let prompt = format!(
        "Schreibe eine professionelle, freundliche E-Mail auf Deutsch für einen Umzugskunden. \
         Anrede: Sehr geehrte(r) {name}. Umzug von {origin} nach {dest}. \
         Die E-Mail soll das beigefügte Angebot kurz vorstellen, Professionalität und \
         Zuverlässigkeit betonen und zur Kontaktaufnahme einladen. \
         Nur den Textkörper, keinen Betreff. Maximal 5 Sätze. \
         Unterschrift: 'Mit freundlichen Grüßen,\\nIhr AUST-Umzüge-Team'"
    );
    let body = match state.llm.complete(&[LlmMessage::user(prompt)]).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("LLM offer email generation failed ({e}), using fallback");
            format!(
                "Sehr geehrte(r) {name},\n\n\
                 anbei erhalten Sie unser Angebot für Ihren Umzug von {origin} nach {dest}.\n\n\
                 Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
                 Mit freundlichen Grüßen,\nIhr AUST-Umzüge-Team"
            )
        }
    };

    // Find or create the email thread for this inquiry
    let thread_id = find_or_create_inquiry_thread(state, inquiry_id).await;
    if thread_id.is_nil() {
        return;
    }

    // Discard any previous LLM offer draft in this thread (stale after regeneration)
    let _ = sqlx::query(
        "UPDATE email_messages SET status = 'discarded' \
         WHERE thread_id = $1 AND status = 'draft' AND llm_generated = true",
    )
    .bind(thread_id)
    .execute(&state.db)
    .await;

    // Insert the new draft
    let _ = sqlx::query(
        r#"
        INSERT INTO email_messages
            (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, 'Ihr Umzugsangebot', $5, true, 'draft', NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(thread_id)
    .bind(&state.config.email.from_address)
    .bind(&email)
    .bind(&body)
    .execute(&state.db)
    .await;
}

/// Find the most recent email thread for an inquiry, or create a new one if none exists.
///
/// **Caller**: `generate_offer_email_draft`
/// **Why**: Offer email drafts must belong to a thread; this ensures one always exists
///          without creating duplicates when multiple offers are generated.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config)
/// - `inquiry_id` — inquiry to find/create the thread for
///
/// # Returns
/// The thread UUID, or `Uuid::nil()` if the inquiry record cannot be found.
async fn find_or_create_inquiry_thread(state: &AppState, inquiry_id: Uuid) -> Uuid {
    // Return existing thread if one already exists
    if let Ok(Some((id,))) = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM email_threads WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    {
        return id;
    }

    // Look up customer_id from the inquiry
    let Ok(Some((customer_id,))) = sqlx::query_as::<_, (Uuid,)>(
        "SELECT customer_id FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    else {
        return Uuid::nil();
    };

    let thread_id = Uuid::now_v7();
    let _ = sqlx::query(
        "INSERT INTO email_threads (id, customer_id, inquiry_id, subject, created_at, updated_at) \
         VALUES ($1, $2, $3, 'Ihr Umzugsangebot', NOW(), NOW())",
    )
    .bind(thread_id)
    .bind(customer_id)
    .bind(inquiry_id)
    .execute(&state.db)
    .await;

    thread_id
}

/// `GET /api/v1/inquiries/{id}/emails` -- Fetch email thread for this inquiry.
///
/// **Caller**: Admin dashboard inquiry detail email tab.
/// **Why**: Shows linked email conversations for the inquiry.
async fn get_inquiry_emails(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<Vec<EmailThreadSummary>>, ApiError> {
    let threads: Vec<EmailThreadSummary> = sqlx::query_as(
        r#"
        SELECT
            et.id,
            et.subject,
            (SELECT MAX(em.created_at) FROM email_messages em WHERE em.thread_id = et.id) AS last_message_at,
            COALESCE((SELECT COUNT(*) FROM email_messages em WHERE em.thread_id = et.id), 0) AS message_count
        FROM email_threads et
        WHERE et.inquiry_id = $1
        ORDER BY et.created_at DESC
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(threads))
}

/// `POST /api/v1/inquiries/{id}/estimate/depth` and `/estimate/video`
///
/// **Caller**: Admin dashboard — triggers vision pipeline on an existing inquiry.
/// **Why**: Accepts multipart image/video upload, runs S3 upload + vision estimation
///          in the background, and auto-generates an offer when complete.
async fn trigger_estimate_upload(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Verify inquiry exists
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM inquiries WHERE id = $1)")
        .bind(inquiry_id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::NotFound(format!("Inquiry {inquiry_id} not found")));
    }

    let parsed = parse_inquiry_form(multipart, false).await?;
    if parsed.images.is_empty() {
        return Err(ApiError::Validation("Mindestens ein Bild erforderlich".into()));
    }

    // Update status to estimating
    let now = chrono::Utc::now();
    sqlx::query("UPDATE inquiries SET status = 'estimating', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    // Pre-create the estimation row so the frontend can poll it immediately.
    let estimation_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, created_at) \
         VALUES ($1, $2, 'depth_sensor', 'processing', '{}', NOW())",
    )
    .bind(estimation_id)
    .bind(inquiry_id)
    .execute(&state.db)
    .await?;

    // Upload images to S3 synchronously so the frontend can display them while Modal processes.
    let s3_keys = if !parsed.images.is_empty() {
        services::vision::upload_images_to_s3(
            &*state.storage,
            inquiry_id,
            estimation_id,
            &parsed.images,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(inquiry_id = %inquiry_id, "Pre-spawn S3 upload failed: {e}");
            Vec::new()
        })
    } else {
        Vec::new()
    };

    // Update source_data with s3_keys immediately so images are visible in the admin UI
    // while Modal is still processing.
    if !s3_keys.is_empty() {
        let source_data = serde_json::json!({ "s3_keys": &s3_keys, "image_count": s3_keys.len() });
        let _ = sqlx::query(
            "UPDATE volume_estimations SET source_data = $1 WHERE id = $2",
        )
        .bind(&source_data)
        .bind(estimation_id)
        .execute(&state.db)
        .await;
    }

    // Spawn background processing (same pipeline as public submission)
    let state_bg = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = process_submission_background(
            Arc::clone(&state_bg),
            inquiry_id,
            estimation_id,
            parsed.images,
            parsed.depth_maps,
            parsed.ar_metadata,
            String::new(),
            String::new(),
            s3_keys,
            now,
        )
        .await
        {
            tracing::error!(inquiry_id = %inquiry_id, error = %e, "Background estimation failed");
            let _ = sqlx::query(
                "UPDATE volume_estimations SET status = 'failed' WHERE id = $1 AND status = 'processing'",
            )
            .bind(estimation_id)
            .execute(&state_bg.db)
            .await;
        }
    });

    // Return an array of { id, status } so the frontend can poll each estimation
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!([{
            "id": estimation_id,
            "status": "processing"
        }])),
    ))
}

// ===========================================================================
// Video estimation handler
// ===========================================================================

/// `POST /api/v1/inquiries/{id}/estimate/video`
///
/// **Caller**: Admin dashboard — triggers video 3D pipeline on an existing inquiry.
/// **Why**: Accepts multipart video upload, saves the file to S3, then queues it for
///          processing on the Modal video endpoint (MASt3R + SAM 2 pipeline).
///          Returns immediately with a processing estimation ID for polling.
async fn trigger_video_upload(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM inquiries WHERE id = $1)")
        .bind(inquiry_id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::NotFound(format!("Inquiry {inquiry_id} not found")));
    }

    // Read the video field from the multipart body
    let mut video_data: Option<(Vec<u8>, String)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Ungültige Formulardaten: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        if field_name == "video" {
            // Accept any content-type that starts with "video/", or fall back to
            // "video/mp4" for generic types (application/octet-stream, empty) that
            // some browsers/OS combos send for valid video files (.mov, .mkv, etc.).
            // The frontend already validates by file extension before queuing.
            let content_type = field
                .content_type()
                .map(|ct| {
                    if ct.starts_with("video/") {
                        ct.to_string()
                    } else {
                        "video/mp4".to_string()
                    }
                })
                .unwrap_or_else(|| "video/mp4".to_string());
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("Video konnte nicht gelesen werden: {e}")))?;
            video_data = Some((data.to_vec(), content_type));
        }
    }

    let (video_bytes, mime_type) = video_data
        .ok_or_else(|| ApiError::Validation("Kein Video-Feld in der Anfrage gefunden".into()))?;

    if video_bytes.is_empty() {
        return Err(ApiError::Validation("Video-Datei ist leer".into()));
    }

    let now = chrono::Utc::now();
    sqlx::query("UPDATE inquiries SET status = 'estimating', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    let estimation_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, created_at) \
         VALUES ($1, $2, 'video', 'processing', '{}', NOW())",
    )
    .bind(estimation_id)
    .bind(inquiry_id)
    .execute(&state.db)
    .await?;

    // Upload video to S3 synchronously so the frontend can reference the file
    // while Modal processes it in the background.
    let s3_key = format!("estimates/{inquiry_id}/{estimation_id}/video.mp4");
    state
        .storage
        .upload(
            &s3_key,
            bytes::Bytes::from(video_bytes.clone()),
            &mime_type,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("S3 video upload failed: {e}")))?;

    tracing::info!(inquiry_id = %inquiry_id, %s3_key, "Video uploaded to S3 before spawn");

    let state_bg = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) =
            process_video_background(state_bg.clone(), inquiry_id, estimation_id, video_bytes, mime_type, s3_key).await
        {
            tracing::error!(inquiry_id = %inquiry_id, error = %e, "Background video estimation failed");
            let _ = sqlx::query(
                "UPDATE volume_estimations SET status = 'failed' WHERE id = $1 AND status = 'processing'",
            )
            .bind(estimation_id)
            .execute(&state_bg.db)
            .await;
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!([{"id": estimation_id, "status": "processing"}])),
    ))
}

/// Background task: semaphore acquire → async Modal video submit → poll → store results → generate offer.
///
/// **Caller**: `trigger_video_upload` (admin dashboard) and `video_inquiry` (public endpoint).
/// **Why**: Video upload to S3 is now done synchronously by the caller before this task
///          is spawned. This function acquires the vision semaphore, submits the video
///          to Modal via the async submit/poll pattern, and stores the result.
///          No LLM fallback — if the vision service fails, the estimation is marked failed.
///
/// # Parameters
/// - `s3_key` — the S3 key where the video was already uploaded by the caller
///
/// # Errors
/// Returns `Err(String)` on any fatal failure; the caller marks the estimation 'failed'.
async fn process_video_background(
    state: Arc<AppState>,
    inquiry_id: Uuid,
    estimation_id: Uuid,
    video_bytes: Vec<u8>,
    mime_type: String,
    s3_key: String,
) -> Result<(), String> {
    // 1. Acquire vision semaphore — waits if a photo or video job is already running on Modal.
    let _vision_permit = state
        .vision_semaphore
        .acquire()
        .await
        .map_err(|e| format!("Vision semaphore closed: {e}"))?;
    tracing::info!(estimation_id = %estimation_id, "Vision semaphore acquired, submitting video to Modal");

    // 2. Submit video job and poll for result via async pattern.
    let client = state
        .vision_service
        .as_ref()
        .ok_or("Vision service not configured")?;

    let poll_interval =
        std::time::Duration::from_secs(state.config.vision_service.poll_interval_secs);
    let max_polls = state.config.vision_service.max_polls;
    let max_retries = state.config.vision_service.max_retries;

    let response = client
        .estimate_video_async(
            &estimation_id.to_string(),
            &video_bytes,
            &mime_type,
            None,
            None,
            poll_interval,
            max_polls,
            max_retries,
        )
        .await
        .map_err(|e| {
            tracing::error!(
                inquiry_id = %inquiry_id,
                estimation_id = %estimation_id,
                "Video estimation failed after all retries — manual intervention required: {e}"
            );
            format!("Video estimation failed: {e}")
        })?;

    tracing::info!(
        estimation_id = %estimation_id,
        volume = response.total_volume_m3,
        items = response.detected_items.len(),
        "Video estimation succeeded"
    );

    // 3. Store results
    let source_data = serde_json::json!({
        "source": "video",
        "s3_key": s3_key,
        "mime_type": mime_type,
    });
    let result_data = serde_json::to_value(&response.detected_items)
        .map_err(|e| format!("Failed to serialize items: {e}"))?;

    sqlx::query(r#"
        INSERT INTO volume_estimations
            (id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, 'video', 'completed', $3, $4, $5, $6, $7)
        ON CONFLICT (id) DO UPDATE SET
            status            = 'completed',
            source_data       = EXCLUDED.source_data,
            result_data       = EXCLUDED.result_data,
            total_volume_m3   = EXCLUDED.total_volume_m3,
            confidence_score  = EXCLUDED.confidence_score
    "#)
    .bind(estimation_id)
    .bind(inquiry_id)
    .bind(source_data)
    .bind(result_data)
    .bind(response.total_volume_m3)
    .bind(response.confidence_score)
    .bind(chrono::Utc::now())
    .execute(&state.db)
    .await
    .map_err(|e| format!("Failed to store video estimation: {e}"))?;

    // 4. Update inquiry status and trigger offer generation
    sqlx::query(
        "UPDATE inquiries SET status = 'estimated', volume_m3 = $1, updated_at = $2 WHERE id = $3",
    )
    .bind(response.total_volume_m3)
    .bind(chrono::Utc::now())
    .bind(inquiry_id)
    .execute(&state.db)
    .await
    .map_err(|e| format!("Failed to update inquiry: {e}"))?;

    orchestrator::try_auto_generate_offer(Arc::clone(&state), inquiry_id).await;

    Ok(())
}

// ===========================================================================
// Submission handlers (public, no auth)
// ===========================================================================

/// Response returned from both /photo and /mobile endpoints.
#[derive(Serialize)]
struct SubmitInquiryResponse {
    inquiry_id: Uuid,
    customer_id: Uuid,
    status: String,
    message: String,
}

/// POST /submit/photo -- Photo webapp inquiry (Source C).
async fn photo_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, false).await?;
    handle_submission(state, parsed, "photo_webapp").await
}

/// POST /submit/mobile -- Mobile app inquiry (Source D).
async fn mobile_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, true).await?;
    handle_submission(state, parsed, "mobile_app").await
}

/// `POST /api/v1/submit/video` — Public video inquiry (Source E).
///
/// **Caller**: Public-facing `/angebot` page (video mode).
/// **Why**: Lets customers submit a room walkthrough video without authentication.
///          Creates customer + inquiry records immediately, then queues video
///          processing via Modal (MASt3R + SAM 2) in the background.
async fn video_inquiry(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    // Parse contact + address fields and the video file from the same multipart body
    let mut name: Option<String> = None;
    let mut salutation: Option<String> = None;
    let mut first_name: Option<String> = None;
    let mut last_name: Option<String> = None;
    let mut email: Option<String> = None;
    let mut phone: Option<String> = None;
    let mut departure_address: Option<String> = None;
    let mut departure_floor: Option<String> = None;
    let mut departure_elevator: Option<bool> = None;
    let mut departure_parking_ban: Option<bool> = None;
    let mut arrival_address: Option<String> = None;
    let mut arrival_floor: Option<String> = None;
    let mut arrival_elevator: Option<bool> = None;
    let mut arrival_parking_ban: Option<bool> = None;
    let mut preferred_date: Option<String> = None;
    let mut services_text: Option<String> = None;
    let mut message: Option<String> = None;
    let mut video_files: Vec<(Vec<u8>, String)> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Ungültige Formulardaten: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "name" => name = Some(read_text_field(field).await?),
            "salutation" | "anrede" => salutation = Some(read_text_field(field).await?),
            "first_name" | "vorname" => first_name = Some(read_text_field(field).await?),
            "last_name" | "nachname" => last_name = Some(read_text_field(field).await?),
            "email" => email = Some(read_text_field(field).await?),
            "phone" => phone = Some(read_text_field(field).await?),
            "auszugsadresse" | "departure_address" => departure_address = Some(read_text_field(field).await?),
            "etage_auszug" | "departure_floor" => departure_floor = Some(read_text_field(field).await?),
            "aufzug_auszug" | "departure_elevator" => {
                let t = read_text_field(field).await?;
                departure_elevator = Some(parse_bool_field(&t));
            }
            "halteverbot_auszug" | "departure_parking_ban" => {
                let t = read_text_field(field).await?;
                departure_parking_ban = Some(parse_bool_field(&t));
            }
            "einzugsadresse" | "arrival_address" => arrival_address = Some(read_text_field(field).await?),
            "etage_einzug" | "arrival_floor" => arrival_floor = Some(read_text_field(field).await?),
            "aufzug_einzug" | "arrival_elevator" => {
                let t = read_text_field(field).await?;
                arrival_elevator = Some(parse_bool_field(&t));
            }
            "halteverbot_einzug" | "arrival_parking_ban" => {
                let t = read_text_field(field).await?;
                arrival_parking_ban = Some(parse_bool_field(&t));
            }
            "wunschtermin" | "preferred_date" => preferred_date = Some(read_text_field(field).await?),
            "zusatzleistungen" | "services" => services_text = Some(read_text_field(field).await?),
            "nachricht" | "message" => message = Some(read_text_field(field).await?),
            "video" => {
                // Accept any video/* MIME type; fall back to video/mp4 for generic types
                // (application/octet-stream, empty) that browsers send for .mov, .mkv, etc.
                let content_type = field
                    .content_type()
                    .map(|ct| {
                        if ct.starts_with("video/") { ct.to_string() } else { "video/mp4".to_string() }
                    })
                    .unwrap_or_else(|| "video/mp4".to_string());
                let data = field.bytes().await.map_err(|e| {
                    ApiError::BadRequest(format!("Video konnte nicht gelesen werden: {e}"))
                })?;
                if !data.is_empty() {
                    video_files.push((data.to_vec(), content_type));
                }
            }
            _ => continue,
        }
    }

    let name = name.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Name ist erforderlich".into()))?;
    let email = email.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("E-Mail ist erforderlich".into()))?;
    let departure_address = departure_address.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Auszugsadresse ist erforderlich".into()))?;
    let arrival_address = arrival_address.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Einzugsadresse ist erforderlich".into()))?;
    if video_files.is_empty() {
        return Err(ApiError::Validation("Kein Video-Feld in der Anfrage gefunden".into()));
    }

    let now = chrono::Utc::now();

    // Upsert customer
    let customer_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        r#"INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
           ON CONFLICT (email) DO UPDATE SET
               name = COALESCE(EXCLUDED.name, customers.name),
               salutation = COALESCE(EXCLUDED.salutation, customers.salutation),
               first_name = COALESCE(EXCLUDED.first_name, customers.first_name),
               last_name = COALESCE(EXCLUDED.last_name, customers.last_name),
               phone = COALESCE(EXCLUDED.phone, customers.phone),
               updated_at = $8
           RETURNING id"#,
    )
    .bind(Uuid::now_v7()).bind(&email).bind(&name).bind(&salutation).bind(&first_name).bind(&last_name).bind(&phone).bind(now)
    .fetch_one(&state.db).await.map(|(id,)| id)
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    // Create addresses
    let (dep_street, dep_city, dep_postal) = services::vision::parse_address(&departure_address);
    let origin_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(Uuid::now_v7()).bind(&dep_street).bind(&dep_city).bind(&dep_postal)
    .bind(&departure_floor).bind(departure_elevator)
    .fetch_one(&state.db).await.map(|(id,)| id)
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    let (arr_street, arr_city, arr_postal) = services::vision::parse_address(&arrival_address);
    let dest_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(Uuid::now_v7()).bind(&arr_street).bind(&arr_city).bind(&arr_postal)
    .bind(&arrival_floor).bind(arrival_elevator)
    .fetch_one(&state.db).await.map(|(id,)| id)
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

    let preferred_date_ts = preferred_date.as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    let notes = build_notes(
        services_text.as_deref(),
        departure_parking_ban,
        arrival_parking_ban,
        message.as_deref(),
    );

    // Create inquiry
    let inquiry_id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                               status, preferred_date, notes, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)"#,
    )
    .bind(inquiry_id).bind(customer_id).bind(Some(origin_id)).bind(Some(dest_id))
    .bind("pending").bind(preferred_date_ts).bind(&notes).bind(now)
    .execute(&state.db).await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    // Pre-create one estimation row per uploaded video and upload each video to S3
    // synchronously before returning 202, so the frontend can reference the files
    // while Modal processes them.
    let mut estimation_ids: Vec<Uuid> = Vec::new();
    let mut s3_keys_per_video: Vec<String> = Vec::new();
    for (video_bytes, mime_type) in &video_files {
        let eid = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, created_at) \
             VALUES ($1, $2, 'video', 'processing', '{}', NOW())",
        )
        .bind(eid).bind(inquiry_id).execute(&state.db).await
        .map_err(|e| ApiError::Internal(format!("Schätzung konnte nicht erstellt werden: {e}")))?;
        estimation_ids.push(eid);

        // Upload video to S3
        let s3_key = format!("estimates/{inquiry_id}/{eid}/video.mp4");
        if let Err(e) = state.storage.upload(&s3_key, bytes::Bytes::from(video_bytes.clone()), mime_type).await {
            tracing::warn!(inquiry_id = %inquiry_id, "Pre-spawn video S3 upload failed: {e}");
            s3_keys_per_video.push(String::new());
        } else {
            // Update source_data immediately so the admin UI can show the video
            let source_data = serde_json::json!({ "video_s3_key": &s3_key });
            let _ = sqlx::query("UPDATE volume_estimations SET source_data = $1 WHERE id = $2")
                .bind(&source_data).bind(eid).execute(&state.db).await;
            s3_keys_per_video.push(s3_key);
        }
    }

    tracing::info!(
        inquiry_id = %inquiry_id,
        video_count = video_files.len(),
        "Video inquiry created, spawning background processing"
    );

    // Spawn background: distance calc → for each video: semaphore → async Modal → offer
    let state_bg = Arc::clone(&state);
    let dep_addr = departure_address.clone();
    let arr_addr = arrival_address.clone();
    tokio::spawn(async move {
        // Distance calculation (once, shared across all videos)
        let api_key = &state_bg.config.maps.api_key;
        if !api_key.is_empty() {
            let calc = aust_distance_calculator::RouteCalculator::new(api_key.clone());
            let req = aust_distance_calculator::RouteRequest { addresses: vec![dep_addr, arr_addr] };
            match calc.calculate(&req).await {
                Ok(r) => {
                    let _ = sqlx::query("UPDATE inquiries SET distance_km = $1, updated_at = $2 WHERE id = $3")
                        .bind(r.total_distance_km).bind(chrono::Utc::now()).bind(inquiry_id)
                        .execute(&state_bg.db).await;
                }
                Err(e) => tracing::warn!(inquiry_id = %inquiry_id, error = %e, "Distance calculation failed"),
            }
        }
        for (((video_bytes, mime_type), estimation_id), s3_key) in video_files.into_iter()
            .zip(estimation_ids.into_iter())
            .zip(s3_keys_per_video.into_iter())
        {
            if let Err(e) = process_video_background(
                state_bg.clone(), inquiry_id, estimation_id, video_bytes, mime_type, s3_key,
            ).await {
                tracing::error!(inquiry_id = %inquiry_id, estimation_id = %estimation_id, error = %e, "Background video estimation failed");
                let _ = sqlx::query(
                    "UPDATE volume_estimations SET status = 'failed' WHERE id = $1 AND status = 'processing'",
                )
                .bind(estimation_id).execute(&state_bg.db).await;
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitInquiryResponse {
            inquiry_id,
            customer_id,
            status: "processing".to_string(),
            message: "Anfrage erhalten. Video wird analysiert und Angebot wird erstellt.".to_string(),
        }),
    ))
}

/// All parsed fields from the multipart form.
struct ParsedInquiryForm {
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    departure_address: Option<String>,
    departure_floor: Option<String>,
    departure_parking_ban: Option<bool>,
    departure_elevator: Option<bool>,
    arrival_address: Option<String>,
    arrival_floor: Option<String>,
    arrival_parking_ban: Option<bool>,
    arrival_elevator: Option<bool>,
    preferred_date: Option<String>,
    services: Option<String>,
    message: Option<String>,
    images: Vec<(Vec<u8>, String)>,
    depth_maps: Vec<(Vec<u8>, String)>,
    ar_metadata: Option<String>,
}

/// Parse the multipart form data into a structured form.
async fn parse_inquiry_form(
    mut multipart: Multipart,
    accept_depth: bool,
) -> Result<ParsedInquiryForm, ApiError> {
    let mut form = ParsedInquiryForm {
        name: None,
        salutation: None,
        first_name: None,
        last_name: None,
        email: None,
        phone: None,
        departure_address: None,
        departure_floor: None,
        departure_parking_ban: None,
        departure_elevator: None,
        arrival_address: None,
        arrival_floor: None,
        arrival_parking_ban: None,
        arrival_elevator: None,
        preferred_date: None,
        services: None,
        message: None,
        images: Vec::new(),
        depth_maps: Vec::new(),
        ar_metadata: None,
    };

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Ungültige Formulardaten: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "name" => form.name = Some(read_text_field(field).await?),
            "salutation" | "anrede" => form.salutation = Some(read_text_field(field).await?),
            "first_name" | "vorname" => form.first_name = Some(read_text_field(field).await?),
            "last_name" | "nachname" => form.last_name = Some(read_text_field(field).await?),
            "email" => form.email = Some(read_text_field(field).await?),
            "phone" => form.phone = Some(read_text_field(field).await?),
            "departure_address" | "auszugsadresse" => {
                form.departure_address = Some(read_text_field(field).await?);
            }
            "departure_floor" | "etage_auszug" | "etage-auszug" => {
                form.departure_floor = Some(read_text_field(field).await?);
            }
            "departure_parking_ban" | "halteverbot_auszug" | "halteverbot-auszug" => {
                let text = read_text_field(field).await?;
                form.departure_parking_ban = Some(parse_bool_field(&text));
            }
            "departure_elevator" | "aufzug_auszug" | "aufzug-auszug" => {
                let text = read_text_field(field).await?;
                form.departure_elevator = Some(parse_bool_field(&text));
            }
            "arrival_address" | "einzugsadresse" => {
                form.arrival_address = Some(read_text_field(field).await?);
            }
            "arrival_floor" | "etage_einzug" | "etage-einzug" => {
                form.arrival_floor = Some(read_text_field(field).await?);
            }
            "arrival_parking_ban" | "halteverbot_einzug" | "halteverbot-einzug" => {
                let text = read_text_field(field).await?;
                form.arrival_parking_ban = Some(parse_bool_field(&text));
            }
            "arrival_elevator" | "aufzug_einzug" | "aufzug-einzug" => {
                let text = read_text_field(field).await?;
                form.arrival_elevator = Some(parse_bool_field(&text));
            }
            "preferred_date" | "wunschtermin" => {
                form.preferred_date = Some(read_text_field(field).await?);
            }
            "services" | "zusatzleistungen" => {
                form.services = Some(read_text_field(field).await?);
            }
            "message" | "nachricht" => form.message = Some(read_text_field(field).await?),
            "images" => {
                // Accept any file type — images go to vision pipeline, other types
                // (videos, docs) are stored in S3 and attached to the inquiry.
                let content_type = field
                    .content_type()
                    .map(|ct| ct.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| {
                        ApiError::BadRequest(format!("Datei konnte nicht gelesen werden: {e}"))
                    })?;
                if !data.is_empty() {
                    form.images.push((data.to_vec(), content_type));
                }
            }
            "depth_maps" if accept_depth => {
                let content_type = field.content_type().unwrap_or("image/png").to_string();
                let data = field.bytes().await.map_err(|e| {
                    ApiError::BadRequest(format!(
                        "Tiefenkarte konnte nicht gelesen werden: {e}"
                    ))
                })?;
                form.depth_maps.push((data.to_vec(), content_type));
            }
            "ar_metadata" if accept_depth => {
                form.ar_metadata = Some(read_text_field(field).await?);
            }
            _ => continue,
        }
    }

    Ok(form)
}

async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> Result<String, ApiError> {
    field
        .text()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Feld konnte nicht gelesen werden: {e}")))
}

fn parse_bool_field(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "true" | "1" | "yes" | "ja"
    )
}

/// Shared handler for both photo and mobile submissions.
async fn handle_submission(
    state: Arc<AppState>,
    form: ParsedInquiryForm,
    source: &str,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    // Validate required fields
    let name = form
        .name
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Name ist erforderlich".into()))?;
    let email = form
        .email
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("E-Mail ist erforderlich".into()))?;
    let departure_address = form
        .departure_address
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Auszugsadresse ist erforderlich".into()))?;
    let arrival_address = form
        .arrival_address
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Einzugsadresse ist erforderlich".into()))?;


    let now = chrono::Utc::now();

    // 1. Create or update customer by email
    let customer_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        ON CONFLICT (email) DO UPDATE SET
            name = COALESCE(EXCLUDED.name, customers.name),
            salutation = COALESCE(EXCLUDED.salutation, customers.salutation),
            first_name = COALESCE(EXCLUDED.first_name, customers.first_name),
            last_name = COALESCE(EXCLUDED.last_name, customers.last_name),
            phone = COALESCE(EXCLUDED.phone, customers.phone),
            updated_at = $8
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(&name)
    .bind(&form.salutation)
    .bind(&form.first_name)
    .bind(&form.last_name)
    .bind(&form.phone)
    .bind(now)
    .fetch_one(&state.db)
    .await
    .map(|(id,)| id)
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    tracing::info!(customer_id = %customer_id, email = %email, "Customer created/updated");

    // 2. Create origin address
    let (dep_street, dep_city, dep_postal) =
        services::vision::parse_address(&departure_address);
    let origin_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&dep_street)
    .bind(&dep_city)
    .bind(&dep_postal)
    .bind(&form.departure_floor)
    .bind(form.departure_elevator)
    .fetch_one(&state.db)
    .await
    .map(|(id,)| id)
    .map_err(|e| {
        ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}"))
    })?;

    // 3. Create destination address
    let (arr_street, arr_city, arr_postal) =
        services::vision::parse_address(&arrival_address);
    let dest_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(&arr_street)
    .bind(&arr_city)
    .bind(&arr_postal)
    .bind(&form.arrival_floor)
    .bind(form.arrival_elevator)
    .fetch_one(&state.db)
    .await
    .map(|(id,)| id)
    .map_err(|e| {
        ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}"))
    })?;

    // 4. Parse preferred date
    let preferred_date_ts = form
        .preferred_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    // 5. Build notes from services, parking bans, and message
    let notes = build_notes(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
        form.message.as_deref(),
    );

    // 5b. Parse services string into JSONB struct
    let services_struct = parse_services_string(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
    );
    let services_json = serde_json::to_value(&services_struct).unwrap_or(serde_json::json!({}));

    // 6. Create inquiry
    let inquiry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, preferred_date, notes, services, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .bind(Some(origin_id))
    .bind(Some(dest_id))
    .bind("pending")
    .bind(preferred_date_ts)
    .bind(&notes)
    .bind(&services_json)
    .bind(source)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    tracing::info!(inquiry_id = %inquiry_id, "Inquiry created for submission");

    // 7. Pre-create estimation row and upload images to S3 *before* spawning the
    //    background task so the frontend sees images immediately after receiving 202.
    let estimation_id = Uuid::now_v7();

    // Pre-create estimation record with status='processing' so polling works immediately.
    sqlx::query(
        "INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, created_at) \
         VALUES ($1, $2, 'depth_sensor', 'processing', '{}', NOW())",
    )
    .bind(estimation_id)
    .bind(inquiry_id)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(format!("Schätzung konnte nicht erstellt werden: {e}")))?;

    // Upload images to S3 synchronously — frontend can display them while Modal processes.
    let s3_keys = if !form.images.is_empty() {
        services::vision::upload_images_to_s3(
            &*state.storage,
            inquiry_id,
            estimation_id,
            &form.images,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(inquiry_id = %inquiry_id, "Pre-spawn S3 upload failed: {e}");
            Vec::new()
        })
    } else {
        Vec::new()
    };

    tracing::info!(
        inquiry_id = %inquiry_id,
        image_count = form.images.len(),
        s3_keys_count = s3_keys.len(),
        "Images uploaded to S3 before spawn"
    );

    // Update source_data with s3_keys immediately so images are visible in the admin UI
    // while Modal is still processing.
    if !s3_keys.is_empty() {
        let source_data = serde_json::json!({ "s3_keys": &s3_keys, "image_count": s3_keys.len() });
        let _ = sqlx::query(
            "UPDATE volume_estimations SET source_data = $1 WHERE id = $2",
        )
        .bind(&source_data)
        .bind(estimation_id)
        .execute(&state.db)
        .await;
    }

    // 8. Spawn background processing: distance calc → semaphore → Modal → offer.
    let state_bg = Arc::clone(&state);
    let dep_addr = departure_address.clone();
    let arr_addr = arrival_address.clone();
    tokio::spawn(async move {
        if let Err(e) = process_submission_background(
            state_bg,
            inquiry_id,
            estimation_id,
            form.images,
            form.depth_maps,
            form.ar_metadata,
            dep_addr,
            arr_addr,
            s3_keys,
            now,
        )
        .await
        {
            tracing::error!(inquiry_id = %inquiry_id, error = %e, "Background inquiry processing failed");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitInquiryResponse {
            inquiry_id,
            customer_id,
            status: "processing".to_string(),
            message: "Anfrage erhalten. Bilder werden analysiert und Angebot wird erstellt."
                .to_string(),
        }),
    ))
}

/// Background processing: distance calc → semaphore acquire → async Modal submission
/// → poll for result → store estimation → generate offer.
///
/// **Caller**: `handle_submission` (photo/mobile public endpoints) and
///             `trigger_estimate_upload` (admin dashboard).
/// **Why**: S3 upload and estimation row creation now happen synchronously in the
///          caller before this task is spawned, so the frontend can display images
///          immediately. This function acquires the vision semaphore, submits the job
///          to Modal via the async submit/poll pattern, and stores the result.
///          No LLM fallback — if the vision service fails after all retries, the
///          estimation is marked failed and manual intervention is required.
///
/// # Parameters
/// - `s3_keys` — already-uploaded image keys (pre-computed by the caller)
///
/// # Errors
/// Returns `Err(String)` on any fatal failure; the caller marks the estimation 'failed'.
async fn process_submission_background(
    state: Arc<AppState>,
    inquiry_id: Uuid,
    estimation_id: Uuid,
    images: Vec<(Vec<u8>, String)>,
    depth_maps: Vec<(Vec<u8>, String)>,
    ar_metadata: Option<String>,
    departure_address: String,
    arrival_address: String,
    s3_keys: Vec<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    // 0. Calculate distance between origin and destination
    let api_key = &state.config.maps.api_key;
    if !api_key.is_empty() {
        let calculator = aust_distance_calculator::RouteCalculator::new(api_key.clone());
        let request = aust_distance_calculator::RouteRequest {
            addresses: vec![departure_address, arrival_address],
        };
        match calculator.calculate(&request).await {
            Ok(result) => {
                tracing::info!(
                    inquiry_id = %inquiry_id,
                    distance_km = result.total_distance_km,
                    "Distance calculated"
                );
                let _ = sqlx::query(
                    "UPDATE inquiries SET distance_km = $1, updated_at = $2 WHERE id = $3",
                )
                .bind(result.total_distance_km)
                .bind(chrono::Utc::now())
                .bind(inquiry_id)
                .execute(&state.db)
                .await;
            }
            Err(e) => {
                tracing::warn!(inquiry_id = %inquiry_id, error = %e, "Distance calculation failed, continuing without");
            }
        }
    } else {
        tracing::warn!("Maps API key not configured, skipping distance calculation");
    }

    // 1. Upload depth maps if present (images are already in S3 from the caller)
    if !depth_maps.is_empty() {
        if let Err(e) =
            upload_depth_maps_to_s3(&*state.storage, inquiry_id, estimation_id, &depth_maps).await
        {
            tracing::warn!("Failed to upload depth maps: {e}");
        }
    }

    // 2. Acquire the vision semaphore so only one job runs on Modal at a time.
    //    Other workers will queue here until the current GPU job completes.
    let _vision_permit = state
        .vision_semaphore
        .acquire()
        .await
        .map_err(|e| format!("Vision semaphore closed: {e}"))?;
    tracing::info!(estimation_id = %estimation_id, "Vision semaphore acquired, submitting to Modal");

    // 3. Run volume estimation via async submit + poll (no LLM fallback).
    //    If the vision service fails after all retries, the estimation is marked
    //    'failed' by the tokio::spawn error handler — manual review required.
    let (total_volume, confidence, result_data) = services::vision::try_vision_service_async(
        &state,
        &images,
        estimation_id,
        inquiry_id,
        estimation_id,
    )
    .await
    .map_err(|e| {
        tracing::error!(
            inquiry_id = %inquiry_id,
            estimation_id = %estimation_id,
            "Vision estimation failed after all retries — manual intervention required: {e}"
        );
        format!("Vision estimation failed: {e}")
    })?;

    let method = EstimationMethod::DepthSensor;

    tracing::info!(
        estimation_id = %estimation_id,
        volume = total_volume,
        "Vision service estimation succeeded"
    );

    // 4. Build source_data JSON
    let source_data = serde_json::json!({
        "source": if depth_maps.is_empty() { "photo_webapp" } else { "mobile_app" },
        "image_count": images.len(),
        "depth_map_count": depth_maps.len(),
        "s3_keys": s3_keys,
        "ar_metadata": ar_metadata.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
    });

    // 5. Store volume_estimation record — UPSERT so it works whether the row was
    //    pre-created as 'processing' (admin trigger) or is brand-new (public submission).
    let now_update = chrono::Utc::now();
    sqlx::query(
        r#"
        INSERT INTO volume_estimations
            (id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, 'completed', $4, $5, $6, $7, $8)
        ON CONFLICT (id) DO UPDATE SET
            method            = EXCLUDED.method,
            status            = 'completed',
            source_data       = EXCLUDED.source_data,
            result_data       = EXCLUDED.result_data,
            total_volume_m3   = EXCLUDED.total_volume_m3,
            confidence_score  = EXCLUDED.confidence_score
        "#,
    )
    .bind(estimation_id)
    .bind(inquiry_id)
    .bind(method.as_str())
    .bind(&source_data)
    .bind(result_data.as_ref())
    .bind(total_volume)
    .bind(confidence)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("Failed to store estimation: {e}"))?;

    // 6. Update inquiry with estimated volume
    update_quote_volume(
        &state.db,
        inquiry_id,
        total_volume,
        "estimated",
        now_update,
    )
    .await
    .map_err(|e| format!("Failed to update inquiry: {e}"))?;

    tracing::info!(
        inquiry_id = %inquiry_id,
        estimation_id = %estimation_id,
        volume = total_volume,
        "Volume estimation completed"
    );

    // 7. Auto-generate offer (XLSX -> PDF -> Telegram)
    orchestrator::try_auto_generate_offer(Arc::clone(&state), inquiry_id).await;

    Ok(())
}

/// Upload depth maps to S3.
async fn upload_depth_maps_to_s3(
    storage: &dyn StorageProvider,
    inquiry_id: Uuid,
    estimation_id: Uuid,
    depth_maps: &[(Vec<u8>, String)],
) -> Result<Vec<String>, ApiError> {
    let mut s3_keys = Vec::with_capacity(depth_maps.len());
    for (idx, (data, mime_type)) in depth_maps.iter().enumerate() {
        let ext = match mime_type.as_str() {
            "image/png" => "png",
            _ => "bin",
        };
        let key = format!("estimates/{inquiry_id}/{estimation_id}/depth/{idx}.{ext}");
        storage
            .upload(&key, Bytes::from(data.clone()), mime_type)
            .await
            .map_err(|e| {
                ApiError::Internal(format!("Tiefenkarten-Upload fehlgeschlagen: {e}"))
            })?;
        s3_keys.push(key);
    }
    Ok(s3_keys)
}

/// Build notes string from services, parking bans, and optional message.
/// Convert a comma-separated services string (from multipart form) + parking ban flags
/// into a typed `Services` struct for JSONB storage.
fn parse_services_string(
    services: Option<&str>,
    departure_parking_ban: Option<bool>,
    arrival_parking_ban: Option<bool>,
) -> Services {
    let s = services.unwrap_or("").to_lowercase();
    let without_dis = s.replace("disassembly", "").replace("demontage", "");
    Services {
        packing: s.contains("packing") || s.contains("einpack") || s.contains("verpackung"),
        assembly: without_dis.contains("assembly") || without_dis.contains("montage"),
        disassembly: s.contains("disassembly") || s.contains("demontage"),
        storage: s.contains("storage") || s.contains("einlagerung"),
        disposal: s.contains("disposal") || s.contains("entsorgung"),
        parking_ban_origin: departure_parking_ban.unwrap_or(false),
        parking_ban_destination: arrival_parking_ban.unwrap_or(false),
    }
}

fn build_notes(
    services: Option<&str>,
    departure_parking_ban: Option<bool>,
    arrival_parking_ban: Option<bool>,
    message: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    if let Some(services_str) = services {
        for service in services_str.split(',') {
            let s = service.trim();
            let lower = s.to_lowercase();
            match lower.as_str() {
                "packing" => parts.push("Verpackungsservice".to_string()),
                "assembly" => parts.push("Montage".to_string()),
                "disassembly" => parts.push("Demontage".to_string()),
                "storage" => parts.push("Einlagerung".to_string()),
                "disposal" => parts.push("Entsorgung".to_string()),
                _ if lower.contains("demontage") => parts.push("Demontage".to_string()),
                _ if lower.contains("montage") => parts.push("Montage".to_string()),
                _ if lower.contains("einpack") || lower.contains("verpackung") => {
                    parts.push("Verpackungsservice".to_string());
                }
                _ if lower.contains("einlagerung") => parts.push("Einlagerung".to_string()),
                _ if lower.contains("entsorgung") => parts.push("Entsorgung".to_string()),
                _ if !s.is_empty() => parts.push(s.to_string()),
                _ => {}
            }
        }
    }

    if departure_parking_ban == Some(true) {
        parts.push("Halteverbot Auszug".to_string());
    }
    if arrival_parking_ban == Some(true) {
        parts.push("Halteverbot Einzug".to_string());
    }

    if let Some(msg) = message {
        if !msg.trim().is_empty() {
            parts.push(msg.trim().to_string());
        }
    }

    parts.join(", ")
}

// ---------------------------------------------------------------------------
// Employee assignment endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow)]
struct EmployeeAssignmentRow {
    employee_id: Uuid,
    first_name: String,
    last_name: String,
    email: String,
    planned_hours: f64,
    actual_hours: Option<f64>,
    notes: Option<String>,
}

/// `GET /api/v1/inquiries/{id}/employees` — List employees assigned to this inquiry.
///
/// **Caller**: Inquiry detail Mitarbeiter card.
/// **Why**: Shows which employees are assigned to a job and their hours.
///
/// # Returns
/// `200 OK` with `{ assignments: [...] }`.
async fn list_inquiry_employees(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<EmployeeAssignmentRow> = sqlx::query_as(
        r#"
        SELECT ie.employee_id, e.first_name, e.last_name, e.email,
               ie.planned_hours::float8 AS planned_hours,
               ie.actual_hours::float8 AS actual_hours,
               ie.notes
        FROM inquiry_employees ie
        JOIN employees e ON ie.employee_id = e.id
        WHERE ie.inquiry_id = $1
        ORDER BY e.last_name, e.first_name
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "assignments": rows })))
}

/// `POST /api/v1/inquiries/{id}/employees` — Assign an employee to this inquiry.
///
/// **Caller**: Inquiry detail Mitarbeiter card assign button.
/// **Why**: Links an employee to a moving job with planned hours.
///
/// # Returns
/// `201 Created` with the assignment.
async fn assign_employee(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<aust_core::models::AssignEmployee>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Verify inquiry exists
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM inquiries WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Anfrage nicht gefunden".into()));
    }

    // Verify employee exists and is active
    let emp: Option<(bool,)> =
        sqlx::query_as("SELECT active FROM employees WHERE id = $1")
            .bind(body.employee_id)
            .fetch_optional(&state.db)
            .await?;
    match emp {
        None => return Err(ApiError::NotFound("Mitarbeiter nicht gefunden".into())),
        Some((false,)) => {
            return Err(ApiError::BadRequest("Mitarbeiter ist inaktiv".into()))
        }
        _ => {}
    }

    sqlx::query(
        r#"
        INSERT INTO inquiry_employees (id, inquiry_id, employee_id, planned_hours, notes)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(body.employee_id)
    .bind(body.planned_hours)
    .bind(&body.notes)
    .execute(&state.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("inquiry_employees_inquiry_id_employee_id_key") {
                return ApiError::Conflict(
                    "Mitarbeiter ist bereits dieser Anfrage zugewiesen".into(),
                );
            }
        }
        ApiError::from(e)
    })?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "employee_id": body.employee_id,
            "inquiry_id": id,
            "planned_hours": body.planned_hours,
            "notes": body.notes,
        })),
    ))
}

/// `PATCH /api/v1/inquiries/{id}/employees/{emp_id}` — Update assignment hours/notes.
///
/// **Caller**: Inquiry detail Mitarbeiter card inline edit.
/// **Why**: Allows updating planned/actual hours after initial assignment.
///
/// # Returns
/// `200 OK` with updated assignment.
async fn update_assignment(
    State(state): State<Arc<AppState>>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<aust_core::models::UpdateAssignment>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE inquiry_employees SET
            planned_hours = COALESCE($3, planned_hours),
            actual_hours = COALESCE($4, actual_hours),
            notes = COALESCE($5, notes)
        WHERE inquiry_id = $1 AND employee_id = $2
        "#,
    )
    .bind(id)
    .bind(emp_id)
    .bind(body.planned_hours)
    .bind(body.actual_hours)
    .bind(&body.notes)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    #[derive(sqlx::FromRow)]
    struct Updated {
        planned_hours: f64,
        actual_hours: Option<f64>,
        notes: Option<String>,
    }

    let row: Updated = sqlx::query_as(
        r#"
        SELECT planned_hours::float8 AS planned_hours,
               actual_hours::float8 AS actual_hours,
               notes
        FROM inquiry_employees
        WHERE inquiry_id = $1 AND employee_id = $2
        "#,
    )
    .bind(id)
    .bind(emp_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(serde_json::json!({
        "employee_id": emp_id,
        "inquiry_id": id,
        "planned_hours": row.planned_hours,
        "actual_hours": row.actual_hours,
        "notes": row.notes,
    })))
}

/// `DELETE /api/v1/inquiries/{id}/employees/{emp_id}` — Remove employee from inquiry.
///
/// **Caller**: Inquiry detail Mitarbeiter card remove button.
/// **Why**: Unlinks an employee from a moving job.
///
/// # Returns
/// `204 No Content`.
async fn remove_assignment(
    State(state): State<Arc<AppState>>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query(
        "DELETE FROM inquiry_employees WHERE inquiry_id = $1 AND employee_id = $2",
    )
    .bind(id)
    .bind(emp_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    Ok(StatusCode::NO_CONTENT)
}
