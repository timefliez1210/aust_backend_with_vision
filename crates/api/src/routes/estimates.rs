use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde::Deserialize;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, AppState};
use aust_core::models::{EstimationMethod, InventoryForm, VolumeEstimation};
use aust_volume_estimator::VisionAnalyzer;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/vision", post(vision_estimate))
        .route("/inventory", post(inventory_estimate))
        .route("/{id}", get(get_estimate))
}

#[derive(Debug, FromRow)]
struct VolumeEstimationRow {
    id: Uuid,
    quote_id: Uuid,
    method: String,
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
            "manual" => EstimationMethod::Manual,
            _ => EstimationMethod::Manual,
        };

        VolumeEstimation {
            id: row.id,
            quote_id: row.quote_id,
            method,
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

async fn vision_estimate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<VisionEstimateRequest>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    if request.images.is_empty() {
        return Err(ApiError::Validation("At least one image is required".into()));
    }

    let analyzer = VisionAnalyzer::new(state.llm.clone());
    let mut total_volume = 0.0;
    let mut results = Vec::new();

    for image in &request.images {
        let data = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .map_err(|e| ApiError::Validation(format!("Invalid base64 image data: {e}")))?;

        let result = analyzer
            .analyze_image(&data, &image.mime_type)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        total_volume += result.total_volume_m3;
        results.push(result);
    }

    let id = Uuid::now_v7();
    let now = chrono::Utc::now();
    let avg_confidence = results.iter().map(|r| r.confidence_score).sum::<f64>() / results.len() as f64;

    let row: VolumeEstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#
    )
    .bind(id)
    .bind(request.quote_id)
    .bind(EstimationMethod::Vision.as_str())
    .bind(serde_json::json!({"image_count": request.images.len()}))
    .bind(serde_json::to_value(&results).ok())
    .bind(total_volume)
    .bind(avg_confidence)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Update quote with estimated volume
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4"
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(request.quote_id)
    .execute(&state.db)
    .await?;

    Ok(Json(VolumeEstimation::from(row)))
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
        RETURNING id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at
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
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4"
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(now)
    .bind(request.quote_id)
    .execute(&state.db)
    .await?;

    Ok(Json(VolumeEstimation::from(row)))
}

async fn get_estimate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<VolumeEstimation>, ApiError> {
    let row: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at
        FROM volume_estimations WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Estimation {id} not found")))?;
    Ok(Json(VolumeEstimation::from(row)))
}
