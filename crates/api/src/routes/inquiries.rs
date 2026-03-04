//! Inquiry endpoints — main CRUD resource for the unified inquiry lifecycle.
//!
//! Also includes submission endpoints for photo webapp (Source C) and mobile app (Source D).

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use bytes::Bytes;
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
    InquiryStatus, Offer, Services,
};
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
        .route("/{id}/estimate/{method}", post(trigger_estimate))
        .route("/{id}/generate-offer", post(generate_inquiry_offer))
        .route("/{id}/emails", get(get_inquiry_emails))
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
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateInquiryRequest {
    customer_email: String,
    customer_name: Option<String>,
    customer_phone: Option<String>,
    origin_address: Option<String>,
    origin_floor: Option<String>,
    origin_elevator: Option<bool>,
    destination_address: Option<String>,
    destination_floor: Option<String>,
    destination_elevator: Option<bool>,
    services: Option<Services>,
    notes: Option<String>,
    preferred_date: Option<String>,
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
    let email = request.customer_email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::Validation(
            "Gültige E-Mail-Adresse erforderlich".into(),
        ));
    }

    let now = chrono::Utc::now();

    // Upsert customer
    let (customer_id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        ON CONFLICT (email) DO UPDATE SET
            name = COALESCE(EXCLUDED.name, customers.name),
            phone = COALESCE(EXCLUDED.phone, customers.phone),
            updated_at = $5
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(&request.customer_name)
    .bind(&request.customer_phone)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Create origin address if provided
    let origin_id = if let Some(ref addr) = request.origin_address {
        if !addr.trim().is_empty() {
            let (street, city, postal) = services::vision::parse_address(addr);
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&postal)
            .bind(&request.origin_floor)
            .bind(request.origin_elevator)
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
    let dest_id = if let Some(ref addr) = request.destination_address {
        if !addr.trim().is_empty() {
            let (street, city, postal) = services::vision::parse_address(addr);
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&postal)
            .bind(&request.destination_floor)
            .bind(request.destination_elevator)
            .fetch_one(&state.db)
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Parse preferred date
    let preferred_date = request
        .preferred_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    // Build notes from services if not provided directly
    let notes = request.notes.clone();

    let services_json = serde_json::to_value(request.services.unwrap_or_default())
        .unwrap_or(serde_json::json!({}));

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
    .bind(origin_id)
    .bind(dest_id)
    .bind(InquiryStatus::Pending.as_str())
    .bind(preferred_date)
    .bind(&notes)
    .bind(&services_json)
    .bind("admin_dashboard")
    .bind(now)
    .execute(&state.db)
    .await?;

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

    sqlx::query(
        r#"
        UPDATE inquiries SET
            status = COALESCE($2, status),
            notes = COALESCE($3, notes),
            services = COALESCE($4, services),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            preferred_date = COALESCE($7, preferred_date),
            origin_address_id = COALESCE($8, origin_address_id),
            destination_address_id = COALESCE($9, destination_address_id),
            updated_at = $10
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
    .bind(request.origin_address_id)
    .bind(request.destination_address_id)
    .bind(now)
    .execute(&state.db)
    .await?;

    let response = inquiry_builder::build_inquiry_response(&state.db, id).await?;
    Ok(Json(response))
}

/// `DELETE /api/v1/inquiries/{id}` -- Soft-delete by setting status to cancelled.
///
/// **Caller**: Admin dashboard.
/// **Why**: Inquiries are never physically deleted for audit trail.
async fn delete_inquiry(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InquiryResponseModel>, ApiError> {
    let now = chrono::Utc::now();

    let rows_affected = sqlx::query(
        "UPDATE inquiries SET status = 'cancelled', updated_at = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(now)
    .execute(&state.db)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(ApiError::NotFound(format!("Inquiry {id} not found")));
    }

    let response = inquiry_builder::build_inquiry_response(&state.db, id).await?;
    Ok(Json(response))
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
    });

    // Reuse any existing active offer so we UPDATE in-place
    let existing_offer_id: Option<Uuid> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM offers WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled') LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?
    .map(|(id,)| id);

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

    Ok(Json(result.offer))
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
    handle_submission(state, parsed).await
}

/// POST /submit/mobile -- Mobile app inquiry (Source D).
async fn mobile_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, true).await?;
    handle_submission(state, parsed).await
}

/// All parsed fields from the multipart form.
struct ParsedInquiryForm {
    name: Option<String>,
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
                let content_type = field.content_type().unwrap_or("image/jpeg").to_string();
                if !content_type.starts_with("image/") {
                    continue;
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| {
                        ApiError::BadRequest(format!("Bild konnte nicht gelesen werden: {e}"))
                    })?;
                form.images.push((data.to_vec(), content_type));
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

    if form.images.is_empty() {
        return Err(ApiError::Validation(
            "Mindestens ein Bild ist erforderlich".into(),
        ));
    }

    let now = chrono::Utc::now();

    // 1. Create or update customer by email
    let customer_id: Uuid = sqlx::query_as::<_, (Uuid,)>(
        r#"
        INSERT INTO customers (id, email, name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        ON CONFLICT (email) DO UPDATE SET
            name = COALESCE(EXCLUDED.name, customers.name),
            phone = COALESCE(EXCLUDED.phone, customers.phone),
            updated_at = $5
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(&name)
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

    // 6. Create inquiry
    let inquiry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .bind(Some(origin_id))
    .bind(Some(dest_id))
    .bind("pending")
    .bind(preferred_date_ts)
    .bind(&notes)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    tracing::info!(inquiry_id = %inquiry_id, "Inquiry created for submission");

    // 7. Return 202 immediately -- spawn background processing
    let state_bg = Arc::clone(&state);
    let dep_addr = departure_address.clone();
    let arr_addr = arrival_address.clone();
    tokio::spawn(async move {
        if let Err(e) = process_submission_background(
            state_bg,
            inquiry_id,
            form.images,
            form.depth_maps,
            form.ar_metadata,
            dep_addr,
            arr_addr,
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

/// Background processing: distance calc -> S3 upload -> vision estimation -> store results -> generate offer.
async fn process_submission_background(
    state: Arc<AppState>,
    inquiry_id: Uuid,
    images: Vec<(Vec<u8>, String)>,
    depth_maps: Vec<(Vec<u8>, String)>,
    ar_metadata: Option<String>,
    departure_address: String,
    arrival_address: String,
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

    let estimation_id = Uuid::now_v7();

    // 1. Upload images to S3
    let s3_keys =
        services::vision::upload_images_to_s3(&*state.storage, inquiry_id, estimation_id, &images)
            .await
            .map_err(|e| format!("S3 upload failed: {e}"))?;

    tracing::info!(
        inquiry_id = %inquiry_id,
        image_count = images.len(),
        "Images uploaded to S3"
    );

    // Upload depth maps if present
    if !depth_maps.is_empty() {
        if let Err(e) =
            upload_depth_maps_to_s3(&*state.storage, inquiry_id, estimation_id, &depth_maps).await
        {
            tracing::warn!("Failed to upload depth maps: {e}");
        }
    }

    // 2. Run volume estimation (vision service -> LLM fallback)
    let (total_volume, confidence, result_data, method) = match services::vision::try_vision_service(
        &state,
        &images,
        estimation_id,
        inquiry_id,
        estimation_id,
    )
    .await
    {
        Ok((vol, conf, data)) => {
            tracing::info!(
                estimation_id = %estimation_id,
                volume = vol,
                "Vision service estimation succeeded"
            );
            (vol, conf, data, EstimationMethod::DepthSensor)
        }
        Err(e) => {
            tracing::warn!("Vision service unavailable, falling back to LLM: {e}");
            services::vision::fallback_llm_analysis(&state, &images)
                .await
                .map_err(|e| format!("LLM fallback failed: {e}"))?
        }
    };

    // 3. Build source_data JSON
    let source_data = serde_json::json!({
        "source": if depth_maps.is_empty() { "photo_webapp" } else { "mobile_app" },
        "image_count": images.len(),
        "depth_map_count": depth_maps.len(),
        "s3_keys": s3_keys,
        "ar_metadata": ar_metadata.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
    });

    // 4. Create volume_estimation record
    let now_update = chrono::Utc::now();
    insert_estimation_no_return(
        &state.db,
        estimation_id,
        inquiry_id,
        method.as_str(),
        &source_data,
        result_data.as_ref(),
        total_volume,
        confidence,
        now,
    )
    .await
    .map_err(|e| format!("Failed to store estimation: {e}"))?;

    // 5. Update inquiry with estimated volume
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

    // 6. Auto-generate offer (XLSX -> PDF -> Telegram)
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
