//! Inquiry endpoints for photo webapp (Source C) and mobile app (Source D).
//!
//! Both endpoints accept multipart form data with customer info, addresses,
//! service preferences, and image uploads. The mobile endpoint additionally
//! accepts depth maps and AR metadata.
//!
//! Processing flow:
//! 1. Create/update customer by email
//! 2. Create origin + destination addresses
//! 3. Create quote
//! 4. Upload images to S3
//! 5. Run volume estimation (vision service → LLM fallback)
//! 6. Update quote with estimated volume
//! 7. Auto-generate offer → Telegram approval

use axum::{extract::Multipart, extract::State, http::StatusCode, routing::post, Json, Router};
use base64::Engine;
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::{orchestrator, ApiError, AppState};
use aust_core::models::EstimationMethod;
use aust_storage::StorageProvider;
use aust_volume_estimator::VisionAnalyzer;

/// Response returned from both /photo and /mobile endpoints.
/// Returned immediately as 202 Accepted — processing continues in background.
#[derive(Serialize)]
struct InquiryResponse {
    quote_id: Uuid,
    customer_id: Uuid,
    status: String,
    message: String,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/photo", post(photo_inquiry))
        .route("/mobile", post(mobile_inquiry))
}

/// POST /photo — Photo webapp inquiry (Source C).
/// Accepts multipart form with customer info, addresses, services, and images.
/// Returns 202 Accepted immediately; processing continues in background.
async fn photo_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<InquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, false).await?;
    handle_inquiry(state, parsed).await
}

/// POST /mobile — Mobile app inquiry (Source D).
/// Same as /photo, plus depth_maps and ar_metadata fields.
/// Returns 202 Accepted immediately; processing continues in background.
async fn mobile_inquiry(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<InquiryResponse>), ApiError> {
    let parsed = parse_inquiry_form(multipart, true).await?;
    handle_inquiry(state, parsed).await
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
    images: Vec<(Vec<u8>, String)>, // (data, mime_type)
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
                    .map_err(|e| ApiError::BadRequest(format!("Bild konnte nicht gelesen werden: {e}")))?;
                form.images.push((data.to_vec(), content_type));
            }
            "depth_maps" if accept_depth => {
                let content_type = field.content_type().unwrap_or("image/png").to_string();
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("Tiefenkarte konnte nicht gelesen werden: {e}")))?;
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

/// Shared handler for both photo and mobile inquiries.
/// Creates customer + addresses + quote synchronously, then spawns background
/// processing for S3 upload, vision estimation, and offer generation.
/// Returns 202 Accepted immediately so the Cloudflare tunnel / client doesn't time out.
async fn handle_inquiry(
    state: Arc<AppState>,
    form: ParsedInquiryForm,
) -> Result<(StatusCode, Json<InquiryResponse>), ApiError> {
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
    let (dep_street, dep_city, dep_postal) = parse_address(&departure_address);
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
    .map_err(|e| ApiError::Internal(format!("Auszugsadresse konnte nicht erstellt werden: {e}")))?;

    // 3. Create destination address
    let (arr_street, arr_city, arr_postal) = parse_address(&arrival_address);
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
    .map_err(|e| ApiError::Internal(format!("Einzugsadresse konnte nicht erstellt werden: {e}")))?;

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

    // 6. Create quote
    let quote_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id,
                           status, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        "#,
    )
    .bind(quote_id)
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

    tracing::info!(quote_id = %quote_id, "Quote created for inquiry");

    // 7. Return 202 immediately — spawn background processing
    let state_bg = Arc::clone(&state);
    let dep_addr = departure_address.clone();
    let arr_addr = arrival_address.clone();
    tokio::spawn(async move {
        if let Err(e) = process_inquiry_background(
            state_bg,
            quote_id,
            form.images,
            form.depth_maps,
            form.ar_metadata,
            dep_addr,
            arr_addr,
            now,
        )
        .await
        {
            tracing::error!(quote_id = %quote_id, error = %e, "Background inquiry processing failed");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(InquiryResponse {
            quote_id,
            customer_id,
            status: "processing".to_string(),
            message: "Anfrage erhalten. Bilder werden analysiert und Angebot wird erstellt."
                .to_string(),
        }),
    ))
}

/// Background processing: distance calc → S3 upload → vision estimation → store results → generate offer.
/// Runs in a spawned task so the HTTP response is not blocked.
async fn process_inquiry_background(
    state: Arc<AppState>,
    quote_id: Uuid,
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
                    quote_id = %quote_id,
                    distance_km = result.total_distance_km,
                    "Distance calculated"
                );
                let _ = sqlx::query(
                    "UPDATE quotes SET distance_km = $1, updated_at = $2 WHERE id = $3",
                )
                .bind(result.total_distance_km)
                .bind(chrono::Utc::now())
                .bind(quote_id)
                .execute(&state.db)
                .await;
            }
            Err(e) => {
                tracing::warn!(quote_id = %quote_id, error = %e, "Distance calculation failed, continuing without");
            }
        }
    } else {
        tracing::warn!("Maps API key not configured, skipping distance calculation");
    }

    let estimation_id = Uuid::now_v7();

    // 1. Upload images to S3
    let s3_keys = upload_images_to_s3(&*state.storage, quote_id, estimation_id, &images)
        .await
        .map_err(|e| format!("S3 upload failed: {e}"))?;

    tracing::info!(
        quote_id = %quote_id,
        image_count = images.len(),
        "Images uploaded to S3"
    );

    // Upload depth maps if present
    if !depth_maps.is_empty() {
        if let Err(e) =
            upload_depth_maps_to_s3(&*state.storage, quote_id, estimation_id, &depth_maps).await
        {
            tracing::warn!("Failed to upload depth maps: {e}");
        }
    }

    // 2. Run volume estimation (vision service → LLM fallback)
    let (total_volume, confidence, result_data, method) =
        match try_vision_service(&state, &images, estimation_id, quote_id, estimation_id).await {
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
                fallback_llm_analysis(&state, &images)
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
    sqlx::query(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(estimation_id)
    .bind(quote_id)
    .bind(method.as_str())
    .bind(&source_data)
    .bind(&result_data)
    .bind(total_volume)
    .bind(confidence)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("Failed to store estimation: {e}"))?;

    // 5. Update quote with estimated volume
    sqlx::query(
        "UPDATE quotes SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4",
    )
    .bind(total_volume)
    .bind("volume_estimated")
    .bind(chrono::Utc::now())
    .bind(quote_id)
    .execute(&state.db)
    .await
    .map_err(|e| format!("Failed to update quote: {e}"))?;

    tracing::info!(
        quote_id = %quote_id,
        estimation_id = %estimation_id,
        volume = total_volume,
        "Volume estimation completed"
    );

    // 6. Auto-generate offer (XLSX → PDF → Telegram)
    orchestrator::try_auto_generate_offer(Arc::clone(&state), quote_id).await;

    Ok(())
}

/// Upload images to S3 under `estimates/{quote_id}/{estimation_id}/`.
async fn upload_images_to_s3(
    storage: &dyn StorageProvider,
    quote_id: Uuid,
    estimation_id: Uuid,
    images: &[(Vec<u8>, String)],
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
            .map_err(|e| ApiError::Internal(format!("Bild-Upload fehlgeschlagen: {e}")))?;
        s3_keys.push(key);
    }
    Ok(s3_keys)
}

/// Upload depth maps to S3 under `estimates/{quote_id}/{estimation_id}/depth/`.
async fn upload_depth_maps_to_s3(
    storage: &dyn StorageProvider,
    quote_id: Uuid,
    estimation_id: Uuid,
    depth_maps: &[(Vec<u8>, String)],
) -> Result<Vec<String>, ApiError> {
    let mut s3_keys = Vec::with_capacity(depth_maps.len());
    for (idx, (data, mime_type)) in depth_maps.iter().enumerate() {
        let ext = match mime_type.as_str() {
            "image/png" => "png",
            _ => "bin",
        };
        let key = format!("estimates/{quote_id}/{estimation_id}/depth/{idx}.{ext}");
        storage
            .upload(&key, Bytes::from(data.clone()), mime_type)
            .await
            .map_err(|e| ApiError::Internal(format!("Tiefenkarten-Upload fehlgeschlagen: {e}")))?;
        s3_keys.push(key);
    }
    Ok(s3_keys)
}

/// Try the Python vision service for 3D volume estimation.
/// Sends raw image bytes via multipart upload (Modal doesn't have S3 access).
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

    Ok((response.total_volume_m3, response.confidence_score, Some(items_value)))
}

/// Fallback: run LLM-based vision analysis on the raw image data.
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

/// Parse a free-form address string into (street, city, postal_code).
fn parse_address(addr: &str) -> (String, String, String) {
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
                // English names
                "packing" => parts.push("Verpackungsservice".to_string()),
                "assembly" => parts.push("Montage".to_string()),
                "disassembly" => parts.push("Demontage".to_string()),
                "storage" => parts.push("Einlagerung".to_string()),
                "disposal" => parts.push("Entsorgung".to_string()),
                // German names (from web form)
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
