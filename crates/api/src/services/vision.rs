//! Shared vision estimation helpers used by both estimates and inquiries routes.

use base64::Engine;
use bytes::Bytes;
use uuid::Uuid;

use crate::{ApiError, AppState};
use aust_core::models::EstimationMethod;
use aust_storage::StorageProvider;
use aust_volume_estimator::VisionAnalyzer;

/// Upload decoded images to S3, returning the list of S3 keys.
pub async fn upload_images_to_s3(
    storage: &dyn StorageProvider,
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

/// Try the Python vision service for 3D volume estimation.
/// Sends raw image bytes directly via multipart upload.
/// Uploads crop thumbnails to S3 and replaces base64 with S3 keys.
pub async fn try_vision_service(
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
                    let key = format!(
                        "estimates/{quote_id}/{estimation_id}/crops/{safe_name}_{idx}.jpg"
                    );
                    if let Ok(decoded) =
                        base64::engine::general_purpose::STANDARD.decode(crop_b64)
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

    Ok((response.total_volume_m3, response.confidence_score, Some(items_value)))
}

/// Fallback: run LLM-based vision analysis on the raw image data.
/// Returns (total_volume, confidence, result_data, method).
pub async fn fallback_llm_analysis(
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

/// Parse a free-form address string into (street, city, postal_code).
/// Best-effort: tries to split "Straße 1, 31157 Sarstedt" into parts.
pub fn parse_address(addr: &str) -> (String, String, String) {
    let parts: Vec<&str> = addr.splitn(2, ',').collect();
    if parts.len() == 2 {
        let street = parts[0].trim().to_string();
        let city_part = parts[1].trim();
        let mut postal = String::new();
        let mut city = city_part.to_string();
        for word in city_part.split_whitespace() {
            if word.len() >= 4 && word.len() <= 5 && word.chars().all(|c| c.is_ascii_digit()) {
                postal = word.to_string();
                city = city_part.replace(word, "").trim().to_string();
                break;
            }
        }
        (street, city, postal)
    } else {
        (addr.to_string(), String::new(), String::new())
    }
}
