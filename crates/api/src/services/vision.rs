//! Shared vision estimation helpers used by both estimates and inquiries routes.

use base64::Engine;
use bytes::Bytes;
use std::time::Duration;
use uuid::Uuid;

use crate::{ApiError, AppState};
use aust_storage::StorageProvider;

/// Upload decoded images to S3, returning the list of S3 keys.
pub async fn upload_images_to_s3(
    storage: &dyn StorageProvider,
    inquiry_id: Uuid,
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
        let key = format!("estimates/{inquiry_id}/{estimation_id}/{idx}.{ext}");
        storage
            .upload(&key, Bytes::from(data.clone()), mime_type)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to upload image to storage: {e}")))?;
        s3_keys.push(key);
    }
    Ok(s3_keys)
}

/// Try the Python vision service using async submit + polling.
///
/// **Caller**: `process_submission_background` and `process_video_background` in
///             `crates/api/src/routes/inquiries.rs`.
/// **Why**: Replaces the old synchronous long-poll approach. The backend submits the job
///          to Modal, receives an immediate acknowledgement, then polls every
///          `vision_service.poll_interval_secs` until the job finishes.
///          This avoids holding an HTTP connection open for the 5-600 s pipeline duration.
///
/// # Parameters
/// - `state` — application state (provides the vision client and config)
/// - `images` — raw image bytes paired with MIME types
/// - `job_id` — UUID of the vision job (used as the Modal job identifier)
/// - `inquiry_id` — parent inquiry (used for S3 crop key paths)
/// - `estimation_id` — the pre-created estimation row (used for S3 crop key paths)
///
/// # Returns
/// `(total_volume_m3, confidence_score, detected_items_json)` on success.
///
/// # Errors
/// Returns `ApiError::Internal` if the vision service is not configured or if all
/// retries are exhausted without a successful result.
pub async fn try_vision_service_async(
    state: &AppState,
    images: &[(Vec<u8>, String)],
    job_id: Uuid,
    inquiry_id: Uuid,
    estimation_id: Uuid,
) -> Result<(f64, f64, Option<serde_json::Value>), ApiError> {
    let client = state
        .vision_service
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Vision service not configured".into()))?;

    let poll_interval =
        Duration::from_secs(state.config.vision_service.poll_interval_secs);
    let max_polls = state.config.vision_service.max_polls;
    let max_retries = state.config.vision_service.max_retries;

    let response = client
        .estimate_upload_async(
            &job_id.to_string(),
            images,
            poll_interval,
            max_polls,
            max_retries,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Upload crop thumbnails to S3 and replace base64 with S3 keys
    let mut items_value = serde_json::to_value(&response.detected_items)
        .map_err(|e| ApiError::Internal(format!("Failed to serialize items: {e}")))?;

    if let Some(items_arr) = items_value.as_array_mut() {
        for (idx, item_val) in items_arr.iter_mut().enumerate() {
            if let Some(crop_b64) = item_val.get("crop_base64").and_then(|v| v.as_str()) {
                if !crop_b64.is_empty() {
                    let name = item_val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("item");
                    let safe_name = name.replace(' ', "_").to_lowercase();
                    let key = format!(
                        "estimates/{inquiry_id}/{estimation_id}/crops/{safe_name}_{idx}.jpg"
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

    Ok((
        response.total_volume_m3,
        response.confidence_score,
        Some(items_value),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_address() {
        let (street, city, postal) = parse_address("Musterstr. 1, 31157 Sarstedt");
        assert_eq!(street, "Musterstr. 1");
        assert_eq!(city, "Sarstedt");
        assert_eq!(postal, "31157");
    }

    #[test]
    fn parse_no_postal_code() {
        let (street, city, postal) = parse_address("Musterstr. 1, Sarstedt");
        assert_eq!(street, "Musterstr. 1");
        assert_eq!(city, "Sarstedt");
        assert_eq!(postal, "");
    }

    #[test]
    fn parse_no_comma() {
        let (street, city, postal) = parse_address("Musterstr 1 Sarstedt");
        assert_eq!(street, "Musterstr 1 Sarstedt");
        assert_eq!(city, "");
        assert_eq!(postal, "");
    }

    #[test]
    fn parse_five_digit_postal() {
        let (street, city, postal) = parse_address("Straße 1, 10115 Berlin");
        assert_eq!(street, "Straße 1");
        assert_eq!(postal, "10115");
        assert_eq!(city, "Berlin");
    }

    #[test]
    fn parse_four_digit_postal() {
        let (street, city, postal) = parse_address("Straße 1, 1010 Wien");
        assert_eq!(street, "Straße 1");
        assert_eq!(postal, "1010");
        assert_eq!(city, "Wien");
    }

    use proptest::prelude::*;
    proptest! {
        #[test]
        fn parse_address_never_panics(s in ".*") {
            let _ = parse_address(&s);
        }
    }
}
