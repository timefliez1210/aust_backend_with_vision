use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bytes::Bytes;
use serde::Deserialize;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::services::db::{insert_estimation, update_quote_volume};
use crate::{orchestrator, services, ApiError, AppState};
use aust_core::models::{EstimationMethod, InventoryForm, VolumeEstimation};
use aust_volume_estimator::VisionAnalyzer;

/// Register the public estimation routes (no auth required).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Image/video proxy must be public so `<img>` tags in the admin dashboard can
/// load estimation images directly without carrying an auth header.
pub fn public_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/images/{*key}", get(serve_image))
}

/// Register the protected estimation routes (require admin JWT).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly, nested under admin
/// JWT middleware.
/// **Why**: All write operations (running vision models, storing results) and the delete
/// endpoint must be admin-only.
pub fn protected_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/vision", post(vision_estimate))
        .route("/inventory", post(inventory_estimate))
        .route("/depth-sensor", post(depth_sensor_estimate))
        .route("/video", post(video_estimate))
        .route("/{id}", get(get_estimate).delete(delete_estimate))
}

#[derive(Debug, FromRow)]
struct VolumeEstimationRow {
    id: Uuid,
    inquiry_id: Uuid,
    method: String,
    status: String,
    source_data: serde_json::Value,
    result_data: Option<serde_json::Value>,
    total_volume_m3: Option<f64>,
    confidence_score: Option<f64>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl From<VolumeEstimationRow> for VolumeEstimation {
    fn from(row: VolumeEstimationRow) -> Self {
        let method = match row.method.as_str() {
            "vision" => EstimationMethod::Vision,
            "inventory" => EstimationMethod::Inventory,
            "depth_sensor" => EstimationMethod::DepthSensor,
            "video" => EstimationMethod::Video,
            "manual" => EstimationMethod::Manual,
            _ => EstimationMethod::Manual,
        };

        VolumeEstimation {
            id: row.id,
            inquiry_id: row.inquiry_id,
            method,
            status: row.status,
            source_data: row.source_data,
            result_data: row.result_data,
            total_volume_m3: row.total_volume_m3,
            confidence_score: row.confidence_score,
            created_at: row.created_at,
        }
    }
}

impl From<crate::services::db::EstimationRow> for VolumeEstimation {
    fn from(row: crate::services::db::EstimationRow) -> Self {
        let method = match row.method.as_str() {
            "vision" => EstimationMethod::Vision,
            "inventory" => EstimationMethod::Inventory,
            "depth_sensor" => EstimationMethod::DepthSensor,
            "video" => EstimationMethod::Video,
            "manual" => EstimationMethod::Manual,
            _ => EstimationMethod::Manual,
        };

        VolumeEstimation {
            id: row.id,
            inquiry_id: row.inquiry_id,
            method,
            status: row.status,
            source_data: row.source_data,
            result_data: row.result_data,
            total_volume_m3: row.total_volume_m3,
            confidence_score: row.confidence_score,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct VisionEstimateRequest {
    inquiry_id: Uuid,
    images: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    data: String,      // base64 encoded
    mime_type: String, // e.g., "image/jpeg"
}


/// `POST /api/v1/estimates/vision` — Run LLM image analysis on base64-encoded photos.
///
/// **Caller**: Axum router / admin dashboard and photo webapp (source C pipeline).
/// **Why**: Accepts one or more photos as base64 JSON, uploads them to S3 for later display,
/// runs `VisionAnalyzer` (LLM vision) on each image, stores a `volume_estimations` record,
/// updates the quote volume, and spawns a background task to auto-generate an offer.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, LLM provider, storage)
/// - `request` — JSON body with `inquiry_id` and `images` (array of `{data, mime_type}`)
///
/// # Returns
/// `200 OK` with the newly created `VolumeEstimation` JSON (status = "completed").
///
/// # Errors
/// - `400` if `images` is empty or base64 decoding fails
/// - `500` on LLM analysis, S3 upload, or DB failures
async fn vision_estimate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<VisionEstimateRequest>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    if request.images.is_empty() {
        return Err(ApiError::Validation("At least one image is required".into()));
    }

    let id = Uuid::now_v7();

    // Decode all images first
    let decoded: Vec<(Vec<u8>, String)> = request
        .images
        .iter()
        .map(|img| {
            let data = base64::engine::general_purpose::STANDARD
                .decode(&img.data)
                .map_err(|e| ApiError::Validation(format!("Invalid base64 image data: {e}")))?;
            Ok((data, img.mime_type.clone()))
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    // Upload images to S3 for future retrieval
    let s3_keys = services::vision::upload_images_to_s3(&*state.storage, request.inquiry_id, id, &decoded).await;
    let s3_keys = match s3_keys {
        Ok(keys) => keys,
        Err(e) => {
            tracing::warn!("Failed to upload vision images to S3, continuing with LLM analysis: {e}");
            Vec::new()
        }
    };

    // Run LLM analysis
    let analyzer = VisionAnalyzer::new(state.llm.clone());
    let mut total_volume = 0.0;
    let mut results = Vec::new();

    for (data, mime_type) in &decoded {
        let result = analyzer
            .analyze_image(data, mime_type)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        total_volume += result.total_volume_m3;
        results.push(result);
    }

    let now = chrono::Utc::now();
    let avg_confidence =
        results.iter().map(|r| r.confidence_score).sum::<f64>() / results.len() as f64;

    let source_data = serde_json::json!({
        "image_count": request.images.len(),
        "s3_keys": s3_keys,
    });
    let result_data = serde_json::to_value(&results).ok();

    let est = insert_estimation(
        &state.db,
        id,
        request.inquiry_id,
        EstimationMethod::Vision.as_str(),
        &source_data,
        result_data.as_ref(),
        total_volume,
        avg_confidence,
        now,
    )
    .await?;

    update_quote_volume(&state.db, request.inquiry_id, total_volume, "volume_estimated", now).await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    let qid = request.inquiry_id;
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, qid).await });

    Ok(Json(VolumeEstimation::from(est)))
}

/// `POST /api/v1/estimates/depth-sensor` — Run the 3D ML pipeline on multipart-uploaded images.
///
/// **Caller**: Axum router / mobile app depth-sensor flow (source D pipeline).
/// **Why**: Receives `multipart/form-data` with `inquiry_id` and one or more image files,
/// uploads images to S3, calls the Modal vision service (Grounding DINO + SAM 2 + depth),
/// and falls back to LLM vision analysis if the vision service is unavailable.
/// After storing results, auto-generates an offer in the background.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, LLM provider, vision service client, storage)
/// - `multipart` — multipart form with `inquiry_id` field + image file fields
///
/// # Returns
/// `200 OK` with the newly created `VolumeEstimation` JSON.
/// The `method` field reflects whether the ML pipeline or LLM fallback was used.
///
/// # Errors
/// - `400` if multipart parsing fails, `inquiry_id` is missing/invalid, or no images provided
/// - `500` on S3 upload, vision service, or DB failures
async fn depth_sensor_estimate(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let mut inquiry_id: Option<Uuid> = None;
    let mut images: Vec<(Vec<u8>, String)> = Vec::new();

    // Parse multipart form: extract inquiry_id and image files
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Invalid multipart data: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "inquiry_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read inquiry_id: {e}")))?;
                inquiry_id = Some(
                    text.parse::<Uuid>()
                        .map_err(|e| ApiError::Validation(format!("Invalid inquiry_id: {e}")))?,
                );
            }
            _ => {
                // Treat any other field as an image file
                let content_type = field
                    .content_type()
                    .unwrap_or("image/jpeg")
                    .to_string();
                if !content_type.starts_with("image/") {
                    continue;
                }
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read image: {e}")))?;
                images.push((data.to_vec(), content_type));
            }
        }
    }

    let inquiry_id =
        inquiry_id.ok_or_else(|| ApiError::Validation("inquiry_id field is required".into()))?;
    if images.is_empty() {
        return Err(ApiError::Validation(
            "At least one image file is required".into(),
        ));
    }

    let id = Uuid::now_v7();

    // Upload images to S3
    let s3_keys = services::vision::upload_images_to_s3(&*state.storage, inquiry_id, id, &images).await?;

    // Try the vision service first, fall back to LLM analysis
    let (total_volume, confidence, result_data, method) =
        match services::vision::try_vision_service(&state, &images, id, inquiry_id, id).await {
            Ok((vol, conf, data)) => (vol, conf, data, EstimationMethod::DepthSensor),
            Err(e) => {
                tracing::warn!("Vision service failed, falling back to LLM analysis: {e}");
                services::vision::fallback_llm_analysis(&state, &images).await?
            }
        };

    let now = chrono::Utc::now();
    let source_data = serde_json::json!({
        "image_count": images.len(),
        "s3_keys": s3_keys,
    });

    let est = insert_estimation(
        &state.db,
        id,
        inquiry_id,
        method.as_str(),
        &source_data,
        result_data.as_ref(),
        total_volume,
        confidence,
        now,
    )
    .await?;

    update_quote_volume(&state.db, inquiry_id, total_volume, "volume_estimated", now).await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, inquiry_id).await });

    Ok(Json(VolumeEstimation::from(est)))
}

/// `POST /api/v1/estimates/video` — Upload video(s) and start the 3D reconstruction pipeline.
///
/// **Caller**: Axum router / admin dashboard video upload (planned) and direct API consumers.
/// **Why**: Accepts one or more video files via multipart upload. Each video is stored in S3
/// and a `volume_estimations` row is inserted with status `processing`. Long-running Modal
/// calls (MASt3R + SAM 2 tracking + RE lookup) run in a Tokio background task to avoid
/// blocking the HTTP response. Returns immediately with the `processing` estimation records.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, storage, vision service client — must be configured)
/// - `multipart` — form with `inquiry_id`, `video` file(s), and optional `max_keyframes` /
///   `detection_threshold` tuning parameters
///
/// # Returns
/// `200 OK` with a `Vec<VolumeEstimation>` (one per video, all in status "processing").
/// The actual results are written by `process_video_background` when Modal finishes.
///
/// # Errors
/// - `400` if multipart parsing fails, `inquiry_id` missing, or no videos provided
/// - `500` if vision service is not configured or S3/DB fails
async fn video_estimate(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Vec<VolumeEstimation>>, ApiError> {
    let mut inquiry_id: Option<Uuid> = None;
    let mut videos: Vec<(Vec<u8>, String)> = Vec::new();
    let mut max_keyframes: Option<u32> = None;
    let mut detection_threshold: Option<f64> = None;

    // Parse multipart form: inquiry_id + video file(s) + optional params
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Invalid multipart data: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "inquiry_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read inquiry_id: {e}")))?;
                inquiry_id = Some(
                    text.parse::<Uuid>()
                        .map_err(|e| ApiError::Validation(format!("Invalid inquiry_id: {e}")))?,
                );
            }
            "max_keyframes" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read max_keyframes: {e}")))?;
                max_keyframes = text.parse().ok();
            }
            "detection_threshold" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read detection_threshold: {e}")))?;
                detection_threshold = text.parse().ok();
            }
            "video" => {
                // Infer content type from content_type header or file extension
                let content_type = field
                    .content_type()
                    .map(|ct| ct.to_string())
                    .unwrap_or_default();
                let file_name = field.file_name().unwrap_or("").to_lowercase();
                let mime = if content_type.starts_with("video/") {
                    content_type
                } else if file_name.ends_with(".mov") {
                    "video/quicktime".to_string()
                } else if file_name.ends_with(".webm") {
                    "video/webm".to_string()
                } else if file_name.ends_with(".mkv") {
                    "video/x-matroska".to_string()
                } else {
                    "video/mp4".to_string()
                };
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read video: {e}")))?;
                videos.push((data.to_vec(), mime));
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    let inquiry_id =
        inquiry_id.ok_or_else(|| ApiError::Validation("inquiry_id field is required".into()))?;
    if videos.is_empty() {
        return Err(ApiError::Validation("At least one video file is required".into()));
    }

    // Check vision service is configured before uploading
    if state.vision_service.is_none() {
        return Err(ApiError::Internal("Vision service not configured".into()));
    }

    let now = chrono::Utc::now();
    let mut estimations = Vec::with_capacity(videos.len());

    for (video_bytes, video_mime) in videos {
        let id = Uuid::now_v7();

        // Upload video to S3
        let ext = match video_mime.as_str() {
            "video/quicktime" => "mov",
            "video/webm" => "webm",
            "video/x-matroska" => "mkv",
            _ => "mp4",
        };
        let video_s3_key = format!("estimates/{inquiry_id}/{id}/video.{ext}");
        state
            .storage
            .upload(&video_s3_key, Bytes::from(video_bytes.clone()), &video_mime)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to upload video to storage: {e}")))?;

        // Insert estimation as "processing" — this path uses a custom status so we keep the inline query
        let source_data = serde_json::json!({
            "video_s3_key": video_s3_key,
            "video_mime": video_mime,
        });

        let row: VolumeEstimationRow = sqlx::query_as(
            r#"
            INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, total_volume_m3, confidence_score, created_at)
            VALUES ($1, $2, $3, 'processing', $4, NULL, NULL, $5)
            RETURNING id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
            "#
        )
        .bind(id)
        .bind(inquiry_id)
        .bind(EstimationMethod::Video.as_str())
        .bind(&source_data)
        .bind(now)
        .fetch_one(&state.db)
        .await?;

        tracing::info!(
            %inquiry_id,
            %id,
            video_size_mb = video_bytes.len() / (1024 * 1024),
            "Video uploaded, starting background processing..."
        );

        // Spawn background task for the long-running Modal call
        let state_bg = state.clone();
        tokio::spawn(async move {
            process_video_background(state_bg, id, inquiry_id, video_bytes, video_mime, max_keyframes, detection_threshold).await;
        });

        estimations.push(VolumeEstimation::from(row));
    }

    // Update quote status to show processing is underway (once for all videos)
    sqlx::query("UPDATE inquiries SET status = 'processing', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    Ok(Json(estimations))
}

/// Background task: call the Modal video pipeline, persist results, and trigger offer generation.
///
/// **Caller**: Spawned by `video_estimate` via `tokio::spawn` for each uploaded video.
/// **Why**: The MASt3R 3D reconstruction + SAM 2 tracking pipeline can take 60–600 seconds
/// on Modal. This function runs asynchronously after the HTTP response has been returned so
/// the client is not blocked. On completion it:
/// 1. Uploads crop thumbnails (base64 → S3)
/// 2. Updates the `volume_estimations` row to `completed` with `result_data`
/// 3. Waits until all videos for the quote finish processing
/// 4. Sums volumes across completed estimations and updates the quote
/// 5. Calls `try_auto_generate_offer` to produce the offer and Telegram notification
///
/// # Parameters
/// - `state` — shared AppState (DB pool, storage, vision service client)
/// - `estimation_id` — the `volume_estimations` row to update
/// - `inquiry_id` — the parent quote
/// - `video_bytes` — raw video data sent to Modal
/// - `video_mime` — MIME type string (e.g. "video/mp4")
/// - `max_keyframes` — optional cap on extracted keyframes passed to Modal
/// - `detection_threshold` — optional confidence threshold passed to Modal
///
/// # Errors
/// Errors are logged and the estimation status is set to `failed`; no panic propagation.
async fn process_video_background(
    state: Arc<AppState>,
    estimation_id: Uuid,
    inquiry_id: Uuid,
    video_bytes: Vec<u8>,
    video_mime: String,
    max_keyframes: Option<u32>,
    detection_threshold: Option<f64>,
) {
    let client = match state.vision_service.as_ref() {
        Some(c) => c,
        None => {
            tracing::error!(%estimation_id, "Vision service not configured in background task");
            let _ = sqlx::query("UPDATE volume_estimations SET status = 'failed' WHERE id = $1")
                .bind(estimation_id)
                .execute(&state.db)
                .await;
            return;
        }
    };

    tracing::info!(
        %inquiry_id,
        %estimation_id,
        video_size_mb = video_bytes.len() / (1024 * 1024),
        "Background: calling vision service video endpoint..."
    );

    let response = match client
        .estimate_video(
            &estimation_id.to_string(),
            &video_bytes,
            &video_mime,
            max_keyframes,
            detection_threshold,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(%inquiry_id, %estimation_id, error = %e, "Background: vision service video call failed");
            let _ = sqlx::query("UPDATE volume_estimations SET status = 'failed' WHERE id = $1")
                .bind(estimation_id)
                .execute(&state.db)
                .await;
            return;
        }
    };

    tracing::info!(
        %inquiry_id,
        %estimation_id,
        items = response.detected_items.len(),
        total_volume = response.total_volume_m3,
        processing_ms = response.processing_time_ms,
        "Background: vision service video response received"
    );

    // Upload crop thumbnails to S3 and replace base64 with S3 keys
    let mut items_value = match serde_json::to_value(&response.detected_items) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(%estimation_id, error = %e, "Background: failed to serialize items");
            let _ = sqlx::query("UPDATE volume_estimations SET status = 'failed' WHERE id = $1")
                .bind(estimation_id)
                .execute(&state.db)
                .await;
            return;
        }
    };

    if let Some(items_arr) = items_value.as_array_mut() {
        for (idx, item_val) in items_arr.iter_mut().enumerate() {
            if let Some(crop_b64) = item_val.get("crop_base64").and_then(|v| v.as_str()) {
                if !crop_b64.is_empty() {
                    let name = item_val.get("name").and_then(|v| v.as_str()).unwrap_or("item");
                    let safe_name = name.replace(' ', "_").to_lowercase();
                    let key = format!("estimates/{inquiry_id}/{estimation_id}/crops/{safe_name}_{idx}.jpg");
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(crop_b64)
                    {
                        if state
                            .storage
                            .upload(&key, Bytes::from(decoded), "image/jpeg")
                            .await
                            .is_ok()
                        {
                            item_val.as_object_mut().map(|obj| {
                                obj.remove("crop_base64");
                                obj.insert(
                                    "crop_s3_key".to_string(),
                                    serde_json::Value::String(key),
                                );
                            });
                        }
                    }
                }
            }
        }
    }

    let now = chrono::Utc::now();

    // Update estimation with results
    let _ = sqlx::query(
        r#"
        UPDATE volume_estimations
        SET status = 'completed', result_data = $1, total_volume_m3 = $2, confidence_score = $3
        WHERE id = $4
        "#,
    )
    .bind(Some(&items_value))
    .bind(response.total_volume_m3)
    .bind(response.confidence_score)
    .bind(estimation_id)
    .execute(&state.db)
    .await;

    // Check if other videos for this quote are still processing
    let still_processing: (i64,) = match sqlx::query_as(
        "SELECT COUNT(*) FROM volume_estimations WHERE inquiry_id = $1 AND status = 'processing'",
    )
    .bind(inquiry_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(%inquiry_id, error = %e, "Background: failed to check processing count");
            (0,)
        }
    };

    if still_processing.0 > 0 {
        tracing::info!(
            %inquiry_id,
            %estimation_id,
            still_processing = still_processing.0,
            "Background: other videos still processing, skipping offer generation"
        );
        return;
    }

    // All videos done — sum volumes from all completed estimations
    let total_volume: (Option<f64>,) = sqlx::query_as(
        "SELECT SUM(total_volume_m3) FROM volume_estimations WHERE inquiry_id = $1 AND status = 'completed'",
    )
    .bind(inquiry_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or((None,));

    let combined_volume = total_volume.0.unwrap_or(0.0);

    // Update quote with combined estimated volume
    let _ = update_quote_volume(&state.db, inquiry_id, combined_volume, "volume_estimated", now).await;

    tracing::info!(%inquiry_id, %estimation_id, combined_volume, "Background: all video estimations completed, triggering offer generation");

    // Auto-generate offer
    orchestrator::try_auto_generate_offer(state, inquiry_id).await;
}


#[derive(Debug, Deserialize)]
struct InventoryRequest {
    inquiry_id: Uuid,
    inventory: InventoryForm,
}

/// `POST /api/v1/estimates/inventory` — Calculate volume from a structured inventory form.
///
/// **Caller**: Axum router / "Kostenloses Angebot" form flow and admin manual entry.
/// **Why**: When the customer fills in the structured moving goods form (VolumeCalculator),
/// volumes are computed deterministically from item counts — no ML or vision required.
/// Confidence is fixed at 1.0 because the user explicitly listed their items.
/// After storing results, auto-generates an offer in the background.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `request` — JSON body with `inquiry_id` and `inventory` form data
///
/// # Returns
/// `200 OK` with the newly created `VolumeEstimation` JSON (status = "completed").
///
/// # Errors
/// - `500` on `InventoryProcessor` failures or DB errors
async fn inventory_estimate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<InventoryRequest>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let processor = aust_volume_estimator::InventoryProcessor::new();
    let total_volume = processor
        .process_form(&request.inventory)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let id = Uuid::now_v7();
    let now = chrono::Utc::now();

    let source_data = serde_json::to_value(&request.inventory).unwrap_or_default();
    let est = insert_estimation(
        &state.db,
        id,
        request.inquiry_id,
        EstimationMethod::Inventory.as_str(),
        &source_data,
        None,
        total_volume,
        1.0,
        now,
    )
    .await?;

    update_quote_volume(&state.db, request.inquiry_id, total_volume, "volume_estimated", now).await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    let qid = request.inquiry_id;
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, qid).await });

    Ok(Json(VolumeEstimation::from(est)))
}

/// `GET /api/v1/estimates/{id}` — Retrieve a single volume estimation by ID.
///
/// **Caller**: Axum router / polling clients waiting for `processing` → `completed` transitions.
/// **Why**: Returns the raw `VolumeEstimation` record including `status`, so the frontend
/// can poll until a video processing job finishes.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — estimation UUID path parameter
///
/// # Returns
/// `200 OK` with `VolumeEstimation` JSON.
///
/// # Errors
/// - `404` if no estimation with the given ID exists
async fn get_estimate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let row: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        FROM volume_estimations WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Estimation {id} not found")))?;
    Ok(Json(VolumeEstimation::from(row)))
}

/// Collect all S3 object keys associated with an estimation, for pre-deletion cleanup.
///
/// **Caller**: `delete_estimate` (before the DB row is removed).
/// **Why**: Estimations reference several S3 objects that must be deleted alongside the DB
/// row to avoid orphaned storage objects: the original video or image files, and per-item
/// crop thumbnails generated by the vision pipeline.
///
/// # Parameters
/// - `source_data` — `volume_estimations.source_data` JSON; contains `video_s3_key` for
///   video estimations or `s3_keys` array for image estimations
/// - `result_data` — optional `volume_estimations.result_data` JSON; items may carry
///   `crop_s3_key` values for crop thumbnail objects
///
/// # Returns
/// Flat `Vec<String>` of S3 keys. May be empty if no objects are referenced.
pub fn collect_estimation_s3_keys(
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
) -> Vec<String> {
    let mut keys = Vec::new();

    // Video key (video estimations)
    if let Some(k) = source_data.get("video_s3_key").and_then(|v| v.as_str()) {
        keys.push(k.to_string());
    }

    // Image keys (vision / depth-sensor estimations)
    if let Some(arr) = source_data.get("s3_keys").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(k) = v.as_str() {
                keys.push(k.to_string());
            }
        }
    }

    // Crop thumbnail keys stored on each detected item in result_data
    if let Some(items) = result_data.and_then(|v| v.as_array()) {
        for item in items {
            if let Some(k) = item.get("crop_s3_key").and_then(|v| v.as_str()) {
                keys.push(k.to_string());
            }
        }
    }

    keys
}

/// `DELETE /api/v1/estimates/{id}` — Delete an estimation and its associated S3 objects.
///
/// **Caller**: Axum router / admin dashboard item editor (when removing a bad estimation batch).
/// **Why**: Deleting an estimation must also remove its S3 files (video, images, crop
/// thumbnails) to avoid orphaned objects. After deletion, the quote's volume is
/// recalculated from the remaining completed estimations, and its status is reset to
/// "new" if no estimations remain.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, storage)
/// - `id` — estimation UUID path parameter
///
/// # Returns
/// `204 No Content` on success.
///
/// # Errors
/// - `404` if no estimation with the given ID exists
/// - `500` on DB failures (S3 deletion errors are logged as warnings and do not fail the request)
async fn delete_estimate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    // Fetch the estimation to collect S3 keys and inquiry_id
    let row: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        FROM volume_estimations WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Estimation {id} not found")))?;
    let inquiry_id = row.inquiry_id;

    // Collect S3 keys and delete objects (best-effort — don't fail the request on storage errors)
    let s3_keys = collect_estimation_s3_keys(&row.source_data, row.result_data.as_ref());
    for key in s3_keys {
        if let Err(e) = state.storage.delete(&key).await {
            tracing::warn!(estimation_id = %id, key = %key, error = %e, "Failed to delete estimation S3 object");
        }
    }

    // Delete estimation from DB
    sqlx::query("DELETE FROM volume_estimations WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    // Recalculate quote volume from remaining completed estimations and update quote
    let total: (Option<f64>,) = sqlx::query_as(
        "SELECT SUM(total_volume_m3) FROM volume_estimations WHERE inquiry_id = $1 AND status = 'completed'",
    )
    .bind(inquiry_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or((None,));

    let combined_volume = total.0.unwrap_or(0.0);
    let now = chrono::Utc::now();

    // Only update volume/status if there are still estimations; otherwise reset to new
    let new_status = if combined_volume > 0.0 { "volume_estimated" } else { "new" };
    sqlx::query("UPDATE inquiries SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4")
        .bind(combined_volume)
        .bind(new_status)
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/estimates/images/{*key}` — Proxy an S3 object (image or video) to the browser.
///
/// **Caller**: Axum public router / `<img>` and `<video>` tags in the admin dashboard.
/// **Why**: S3/MinIO objects are not directly accessible from the browser in the dev
/// environment. This proxy route downloads the object from storage and streams it back
/// with the correct `Content-Type`, inferred from the file extension.
///
/// # Parameters
/// - `state` — shared AppState (storage)
/// - `key` — full S3 key (wildcard path segment), e.g. `estimates/uuid/uuid/0.jpg`
///
/// # Returns
/// File bytes with appropriate `Content-Type` header.
///
/// # Errors
/// - `500` if the S3 download fails (key not found returns the storage provider's error)
async fn serve_image(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let file_bytes = state
        .storage
        .download(&key)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to download image: {e}")))?;

    let content_type = if key.ends_with(".png") {
        "image/png"
    } else if key.ends_with(".webp") {
        "image/webp"
    } else if key.ends_with(".mp4") {
        "video/mp4"
    } else if key.ends_with(".mov") {
        "video/quicktime"
    } else if key.ends_with(".webm") {
        "video/webm"
    } else if key.ends_with(".mkv") {
        "video/x-matroska"
    } else {
        "image/jpeg"
    };

    Ok((
        [(axum::http::header::CONTENT_TYPE, content_type.to_string())],
        file_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_keys_empty_source() {
        let source = serde_json::json!({});
        let keys = collect_estimation_s3_keys(&source, None);
        assert!(keys.is_empty());
    }

    #[test]
    fn s3_keys_video_key() {
        let source = serde_json::json!({
            "video_s3_key": "estimates/abc/def/video.mp4"
        });
        let keys = collect_estimation_s3_keys(&source, None);
        assert_eq!(keys, vec!["estimates/abc/def/video.mp4"]);
    }

    #[test]
    fn s3_keys_image_keys_array() {
        let source = serde_json::json!({
            "s3_keys": ["estimates/abc/def/0.jpg", "estimates/abc/def/1.jpg"]
        });
        let keys = collect_estimation_s3_keys(&source, None);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"estimates/abc/def/0.jpg".to_string()));
        assert!(keys.contains(&"estimates/abc/def/1.jpg".to_string()));
    }

    #[test]
    fn s3_keys_crop_keys_from_result_data() {
        let source = serde_json::json!({});
        let result = serde_json::json!([
            {"name": "Sofa", "crop_s3_key": "estimates/abc/def/crops/sofa_0.jpg"},
            {"name": "Tisch", "crop_s3_key": "estimates/abc/def/crops/tisch_1.jpg"},
            {"name": "Stuhl"}  // no crop_s3_key
        ]);
        let keys = collect_estimation_s3_keys(&source, Some(&result));
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"estimates/abc/def/crops/sofa_0.jpg".to_string()));
        assert!(keys.contains(&"estimates/abc/def/crops/tisch_1.jpg".to_string()));
    }

    #[test]
    fn s3_keys_combined_video_and_crops() {
        let source = serde_json::json!({
            "video_s3_key": "estimates/q/e/video.mp4"
        });
        let result = serde_json::json!([
            {"crop_s3_key": "estimates/q/e/crops/item_0.jpg"}
        ]);
        let keys = collect_estimation_s3_keys(&source, Some(&result));
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"estimates/q/e/video.mp4".to_string()));
        assert!(keys.contains(&"estimates/q/e/crops/item_0.jpg".to_string()));
    }

    #[test]
    fn s3_keys_no_duplicates_for_items_without_crops() {
        let source = serde_json::json!({
            "s3_keys": ["estimates/abc/0.jpg"]
        });
        let result = serde_json::json!([
            {"name": "Tisch"}  // no crop
        ]);
        let keys = collect_estimation_s3_keys(&source, Some(&result));
        assert_eq!(keys, vec!["estimates/abc/0.jpg"]);
    }
}
