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

use crate::services::offer_pipeline::try_auto_generate_offer;
use crate::{orchestrator, services, ApiError, AppState};
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
        .route("/video", post(video_inquiry))
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
    pub departure_address: Option<String>,
    pub departure_floor: Option<String>,
    pub departure_parking_ban: Option<bool>,
    pub departure_elevator: Option<bool>,
    pub arrival_address: Option<String>,
    pub arrival_floor: Option<String>,
    pub arrival_parking_ban: Option<bool>,
    pub arrival_elevator: Option<bool>,
    pub preferred_date: Option<String>,
    pub services: Option<String>,
    pub message: Option<String>,
    pub images: Vec<(Vec<u8>, String)>,
    pub depth_maps: Vec<(Vec<u8>, String)>,
    pub ar_metadata: Option<String>,
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

pub(crate) async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> Result<String, ApiError> {
    field
        .text()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Feld konnte nicht gelesen werden: {e}")))
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
    crate::services::db::update_quote_volume(
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
