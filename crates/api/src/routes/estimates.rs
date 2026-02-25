use axum::{
    extract::{Multipart, Path, State},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bytes::Bytes;
use serde::Deserialize;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::{orchestrator, ApiError, AppState};
use aust_core::models::{EstimationMethod, InventoryForm, VolumeEstimation};
use aust_volume_estimator::VisionAnalyzer;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/vision", post(vision_estimate))
        .route("/inventory", post(inventory_estimate))
        .route("/depth-sensor", post(depth_sensor_estimate))
        .route("/video", post(video_estimate))
        .route("/{id}", get(get_estimate))
        .route("/images/{*key}", get(serve_image))
}

#[derive(Debug, FromRow)]
struct VolumeEstimationRow {
    id: Uuid,
    quote_id: Uuid,
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
            quote_id: row.quote_id,
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
    quote_id: Uuid,
    images: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    data: String,      // base64 encoded
    mime_type: String, // e.g., "image/jpeg"
}

/// Upload decoded images to S3, returning the list of S3 keys.
async fn upload_images_to_s3(
    storage: &dyn aust_storage::StorageProvider,
    quote_id: Uuid,
    estimation_id: Uuid,
    images: &[(Vec<u8>, String)], // (data, mime_type)
) -> Result<Vec<String>, ApiError> {
    let mut s3_keys = Vec::with_capacity(images.len());
    for (idx, (data, mime_type)) in images.iter().enumerate() {
        let ext = match mime_type.as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        let key = format!("estimates/{quote_id}/{estimation_id}/{idx}.{ext}");
        storage
            .upload(&key, Bytes::from(data.clone()), mime_type)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to upload image to storage: {e}")))?;
        s3_keys.push(key);
    }
    Ok(s3_keys)
}

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
    let s3_keys = upload_images_to_s3(&*state.storage, request.quote_id, id, &decoded).await;
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

    let row: VolumeEstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#
    )
    .bind(id)
    .bind(request.quote_id)
    .bind(EstimationMethod::Vision.as_str())
    .bind(source_data)
    .bind(serde_json::to_value(&results).ok())
    .bind(total_volume)
    .bind(avg_confidence)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Update quote with estimated volume
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(request.quote_id)
    .execute(&state.db)
    .await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    let qid = request.quote_id;
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, qid).await });

    Ok(Json(VolumeEstimation::from(row)))
}

async fn depth_sensor_estimate(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let mut quote_id: Option<Uuid> = None;
    let mut images: Vec<(Vec<u8>, String)> = Vec::new();

    // Parse multipart form: extract quote_id and image files
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Invalid multipart data: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "quote_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read quote_id: {e}")))?;
                quote_id = Some(
                    text.parse::<Uuid>()
                        .map_err(|e| ApiError::Validation(format!("Invalid quote_id: {e}")))?,
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

    let quote_id =
        quote_id.ok_or_else(|| ApiError::Validation("quote_id field is required".into()))?;
    if images.is_empty() {
        return Err(ApiError::Validation(
            "At least one image file is required".into(),
        ));
    }

    let id = Uuid::now_v7();

    // Upload images to S3
    let s3_keys = upload_images_to_s3(&*state.storage, quote_id, id, &images).await?;

    // Try the vision service first, fall back to LLM analysis
    let (total_volume, confidence, result_data, method) =
        match try_vision_service(&state, &images, id, quote_id, id).await {
            Ok((vol, conf, data)) => (vol, conf, data, EstimationMethod::DepthSensor),
            Err(e) => {
                tracing::warn!("Vision service failed, falling back to LLM analysis: {e}");
                fallback_llm_analysis(&state, &images).await?
            }
        };

    let now = chrono::Utc::now();
    let source_data = serde_json::json!({
        "image_count": images.len(),
        "s3_keys": s3_keys,
    });

    let row: VolumeEstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#
    )
    .bind(id)
    .bind(quote_id)
    .bind(method.as_str())
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume)
    .bind(confidence)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Update quote with estimated volume
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(quote_id)
    .execute(&state.db)
    .await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, quote_id).await });

    Ok(Json(VolumeEstimation::from(row)))
}

async fn video_estimate(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Vec<VolumeEstimation>>, ApiError> {
    let mut quote_id: Option<Uuid> = None;
    let mut videos: Vec<(Vec<u8>, String)> = Vec::new();
    let mut max_keyframes: Option<u32> = None;
    let mut detection_threshold: Option<f64> = None;

    // Parse multipart form: quote_id + video file(s) + optional params
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Invalid multipart data: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "quote_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Failed to read quote_id: {e}")))?;
                quote_id = Some(
                    text.parse::<Uuid>()
                        .map_err(|e| ApiError::Validation(format!("Invalid quote_id: {e}")))?,
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

    let quote_id =
        quote_id.ok_or_else(|| ApiError::Validation("quote_id field is required".into()))?;
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
        let video_s3_key = format!("estimates/{quote_id}/{id}/video.{ext}");
        state
            .storage
            .upload(&video_s3_key, Bytes::from(video_bytes.clone()), &video_mime)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to upload video to storage: {e}")))?;

        // Insert estimation as "processing"
        let source_data = serde_json::json!({
            "video_s3_key": video_s3_key,
            "video_mime": video_mime,
        });

        let row: VolumeEstimationRow = sqlx::query_as(
            r#"
            INSERT INTO volume_estimations (id, quote_id, method, status, source_data, total_volume_m3, confidence_score, created_at)
            VALUES ($1, $2, $3, 'processing', $4, NULL, NULL, $5)
            RETURNING id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
            "#
        )
        .bind(id)
        .bind(quote_id)
        .bind(EstimationMethod::Video.as_str())
        .bind(&source_data)
        .bind(now)
        .fetch_one(&state.db)
        .await?;

        tracing::info!(
            %quote_id,
            %id,
            video_size_mb = video_bytes.len() / (1024 * 1024),
            "Video uploaded, starting background processing..."
        );

        // Spawn background task for the long-running Modal call
        let state_bg = state.clone();
        tokio::spawn(async move {
            process_video_background(state_bg, id, quote_id, video_bytes, video_mime, max_keyframes, detection_threshold).await;
        });

        estimations.push(VolumeEstimation::from(row));
    }

    // Update quote status to show processing is underway (once for all videos)
    sqlx::query("UPDATE quotes SET status = 'processing', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(quote_id)
        .execute(&state.db)
        .await?;

    Ok(Json(estimations))
}

/// Background task: call Modal vision service, store results, trigger offer generation.
async fn process_video_background(
    state: Arc<AppState>,
    estimation_id: Uuid,
    quote_id: Uuid,
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
        %quote_id,
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
            tracing::error!(%quote_id, %estimation_id, error = %e, "Background: vision service video call failed");
            let _ = sqlx::query("UPDATE volume_estimations SET status = 'failed' WHERE id = $1")
                .bind(estimation_id)
                .execute(&state.db)
                .await;
            return;
        }
    };

    tracing::info!(
        %quote_id,
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
                    let key = format!("estimates/{quote_id}/{estimation_id}/crops/{safe_name}_{idx}.jpg");
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
        "SELECT COUNT(*) FROM volume_estimations WHERE quote_id = $1 AND status = 'processing'",
    )
    .bind(quote_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(%quote_id, error = %e, "Background: failed to check processing count");
            (0,)
        }
    };

    if still_processing.0 > 0 {
        tracing::info!(
            %quote_id,
            %estimation_id,
            still_processing = still_processing.0,
            "Background: other videos still processing, skipping offer generation"
        );
        return;
    }

    // All videos done — sum volumes from all completed estimations
    let total_volume: (Option<f64>,) = sqlx::query_as(
        "SELECT SUM(total_volume_m3) FROM volume_estimations WHERE quote_id = $1 AND status = 'completed'",
    )
    .bind(quote_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or((None,));

    let combined_volume = total_volume.0.unwrap_or(0.0);

    // Update quote with combined estimated volume
    let _ = sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(combined_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(quote_id)
    .execute(&state.db)
    .await;

    tracing::info!(%quote_id, %estimation_id, combined_volume, "Background: all video estimations completed, triggering offer generation");

    // Auto-generate offer
    orchestrator::try_auto_generate_offer(state, quote_id).await;
}

/// Try the Python vision service for 3D volume estimation.
/// Sends raw image bytes directly via multipart upload.
/// Uploads crop thumbnails to S3 and replaces base64 with S3 keys.
async fn try_vision_service(
    state: &AppState,
    images: &[(Vec<u8>, String)],
    job_id: Uuid,
    quote_id: Uuid,
    estimation_id: Uuid,
) -> Result<(f64, f64, Option<serde_json::Value>), ApiError> {
    let client = state
        .vision_service
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Vision service not configured".into()))?;

    let response = client
        .estimate_upload(&job_id.to_string(), images)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Upload crop thumbnails to S3 and replace base64 with S3 keys
    let mut items_value = serde_json::to_value(&response.detected_items)
        .map_err(|e| ApiError::Internal(format!("Failed to serialize items: {e}")))?;

    if let Some(items_arr) = items_value.as_array_mut() {
        for (idx, item_val) in items_arr.iter_mut().enumerate() {
            if let Some(crop_b64) = item_val.get("crop_base64").and_then(|v| v.as_str()) {
                if !crop_b64.is_empty() {
                    let name = item_val.get("name").and_then(|v| v.as_str()).unwrap_or("item");
                    let safe_name = name.replace(' ', "_").to_lowercase();
                    let key = format!("estimates/{quote_id}/{estimation_id}/crops/{safe_name}_{idx}.jpg");
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(crop_b64) {
                        if let Ok(_) = state.storage
                            .upload(&key, Bytes::from(decoded), "image/jpeg")
                            .await
                        {
                            item_val.as_object_mut().map(|obj| {
                                obj.remove("crop_base64");
                                obj.insert("crop_s3_key".to_string(), serde_json::Value::String(key));
                            });
                        }
                    }
                }
            }
        }
    }

    let result_data = Some(items_value);
    Ok((response.total_volume_m3, response.confidence_score, result_data))
}

/// Fallback: run LLM-based vision analysis on the raw image data.
/// Returns (total_volume, confidence, result_data, method).
async fn fallback_llm_analysis(
    state: &AppState,
    images: &[(Vec<u8>, String)],
) -> Result<(f64, f64, Option<serde_json::Value>, EstimationMethod), ApiError> {
    let analyzer = VisionAnalyzer::new(state.llm.clone());
    let mut total_volume = 0.0;
    let mut results = Vec::new();

    for (data, mime_type) in images {
        let result = analyzer
            .analyze_image(data, mime_type)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        total_volume += result.total_volume_m3;
        results.push(result);
    }

    let avg_confidence =
        results.iter().map(|r| r.confidence_score).sum::<f64>() / results.len() as f64;
    let result_data = serde_json::to_value(&results).ok();

    Ok((total_volume, avg_confidence, result_data, EstimationMethod::Vision))
}

#[derive(Debug, Deserialize)]
struct InventoryRequest {
    quote_id: Uuid,
    inventory: InventoryForm,
}

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

    let row: VolumeEstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#
    )
    .bind(id)
    .bind(request.quote_id)
    .bind(EstimationMethod::Inventory.as_str())
    .bind(serde_json::to_value(&request.inventory).unwrap_or_default())
    .bind(None::<serde_json::Value>)
    .bind(total_volume)
    .bind(1.0f64)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Update quote with estimated volume
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(request.quote_id)
    .execute(&state.db)
    .await?;

    // Auto-generate offer in background
    let state_clone = state.clone();
    let qid = request.quote_id;
    tokio::spawn(async move { orchestrator::try_auto_generate_offer(state_clone, qid).await });

    Ok(Json(VolumeEstimation::from(row)))
}

async fn get_estimate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let row: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, quote_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        FROM volume_estimations WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Estimation {id} not found")))?;
    Ok(Json(VolumeEstimation::from(row)))
}

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
