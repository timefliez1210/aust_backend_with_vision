//! Public submission handlers — photo webapp, mobile app, and video inquiry endpoints.
//!
//! These endpoints accept multipart uploads from unauthenticated end-users and
//! feed into the vision pipeline → offer generation → Telegram approval workflow.

use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::repositories::{address_repo, customer_repo, estimation_repo, inquiry_repo};
use crate::services::offer_pipeline::try_auto_generate_offer;
use crate::{services, ApiError, AppState};
use aust_core::models::{EstimationMethod, Services};
use aust_storage::StorageProvider;

// ---------------------------------------------------------------------------
// Router constructor
// ---------------------------------------------------------------------------

/// Public submission routes (no auth required).
///
/// **Caller**: `crates/api/src/routes/mod.rs` public route tree.
/// **Why**: Photo webapp and mobile app endpoints accept multipart uploads from
///          unauthenticated end-users.
pub fn submit_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/photo", post(photo_inquiry))
        .route("/mobile", post(mobile_inquiry))
        .route("/mobile/ar", post(ar_inquiry))
        .route("/video", post(video_inquiry))
        .route("/manual", post(manual_inquiry))
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Response returned from both /photo and /mobile endpoints.
#[derive(Serialize)]
pub(crate) struct SubmitInquiryResponse {
    pub inquiry_id: Uuid,
    pub customer_id: Uuid,
    pub status: String,
    pub message: String,
}

/// All parsed fields from the multipart form.
pub(crate) struct ParsedInquiryForm {
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    // New unified-model fields
    pub customer_type: Option<String>,
    pub company_name: Option<String>,
    pub service_type: Option<String>,
    pub submission_mode: Option<String>,
    // Recipient (Leistungsempfänger) — when booking for someone else
    pub recipient_salutation: Option<String>,
    pub recipient_first_name: Option<String>,
    pub recipient_last_name: Option<String>,
    pub recipient_phone: Option<String>,
    pub recipient_email: Option<String>,
    // Billing address (when different from origin/destination)
    pub billing_street: Option<String>,
    pub billing_number: Option<String>,
    pub billing_postal: Option<String>,
    pub billing_city: Option<String>,
    // Address fields
    pub departure_address: Option<String>,
    pub departure_city: Option<String>,
    pub departure_postal: Option<String>,
    pub departure_floor: Option<String>,
    pub departure_parking_ban: Option<bool>,
    pub departure_elevator: Option<bool>,
    pub arrival_address: Option<String>,
    pub arrival_city: Option<String>,
    pub arrival_postal: Option<String>,
    pub arrival_floor: Option<String>,
    pub arrival_parking_ban: Option<bool>,
    pub arrival_elevator: Option<bool>,
    /// Volume in m³ provided by customer (manuell mode)
    pub volumen: Option<String>,
    /// Item list text provided by customer (manuell mode)
    pub umzugsgut: Option<String>,
    /// Volume in m³ (from photo/video mode)
    pub volume_m3: Option<String>,
    /// Item list text (from photo/video mode)
    pub items_list: Option<String>,
    /// The date the customer wants to move. Also accepts `preferred_date`
    /// (legacy alias) in JSON submissions via manual parsing; `wunschtermin` in multipart forms.
    pub scheduled_date: Option<String>,
    pub services: Option<String>,
    pub message: Option<String>,
    pub images: Vec<(Vec<u8>, String)>,
    pub depth_maps: Vec<(Vec<u8>, String)>,
    pub ar_metadata: Option<String>,
    /// `[{"label":"Sofa","frame_count":5}, …]` — tells the backend which frames belong to which item.
    pub item_manifest: Option<String>,
    /// Flat JSON array of 16-float pose matrices in the same order as `images`.
    pub poses: Option<String>,
    /// Camera intrinsics JSON — `{fx,fy,cx,cy,width,height}`.
    pub intrinsics: Option<String>,
}

// ---------------------------------------------------------------------------
// Submission handlers (public, no auth)
// ---------------------------------------------------------------------------

/// POST /submit/photo -- Photo webapp inquiry (Source C).
async fn photo_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, false).await?;
    handle_submission(state, parsed, "photo_webapp", "foto").await
}

/// POST /submit/mobile -- Mobile app inquiry (Source D).
async fn mobile_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, true).await?;
    handle_submission(state, parsed, "mobile_app", "mobile").await
}

/// `POST /api/v1/submit/mobile/ar` — AR per-item mobile app inquiry (Source D variant).
///
/// **Caller**: Mobile app scan → form screen after AR capture session completes.
/// **Why**: Structured multi-view input from the native AR scan plugin. Each item
///          has 4-8 RGB frames (+ optional LiDAR depth maps) taken at 28° arc sweep.
///          The backend uploads them to S3 grouped by item, then forwards to the
///          Modal AR pipeline for per-item 3D reconstruction.
async fn ar_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, true).await?;
    handle_ar_submission(state, parsed).await
}

/// Shared handler for AR mobile submissions.
///
/// **Caller**: `ar_inquiry`
/// **Why**: Validates fields, creates customer/addresses/inquiry (same as `handle_submission`),
///          uploads AR frames to S3 under a grouped layout, stores source_data, then spawns
///          `process_ar_submission_background`.
///
/// # Errors
/// Returns `ApiError::Validation` for missing required fields, `ApiError::Internal` for DB/S3.
async fn handle_ar_submission(
    state: Arc<AppState>,
    form: ParsedInquiryForm,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
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

    // 1. Upsert customer
    let customer_id = customer_repo::upsert(
        &state.db,
        &email,
        Some(&name),
        form.salutation.as_deref(),
        form.first_name.as_deref(),
        form.last_name.as_deref(),
        form.phone.as_deref(),
        form.customer_type.as_deref(),
        form.company_name.as_deref(),
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    // 2. Create addresses
    let (dep_street, dep_city, dep_postal) = merge_address_parts(
        services::vision::parse_address(&departure_address),
        form.departure_city.as_deref(),
        form.departure_postal.as_deref(),
    );
    let origin_id = address_repo::create(
        &state.db,
        &dep_street,
        &dep_city,
        Some(dep_postal.as_str()).filter(|s| !s.is_empty()),
        form.departure_floor.as_deref(),
        form.departure_elevator,
        None,  // house_number not available from parsed address string
        form.departure_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    let (arr_street, arr_city, arr_postal) = merge_address_parts(
        services::vision::parse_address(&arrival_address),
        form.arrival_city.as_deref(),
        form.arrival_postal.as_deref(),
    );
    let dest_id = address_repo::create(
        &state.db,
        &arr_street,
        &arr_city,
        Some(arr_postal.as_str()).filter(|s| !s.is_empty()),
        form.arrival_floor.as_deref(),
        form.arrival_elevator,
        None,  // house_number not available from parsed address string
        form.arrival_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

    let scheduled_date_naive = form
        .scheduled_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

    let notes = build_notes(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
        form.message.as_deref(),
    );
    let services_struct = parse_services_string(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
    );
    let services_json = serde_json::to_value(&services_struct).unwrap_or(serde_json::json!({}));

    // Create recipient customer if provided (booking for someone else)
    let recipient_id = if form.recipient_last_name.as_deref().map_or(false, |s| !s.trim().is_empty()) {
        Some(customer_repo::create_recipient(
            &state.db,
            form.recipient_salutation.as_deref(),
            form.recipient_first_name.as_deref(),
            form.recipient_last_name.as_deref(),
            form.recipient_phone.as_deref(),
            form.recipient_email.as_deref(),
            now,
        ).await?)
    } else {
        None
    };

    // Create billing address if provided
    let billing_address_id = if form.billing_street.is_some() || form.billing_city.is_some() {
        let b_street = form.billing_street.as_deref().unwrap_or("");
        let b_city = form.billing_city.as_deref().unwrap_or("");
        if !b_street.is_empty() && !b_city.is_empty() {
            Some(address_repo::create(
                &state.db,
                b_street,
                b_city,
                form.billing_postal.as_deref().filter(|s| !s.is_empty()),
                None,
                None,
                form.billing_number.as_deref(),
                Some(false),
            ).await.map_err(|e| ApiError::Internal(format!("Rechnungsadresse konnte nicht erstellt werden: {e}")))?)
        } else {
            None
        }
    } else {
        None
    };

    // 3. Create inquiry
    let inquiry_id = Uuid::now_v7();
    inquiry_repo::create_minimal(
        &state.db,
        inquiry_id,
        customer_id,
        Some(origin_id),
        Some(dest_id),
        "pending",
        scheduled_date_naive,
        Some(&notes),
        Some(&services_json),
        "mobile_app_ar",
        form.service_type.as_deref(),      // service_type
        Some("ar"),                       // submission_mode
        recipient_id,                      // recipient_id
        billing_address_id,                 // billing_address_id
        None,                              // custom_fields
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    // 4. Pre-create estimation row
    let estimation_id = Uuid::now_v7();
    estimation_repo::create_processing(&state.db, estimation_id, inquiry_id, "ar")
        .await
        .map_err(|e| ApiError::Internal(format!("Schätzung konnte nicht erstellt werden: {e}")))?;

    // 5. Upload RGB frames to S3 synchronously so admin UI shows images immediately.
    //    Layout: estimates/{inquiry_id}/{est_id}/ar/{idx}.jpg
    let s3_rgb_keys: Vec<String> = {
        let mut keys = Vec::with_capacity(form.images.len());
        for (idx, (data, mime_type)) in form.images.iter().enumerate() {
            let key = format!("estimates/{inquiry_id}/{estimation_id}/ar/{idx}.jpg");
            if let Err(e) = state.storage.upload(&key, Bytes::from(data.clone()), mime_type).await {
                tracing::warn!(inquiry_id = %inquiry_id, "AR RGB frame {idx} upload failed: {e}");
            } else {
                keys.push(key);
            }
        }
        keys
    };

    // 6. Upload depth maps to S3.  Layout: …/ar/depth/{idx}.png
    let s3_depth_keys: Vec<String> = {
        let mut keys = Vec::with_capacity(form.depth_maps.len());
        for (idx, (data, mime_type)) in form.depth_maps.iter().enumerate() {
            let key = format!("estimates/{inquiry_id}/{estimation_id}/ar/depth/{idx}.png");
            if let Err(e) = state.storage.upload(&key, Bytes::from(data.clone()), mime_type).await {
                tracing::warn!(inquiry_id = %inquiry_id, "AR depth map {idx} upload failed: {e}");
            } else {
                keys.push(key);
            }
        }
        keys
    };

    // 7. Persist source_data so admin UI shows AR context before Modal finishes.
    let item_manifest_json: serde_json::Value = form
        .item_manifest
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::json!([]));

    let source_data = serde_json::json!({
        "source": "mobile_app_ar",
        "image_count": form.images.len(),
        "depth_map_count": form.depth_maps.len(),
        "s3_rgb_keys": &s3_rgb_keys,
        "s3_depth_keys": &s3_depth_keys,
        "item_manifest": &item_manifest_json,
    });
    let _ = estimation_repo::update_source_data(&state.db, estimation_id, &source_data).await;

    tracing::info!(
        inquiry_id = %inquiry_id,
        image_count = form.images.len(),
        depth_count = form.depth_maps.len(),
        "AR inquiry created, spawning background processing"
    );

    // 8. Spawn background processing
    let state_bg = Arc::clone(&state);
    let dep_addr = departure_address.clone();
    let arr_addr = arrival_address.clone();
    let images = form.images;
    let item_manifest_str = form.item_manifest.unwrap_or_default();
    let intrinsics_str = form.intrinsics;
    let poses_str = form.poses;
    tokio::spawn(async move {
        if let Err(e) = process_ar_submission_background(
            Arc::clone(&state_bg),
            inquiry_id,
            estimation_id,
            images,
            item_manifest_str,
            intrinsics_str,
            poses_str,
            s3_rgb_keys,
            s3_depth_keys,
            item_manifest_json,
            dep_addr,
            arr_addr,
        )
        .await
        {
            tracing::error!(inquiry_id = %inquiry_id, error = %e, "AR background processing failed");
            let _ = estimation_repo::mark_failed(&state_bg.db, estimation_id).await;
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitInquiryResponse {
            inquiry_id,
            customer_id,
            status: "processing".to_string(),
            message: "Anfrage erhalten. AR-Aufnahmen werden analysiert und Angebot wird erstellt."
                .to_string(),
        }),
    ))
}

/// Background task for AR submissions: distance calc → semaphore → Modal AR pipeline
/// → store estimation → update inquiry → offer generation.
///
/// **Caller**: `handle_ar_submission` via `tokio::spawn`
/// **Why**: Same async submit/poll pattern as `process_submission_background` and
///          `process_video_background`. Images are sent as raw bytes to Modal (already
///          uploaded to S3 for admin UI — Modal receives bytes directly to avoid
///          needing S3 credentials in the no-GPU `serve()` container).
///
/// # Errors
/// Returns `Err(String)` on any fatal failure; caller marks the estimation 'failed'.
async fn process_ar_submission_background(
    state: Arc<AppState>,
    inquiry_id: Uuid,
    estimation_id: Uuid,
    images: Vec<(Vec<u8>, String)>,
    item_manifest: String,
    intrinsics: Option<String>,
    poses: Option<String>,
    s3_rgb_keys: Vec<String>,
    s3_depth_keys: Vec<String>,
    item_manifest_json: serde_json::Value,
    departure_address: String,
    arrival_address: String,
) -> Result<(), String> {
    // 1. Distance calculation
    let api_key = &state.config.maps.api_key;
    if !api_key.is_empty() {
        let calculator = aust_distance_calculator::RouteCalculator::new(api_key.clone());
        let request = aust_distance_calculator::RouteRequest {
            addresses: vec![departure_address, arrival_address],
        };
        match calculator.calculate(&request).await {
            Ok(result) => {
                let _ = inquiry_repo::update_distance(
                    &state.db, inquiry_id, result.total_distance_km,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(
                    inquiry_id = %inquiry_id,
                    error = %e,
                    "AR distance calculation failed, continuing"
                );
            }
        }
    }

    // 2. Acquire vision semaphore
    let _permit = state
        .vision_semaphore
        .acquire()
        .await
        .map_err(|e| format!("Vision semaphore closed: {e}"))?;
    tracing::info!(estimation_id = %estimation_id, "AR vision semaphore acquired, submitting to Modal");

    // 3. Submit to Modal AR endpoint and poll for result
    let client = state
        .vision_service
        .as_ref()
        .ok_or("Vision service not configured")?;

    let poll_interval =
        std::time::Duration::from_secs(state.config.vision_service.poll_interval_secs);
    let max_polls = state.config.vision_service.max_polls;
    let max_retries = state.config.vision_service.max_retries;

    let response = client
        .estimate_ar_async(
            &estimation_id.to_string(),
            &images,
            &item_manifest,
            intrinsics.as_deref(),
            poses.as_deref(),
            poll_interval,
            max_polls,
            max_retries,
        )
        .await
        .map_err(|e| {
            tracing::error!(
                inquiry_id = %inquiry_id,
                estimation_id = %estimation_id,
                "AR estimation failed after all retries — manual intervention required: {e}"
            );
            format!("AR estimation failed: {e}")
        })?;

    tracing::info!(
        estimation_id = %estimation_id,
        volume = response.total_volume_m3,
        items = response.detected_items.len(),
        "AR estimation succeeded"
    );

    // 4. Persist estimation result
    let source_data = serde_json::json!({
        "source": "mobile_app_ar",
        "s3_rgb_keys": &s3_rgb_keys,
        "s3_depth_keys": &s3_depth_keys,
        "item_manifest": &item_manifest_json,
        "has_depth": !s3_depth_keys.is_empty(),
    });
    let result_data = serde_json::to_value(&response.detected_items)
        .map_err(|e| format!("Failed to serialize AR items: {e}"))?;

    let now = chrono::Utc::now();
    estimation_repo::upsert(
        &state.db,
        estimation_id,
        inquiry_id,
        EstimationMethod::DepthSensor.as_str(),
        &source_data,
        Some(&result_data),
        response.total_volume_m3,
        response.confidence_score,
        now,
    )
    .await
    .map_err(|e| format!("Failed to store AR estimation: {e}"))?;

    // 5. Update inquiry volume and advance status
    inquiry_repo::update_volume_and_status(
        &state.db,
        inquiry_id,
        response.total_volume_m3,
        "estimated",
        now,
    )
    .await
    .map_err(|e| format!("Failed to update AR inquiry: {e}"))?;

    // 6. Auto-generate offer (XLSX → PDF → Telegram)
    try_auto_generate_offer(Arc::clone(&state), inquiry_id).await;

    Ok(())
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
    let mut departure_city: Option<String> = None;
    let mut departure_postal: Option<String> = None;
    let mut departure_floor: Option<String> = None;
    let mut departure_elevator: Option<bool> = None;
    let mut departure_parking_ban: Option<bool> = None;
    let mut arrival_address: Option<String> = None;
    let mut arrival_city: Option<String> = None;
    let mut arrival_postal: Option<String> = None;
    let mut arrival_floor: Option<String> = None;
    let mut arrival_elevator: Option<bool> = None;
    let mut arrival_parking_ban: Option<bool> = None;
    let mut scheduled_date: Option<String> = None;
    let mut services_text: Option<String> = None;
    let mut message: Option<String> = None;
    // Unified model fields
    let mut customer_type: Option<String> = None;
    let mut company_name: Option<String> = None;
    let mut service_type: Option<String> = None;
    let mut submission_mode: Option<String> = None;
    let mut recipient_salutation: Option<String> = None;
    let mut recipient_first_name: Option<String> = None;
    let mut recipient_last_name: Option<String> = None;
    let mut recipient_phone: Option<String> = None;
    let mut recipient_email: Option<String> = None;
    let mut billing_street: Option<String> = None;
    let mut billing_number: Option<String> = None;
    let mut billing_postal: Option<String> = None;
    let mut billing_city: Option<String> = None;
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
            "startOrt" => departure_city = Some(read_text_field(field).await?),
            "startPlz" => departure_postal = Some(read_text_field(field).await?),
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
            "endOrt" => arrival_city = Some(read_text_field(field).await?),
            "endPlz" => arrival_postal = Some(read_text_field(field).await?),
            "etage_einzug" | "arrival_floor" => arrival_floor = Some(read_text_field(field).await?),
            "aufzug_einzug" | "arrival_elevator" => {
                let t = read_text_field(field).await?;
                arrival_elevator = Some(parse_bool_field(&t));
            }
            "halteverbot_einzug" | "arrival_parking_ban" => {
                let t = read_text_field(field).await?;
                arrival_parking_ban = Some(parse_bool_field(&t));
            }
            "wunschtermin" | "preferred_date" | "scheduled_date" => scheduled_date = Some(read_text_field(field).await?),
            "zusatzleistungen" | "services" | "services[]" => services_text = Some(read_text_field(field).await?),
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
            // Unified model fields (video form)
            "customer_type" => customer_type = Some(read_text_field(field).await?),
            "company_name" | "firmenname" => company_name = Some(read_text_field(field).await?),
            "service_type" => service_type = Some(read_text_field(field).await?),
            "submission_mode" => submission_mode = Some(read_text_field(field).await?),
            "recipient_salutation" => recipient_salutation = Some(read_text_field(field).await?),
            "recipient_first_name" => recipient_first_name = Some(read_text_field(field).await?),
            "recipient_last_name" => recipient_last_name = Some(read_text_field(field).await?),
            "recipient_phone" => recipient_phone = Some(read_text_field(field).await?),
            "recipient_email" => recipient_email = Some(read_text_field(field).await?),
            "billing_street" => billing_street = Some(read_text_field(field).await?),
            "billing_number" | "billing_house_number" => billing_number = Some(read_text_field(field).await?),
            "billing_postal" | "billing_zip" | "billing_plz" => billing_postal = Some(read_text_field(field).await?),
            "billing_city" | "billing_ort" => billing_city = Some(read_text_field(field).await?),
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
    let customer_id = customer_repo::upsert(
        &state.db,
        &email,
        Some(&name),
        salutation.as_deref(),
        first_name.as_deref(),
        last_name.as_deref(),
        phone.as_deref(),
        customer_type.as_deref(),
        company_name.as_deref(),
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    // Create addresses
    let (dep_street, dep_city, dep_postal) = merge_address_parts(
        services::vision::parse_address(&departure_address),
        departure_city.as_deref(),
        departure_postal.as_deref(),
    );
    let origin_id = address_repo::create(
        &state.db,
        &dep_street,
        &dep_city,
        Some(dep_postal.as_str()).filter(|s| !s.is_empty()),
        departure_floor.as_deref(),
        departure_elevator,
        None,
        departure_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    let (arr_street, arr_city, arr_postal) = merge_address_parts(
        services::vision::parse_address(&arrival_address),
        arrival_city.as_deref(),
        arrival_postal.as_deref(),
    );
    let dest_id = address_repo::create(
        &state.db,
        &arr_street,
        &arr_city,
        Some(arr_postal.as_str()).filter(|s| !s.is_empty()),
        arrival_floor.as_deref(),
        arrival_elevator,
        None,
        arrival_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

    let scheduled_date_naive = scheduled_date.as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

    let notes = build_notes(
        services_text.as_deref(),
        departure_parking_ban,
        arrival_parking_ban,
        message.as_deref(),
    );

    // Parse services into structured JSON
    let services_struct = parse_services_string(
        services_text.as_deref(),
        departure_parking_ban,
        arrival_parking_ban,
    );
    let services_json = serde_json::to_value(&services_struct).unwrap_or(serde_json::json!({}));

    // Create recipient customer if provided (booking for someone else)
    let recipient_id = if recipient_last_name.as_deref().map_or(false, |s| !s.trim().is_empty()) {
        Some(customer_repo::create_recipient(
            &state.db,
            recipient_salutation.as_deref(),
            recipient_first_name.as_deref(),
            recipient_last_name.as_deref(),
            recipient_phone.as_deref(),
            recipient_email.as_deref(),
            now,
        ).await?)
    } else {
        None
    };

    // Create billing address if provided
    let billing_address_id = if billing_street.is_some() || billing_city.is_some() {
        let b_street = billing_street.as_deref().unwrap_or("");
        let b_city = billing_city.as_deref().unwrap_or("");
        if !b_street.is_empty() && !b_city.is_empty() {
            Some(address_repo::create(
                &state.db,
                b_street,
                b_city,
                billing_postal.as_deref().filter(|s| !s.is_empty()),
                None,
                None,
                billing_number.as_deref(),
                Some(false),
            ).await.map_err(|e| ApiError::Internal(format!("Rechnungsadresse konnte nicht erstellt werden: {e}")))?)
        } else {
            None
        }
    } else {
        None
    };

    // Create inquiry
    let inquiry_id = Uuid::now_v7();
    inquiry_repo::create_minimal(
        &state.db,
        inquiry_id,
        customer_id,
        Some(origin_id),
        Some(dest_id),
        "pending",
        scheduled_date_naive,
        Some(&notes),
        Some(&services_json),
        "video_webapp",
        service_type.as_deref(),
        Some("video"),  // submission_mode
        recipient_id,
        billing_address_id,
        None,  // custom_fields
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    // Pre-create one estimation row per uploaded video and upload each video to S3
    // synchronously before returning 202, so the frontend can reference the files
    // while Modal processes them.
    let mut estimation_ids: Vec<Uuid> = Vec::new();
    let mut s3_keys_per_video: Vec<String> = Vec::new();
    for (video_bytes, mime_type) in &video_files {
        let eid = Uuid::now_v7();
        estimation_repo::create_processing(&state.db, eid, inquiry_id, "video")
            .await
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
            let _ = estimation_repo::update_source_data(&state.db, eid, &source_data).await;
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
                    let _ = inquiry_repo::update_distance(&state_bg.db, inquiry_id, r.total_distance_km).await;
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
                let _ = estimation_repo::mark_failed(&state_bg.db, estimation_id).await;
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

/// `POST /api/v1/submit/manual` — Public manual inquiry (no images/video required).
///
/// **Caller**: foto-angebot "manuell" mode, other public forms that don't upload media.
/// **Why**: Public customers need a way to submit inquiries without going through the admin API.
async fn manual_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<SubmitInquiryResponse>), ApiError> {
    let form = parse_inquiry_form(multipart, false).await?;

    // Validate required fields
    let name = form
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Name ist erforderlich".into()))?;
    let email = form
        .email
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("E-Mail ist erforderlich".into()))?;
    let departure_address = form
        .departure_address
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Auszugsadresse ist erforderlich".into()))?;
    let arrival_address = form
        .arrival_address
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::Validation("Einzugsadresse ist erforderlich".into()))?;

    let now = chrono::Utc::now();

    // 1. Upsert customer
    let customer_id = customer_repo::upsert(
        &state.db,
        email,
        Some(name),
        form.salutation.as_deref(),
        form.first_name.as_deref(),
        form.last_name.as_deref(),
        form.phone.as_deref(),
        form.customer_type.as_deref(),
        form.company_name.as_deref(),
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    // 2. Create origin address
    let (dep_street, dep_city, dep_postal) = merge_address_parts(
        services::vision::parse_address(departure_address),
        form.departure_city.as_deref(),
        form.departure_postal.as_deref(),
    );
    let origin_id = address_repo::create(
        &state.db,
        &dep_street,
        &dep_city,
        Some(dep_postal.as_str()).filter(|s| !s.is_empty()),
        form.departure_floor.as_deref(),
        form.departure_elevator,
        None,
        form.departure_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    // 3. Create destination address
    let (arr_street, arr_city, arr_postal) = merge_address_parts(
        services::vision::parse_address(arrival_address),
        form.arrival_city.as_deref(),
        form.arrival_postal.as_deref(),
    );
    let dest_id = address_repo::create(
        &state.db,
        &arr_street,
        &arr_city,
        Some(arr_postal.as_str()).filter(|s| !s.is_empty()),
        form.arrival_floor.as_deref(),
        form.arrival_elevator,
        None,
        form.arrival_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

    // 3b. Create billing address if provided
    let billing_address_id = if form.billing_street.is_some() || form.billing_city.is_some() {
        let b_street = form.billing_street.as_deref().unwrap_or("");
        let b_city = form.billing_city.as_deref().unwrap_or("");
        if !b_street.is_empty() && !b_city.is_empty() {
            Some(address_repo::create(
                &state.db,
                b_street,
                b_city,
                form.billing_postal.as_deref().filter(|s| !s.is_empty()),
                None,
                None,
                form.billing_number.as_deref(),
                Some(false),
            ).await.map_err(|e| ApiError::Internal(format!("Rechnungsadresse konnte nicht erstellt werden: {e}")))?)
        } else {
            None
        }
    } else {
        None
    };

    // 4. Parse scheduled date
    let scheduled_date_naive = form
        .scheduled_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

    // 5. Build notes from services, parking bans, and message
    let notes = build_notes(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
        form.message.as_deref(),
    );

    let services_struct = parse_services_string(
        form.services.as_deref(),
        form.departure_parking_ban,
        form.arrival_parking_ban,
    );
    let services_json = serde_json::to_value(&services_struct).unwrap_or(serde_json::json!({}));

    // Create recipient customer if provided (booking for someone else)
    let recipient_id = if form.recipient_last_name.as_deref().map_or(false, |s| !s.trim().is_empty()) {
        Some(customer_repo::create_recipient(
            &state.db,
            form.recipient_salutation.as_deref(),
            form.recipient_first_name.as_deref(),
            form.recipient_last_name.as_deref(),
            form.recipient_phone.as_deref(),
            form.recipient_email.as_deref(),
            now,
        ).await?)
    } else {
        None
    };

    // 6. Create inquiry
    let inquiry_id = Uuid::now_v7();
    inquiry_repo::create_minimal(
        &state.db,
        inquiry_id,
        customer_id,
        Some(origin_id),
        Some(dest_id),
        "pending",
        scheduled_date_naive,
        Some(&notes),
        Some(&services_json),
        "manuell",
        form.service_type.as_deref(),
        Some("manuell"),  // submission_mode
        recipient_id,                    // recipient_id
        billing_address_id,
        None,              // custom_fields
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    tracing::info!(inquiry_id = %inquiry_id, "Manual inquiry created for submission");

    // 7. Trigger auto-offer and distance calculation in background
    let state_bg = Arc::clone(&state);
    tokio::spawn(async move {
        try_auto_generate_offer(Arc::clone(&state_bg), inquiry_id).await;
    });

    Ok((
        StatusCode::CREATED,
        Json(SubmitInquiryResponse {
            inquiry_id,
            customer_id,
            status: "pending".to_string(),
            message: "Anfrage erstellt. Wir melden uns mit einem Angebot.".to_string(),
        }),
    ))
}


/// Parse the multipart form data into a structured form.
pub(crate) async fn parse_inquiry_form(
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
        customer_type: None,
        company_name: None,
        service_type: None,
        submission_mode: None,
        recipient_salutation: None,
        recipient_first_name: None,
        recipient_last_name: None,
        recipient_phone: None,
        recipient_email: None,
        billing_street: None,
        billing_number: None,
        billing_postal: None,
        billing_city: None,
        departure_address: None,
        departure_city: None,
        departure_postal: None,
        departure_floor: None,
        departure_parking_ban: None,
        departure_elevator: None,
        arrival_address: None,
        arrival_city: None,
        arrival_postal: None,
        arrival_floor: None,
        arrival_parking_ban: None,
        arrival_elevator: None,
        scheduled_date: None,
        volumen: None,
        umzugsgut: None,
        volume_m3: None,
        items_list: None,
        services: None,
        message: None,
        images: Vec::new(),
        depth_maps: Vec::new(),
        ar_metadata: None,
        item_manifest: None,
        poses: None,
        intrinsics: None,
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
            "wunschtermin" | "preferred_date" | "scheduled_date" => {
                form.scheduled_date = Some(read_text_field(field).await?);
            }
            "services" | "zusatzleistungen" | "services[]" => {
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
            "item_manifest" if accept_depth => {
                form.item_manifest = Some(read_text_field(field).await?);
            }
            "poses" if accept_depth => {
                form.poses = Some(read_text_field(field).await?);
            }
            "intrinsics" if accept_depth => {
                form.intrinsics = Some(read_text_field(field).await?);
            }
            // Unified model fields
            "customer_type" => form.customer_type = Some(read_text_field(field).await?),
            "company_name" | "firmenname" => form.company_name = Some(read_text_field(field).await?),
            "service_type" => form.service_type = Some(read_text_field(field).await?),
            "submission_mode" => form.submission_mode = Some(read_text_field(field).await?),
            "recipient_salutation" => form.recipient_salutation = Some(read_text_field(field).await?),
            "recipient_first_name" => form.recipient_first_name = Some(read_text_field(field).await?),
            "recipient_last_name" => form.recipient_last_name = Some(read_text_field(field).await?),
            "recipient_phone" => form.recipient_phone = Some(read_text_field(field).await?),
            "recipient_email" => form.recipient_email = Some(read_text_field(field).await?),
            "billing_street" => form.billing_street = Some(read_text_field(field).await?),
            "billing_number" | "billing_house_number" => form.billing_number = Some(read_text_field(field).await?),
            "billing_postal" | "billing_zip" | "billing_plz" => form.billing_postal = Some(read_text_field(field).await?),
            "billing_city" | "billing_ort" => form.billing_city = Some(read_text_field(field).await?),
            // Separate city/postal fields from manuell mode
            "startOrt" => form.departure_city = Some(read_text_field(field).await?),
            "startPlz" => form.departure_postal = Some(read_text_field(field).await?),
            "endOrt" => form.arrival_city = Some(read_text_field(field).await?),
            "endPlz" => form.arrival_postal = Some(read_text_field(field).await?),
            // Volume and items from different submission modes
            "volumen" | "volume" => form.volumen = Some(read_text_field(field).await?),
            "umzugsgut" | "items" => form.umzugsgut = Some(read_text_field(field).await?),
            "volume_m3" => form.volume_m3 = Some(read_text_field(field).await?),
            "items_list" => form.items_list = Some(read_text_field(field).await?),
            _ => continue,
        }
    }

    Ok(form)
}

pub(crate) async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> Result<String, ApiError> {
    field
        .text()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Feld konnte nicht gelesen werden: {e}")))
}

/// Merge parsed address with separate city/postal fields.
/// Explicit fields (from manuell mode) override parsed values.
pub(crate) fn merge_address_parts(
    parsed: (String, String, String),
    city: Option<&str>,
    postal: Option<&str>,
) -> (String, String, String) {
    let (street, mut parsed_city, mut parsed_postal) = parsed;
    if let Some(c) = city.filter(|s| !s.trim().is_empty()) {
        parsed_city = c.trim().to_string();
    }
    if let Some(p) = postal.filter(|s| !s.trim().is_empty()) {
        parsed_postal = p.trim().to_string();
    }
    (street, parsed_city, parsed_postal)
}

pub(crate) fn parse_bool_field(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "true" | "1" | "yes" | "ja"
    )
}

/// Shared handler for both photo and mobile submissions.
pub(crate) async fn handle_submission(
    state: Arc<AppState>,
    form: ParsedInquiryForm,
    source: &str,
    submission_mode: &str,
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
    let customer_id = customer_repo::upsert(
        &state.db,
        &email,
        Some(&name),
        form.salutation.as_deref(),
        form.first_name.as_deref(),
        form.last_name.as_deref(),
        form.phone.as_deref(),
        form.customer_type.as_deref(),
        form.company_name.as_deref(),
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Kunde konnte nicht erstellt werden: {e}")))?;

    tracing::info!(customer_id = %customer_id, email = %email, "Customer created/updated");

    // 2. Create origin address
    let (dep_street, dep_city, dep_postal) = merge_address_parts(
        services::vision::parse_address(&departure_address),
        form.departure_city.as_deref(),
        form.departure_postal.as_deref(),
    );
    let origin_id = address_repo::create(
        &state.db,
        &dep_street,
        &dep_city,
        Some(dep_postal.as_str()).filter(|s| !s.is_empty()),
        form.departure_floor.as_deref(),
        form.departure_elevator,
        None,
        form.departure_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    // 3. Create destination address
    let (arr_street, arr_city, arr_postal) = merge_address_parts(
        services::vision::parse_address(&arrival_address),
        form.arrival_city.as_deref(),
        form.arrival_postal.as_deref(),
    );
    let dest_id = address_repo::create(
        &state.db,
        &arr_street,
        &arr_city,
        Some(arr_postal.as_str()).filter(|s| !s.is_empty()),
        form.arrival_floor.as_deref(),
        form.arrival_elevator,
        None,
        form.arrival_parking_ban,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

    // 4. Parse preferred date
    let scheduled_date_naive = form
        .scheduled_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

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

    // Create billing address if provided
    let billing_address_id = if form.billing_street.is_some() || form.billing_city.is_some() {
        let b_street = form.billing_street.as_deref().unwrap_or("");
        let b_city = form.billing_city.as_deref().unwrap_or("");
        if !b_street.is_empty() && !b_city.is_empty() {
            Some(address_repo::create(
                &state.db,
                b_street,
                b_city,
                form.billing_postal.as_deref().filter(|s| !s.is_empty()),
                None,
                None,
                form.billing_number.as_deref(),
                Some(false),
            ).await.map_err(|e| ApiError::Internal(format!("Rechnungsadresse konnte nicht erstellt werden: {e}")))?)
        } else {
            None
        }
    } else {
        None
    };


    // 6b. Create recipient customer if provided (booking for someone else)
    let recipient_id = if form.recipient_last_name.as_deref().map_or(false, |s| !s.trim().is_empty()) {
        Some(customer_repo::create_recipient(
            &state.db,
            form.recipient_salutation.as_deref(),
            form.recipient_first_name.as_deref(),
            form.recipient_last_name.as_deref(),
            form.recipient_phone.as_deref(),
            form.recipient_email.as_deref(),
            now,
        ).await?)
    } else {
        None
    };

    // 6c. Create inquiry
    let inquiry_id = Uuid::now_v7();
    inquiry_repo::create_minimal(
        &state.db,
        inquiry_id,
        customer_id,
        Some(origin_id),
        Some(dest_id),
        "pending",
        scheduled_date_naive,
        Some(&notes),
        Some(&services_json),
        source,
        form.service_type.as_deref(),      // service_type
        Some(submission_mode),              // submission_mode
        recipient_id,                       // recipient_id
        billing_address_id,                 // billing_address_id
        None,                              // custom_fields
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Anfrage konnte nicht erstellt werden: {e}")))?;

    tracing::info!(inquiry_id = %inquiry_id, "Inquiry created for submission");

    // 7. Check for manual volume (manuell mode) — skip vision pipeline if provided.
    let manual_volume: Option<f64> = form.volumen.as_deref()
        .or(form.volume_m3.as_deref())
        .and_then(|s| s.trim().parse::<f64>().ok());

    if manual_volume.is_some() && form.images.is_empty() {
        // Fast path: customer provided volume directly, no vision pipeline needed.
        let volume = manual_volume.unwrap();
        let now_update = chrono::Utc::now();
        inquiry_repo::update_volume_and_status(&state.db, inquiry_id, volume, "estimated", now_update).await
            .map_err(|e| ApiError::Internal(format!("Volumen konnte nicht gespeichert werden: {e}")))?;

        // Create completed estimation row with manual method
        let estimation_id = Uuid::now_v7();
        let items_text = form.umzugsgut.as_deref().or(form.items_list.as_deref()).unwrap_or("");
        let source_data = serde_json::json!({
            "source": "manual",
            "submission_mode": submission_mode
        });
        let result_data = serde_json::json!({
            "items_text": items_text,
            "total_volume_m3": volume
        });
        estimation_repo::insert_no_return(
            &state.db,
            estimation_id,
            inquiry_id,
            "manual",
            &source_data,
            Some(&result_data),
            volume,
            0.5,  // moderate confidence — customer-provided
            now,
        ).await
            .map_err(|e| ApiError::Internal(format!("Schätzung konnte nicht erstellt werden: {e}")))?;

        // Spawn distance calculation (same as AR handler)
        let state_bg = Arc::clone(&state);
        let dep_addr = departure_address.clone();
        let arr_addr = arrival_address.clone();
        tokio::spawn(async move {
            try_auto_generate_offer(Arc::clone(&state_bg), inquiry_id).await;
            let api_key = &state_bg.config.maps.api_key;
            if !api_key.is_empty() {
                let calculator = aust_distance_calculator::RouteCalculator::new(api_key.clone());
                let req = aust_distance_calculator::RouteRequest { addresses: vec![dep_addr, arr_addr] };
                if let Ok(r) = calculator.calculate(&req).await {
                    let _ = inquiry_repo::update_distance(&state_bg.db, inquiry_id, r.total_distance_km).await;
                }
            }
        });

        return Ok((
            StatusCode::OK,
            Json(SubmitInquiryResponse {
                inquiry_id,
                customer_id,
                status: "estimated".to_string(),
                message: "Anfrage erhalten. Angebot wird erstellt.".to_string(),
            }),
        ));
    }

    // 8. Vision pipeline path: pre-create estimation row and upload images to S3.
    let estimation_id = Uuid::now_v7();

    // Pre-create estimation record with status='processing' so polling works immediately.
    estimation_repo::create_processing(&state.db, estimation_id, inquiry_id, "vision")
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
        let _ = estimation_repo::update_source_data(&state.db, estimation_id, &source_data).await;
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
pub(crate) async fn process_submission_background(
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
                let _ = inquiry_repo::update_distance(&state.db, inquiry_id, result.total_distance_km).await;
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
    estimation_repo::upsert(
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

    // 6. Update inquiry with estimated volume
    inquiry_repo::update_volume_and_status(&state.db, inquiry_id, total_volume, "estimated", now_update)
        .await
        .map_err(|e| format!("Failed to update inquiry: {e}"))?;

    tracing::info!(
        inquiry_id = %inquiry_id,
        estimation_id = %estimation_id,
        volume = total_volume,
        "Volume estimation completed"
    );

    // 7. Auto-generate offer (XLSX -> PDF -> Telegram)
    try_auto_generate_offer(Arc::clone(&state), inquiry_id).await;

    Ok(())
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
pub(crate) async fn process_video_background(
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

    estimation_repo::upsert(
        &state.db,
        estimation_id,
        inquiry_id,
        "video",
        &source_data,
        Some(&result_data),
        response.total_volume_m3,
        response.confidence_score,
        chrono::Utc::now(),
    )
    .await
    .map_err(|e| format!("Failed to store video estimation: {e}"))?;

    // 4. Update inquiry status and trigger offer generation
    let now_update = chrono::Utc::now();
    inquiry_repo::update_volume_and_status(&state.db, inquiry_id, response.total_volume_m3, "estimated", now_update)
        .await
        .map_err(|e| format!("Failed to update inquiry: {e}"))?;

    try_auto_generate_offer(Arc::clone(&state), inquiry_id).await;

    Ok(())
}

/// Upload depth maps to S3.
pub(crate) async fn upload_depth_maps_to_s3(
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

/// Convert a comma-separated services string (from multipart form) + parking ban flags
/// into a typed `Services` struct for JSONB storage.
pub(crate) fn parse_services_string(
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

/// Build notes string from services, parking bans, and optional message.
pub(crate) fn build_notes(
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
