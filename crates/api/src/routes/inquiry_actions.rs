//! Action handlers for inquiry-level operations — estimation triggers, offer generation,
//! item updates, and employee assignments.

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::routes::offers::{build_offer_with_overrides, OfferOverrides};
use crate::routes::submissions::{
    parse_inquiry_form, process_submission_background, process_video_background,
};
use crate::services::db::{insert_estimation_no_return, update_quote_volume};
use crate::{services, ApiError, AppState};
use aust_core::models::Offer;
use aust_llm_providers::LlmMessage;
use aust_offer_generator::OfferLineItem;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct GenerateOfferRequest {
    pub valid_days: Option<i64>,
    #[serde(default)]
    pub price_cents_netto: Option<i64>,
    #[serde(default)]
    pub persons: Option<u32>,
    #[serde(default)]
    pub hours: Option<f64>,
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub line_items: Option<Vec<GenerateLineItem>>,
    /// Explicit Fahrkostenpauschale flat total in €. When set, overrides ORS calculation and
    /// is persisted so future regenerations also use it. Send `null` to clear a stored override.
    #[serde(default)]
    pub fahrt_flat_total: Option<f64>,
    /// When true, clears any stored Fahrkostenpauschale override so ORS recalculates it.
    #[serde(default)]
    pub fahrt_reset: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GenerateLineItem {
    pub description: String,
    pub quantity: f64,
    pub unit_price: f64,
    #[serde(default)]
    pub remark: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateEstimationItemsRequest {
    pub items: Vec<UpdateEstimationItem>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct UpdateEstimationItem {
    pub name: String,
    pub volume_m3: f64,
    pub quantity: u32,
    pub confidence: f64,
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    #[serde(default)]
    pub seen_in_images: Option<Vec<usize>>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub dimensions: Option<serde_json::Value>,
    #[serde(default = "default_true")]
    pub is_moveable: bool,
    #[serde(default)]
    pub packs_into_boxes: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub(crate) struct EstimationDetail {
    pub id: Uuid,
    pub method: String,
    pub total_volume_m3: f64,
    pub items: Vec<EstimationItemResponse>,
    pub source_images: Vec<String>,
    pub source_videos: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EstimationItemResponse {
    pub name: String,
    pub volume_m3: f64,
    pub quantity: u32,
    pub confidence: f64,
    pub crop_url: Option<String>,
    pub source_image_url: Option<String>,
    pub bbox: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop_s3_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox_image_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_in_images: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<serde_json::Value>,
    pub is_moveable: bool,
    pub packs_into_boxes: bool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub(crate) struct EmployeeAssignmentRow {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub planned_hours: f64,
    pub clock_in: Option<chrono::DateTime<chrono::Utc>>,
    pub clock_out: Option<chrono::DateTime<chrono::Utc>>,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Action handlers
// ---------------------------------------------------------------------------

/// `PUT /api/v1/inquiries/{id}/items` -- Replace detected items on latest estimation.
///
/// **Caller**: Admin dashboard item editor.
/// **Why**: ML/LLM pipeline may produce duplicates or errors. This lets the admin
///          correct items before regenerating the offer.
pub(crate) async fn update_inquiry_items(
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
pub(crate) async fn trigger_estimate(
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
pub(crate) async fn generate_inquiry_offer(
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
pub(crate) async fn generate_offer_email_draft(state: &AppState, inquiry_id: Uuid) {
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
pub(crate) async fn find_or_create_inquiry_thread(state: &AppState, inquiry_id: Uuid) -> Uuid {
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

/// `POST /api/v1/inquiries/{id}/estimate/depth` and `/estimate/video`
///
/// **Caller**: Admin dashboard — triggers vision pipeline on an existing inquiry.
/// **Why**: Accepts multipart image/video upload, runs S3 upload + vision estimation
///          in the background, and auto-generates an offer when complete.
pub(crate) async fn trigger_estimate_upload(
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

/// `POST /api/v1/inquiries/{id}/estimate/video`
///
/// **Caller**: Admin dashboard — triggers video 3D pipeline on an existing inquiry.
/// **Why**: Accepts multipart video upload, saves the file to S3, then queues it for
///          processing on the Modal video endpoint (MASt3R + SAM 2 pipeline).
///          Returns immediately with a processing estimation ID for polling.
pub(crate) async fn trigger_video_upload(
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

// ---------------------------------------------------------------------------
// Employee assignment endpoints
// ---------------------------------------------------------------------------

/// `GET /api/v1/inquiries/{id}/employees` — List employees assigned to this inquiry.
///
/// **Caller**: Inquiry detail Mitarbeiter card.
/// **Why**: Shows which employees are assigned to a job and their hours.
///
/// # Returns
/// `200 OK` with `{ assignments: [...] }`.
pub(crate) async fn list_inquiry_employees(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows: Vec<EmployeeAssignmentRow> = sqlx::query_as(
        r#"
        SELECT ie.employee_id, e.first_name, e.last_name, e.email,
               ie.planned_hours::float8 AS planned_hours,
               ie.clock_in,
               ie.clock_out,
               CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (ie.clock_out - ie.clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
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
pub(crate) async fn assign_employee(
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
pub(crate) async fn update_assignment(
    State(state): State<Arc<AppState>>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<aust_core::models::UpdateAssignment>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = sqlx::query(
        r#"
        UPDATE inquiry_employees SET
            clock_in  = COALESCE($4, clock_in),
            clock_out = COALESCE($5, clock_out),
            planned_hours = CASE
                WHEN COALESCE($4, clock_in) IS NOT NULL AND COALESCE($5, clock_out) IS NOT NULL
                THEN (EXTRACT(EPOCH FROM (COALESCE($5, clock_out) - COALESCE($4, clock_in))) / 3600.0)::float8
                ELSE COALESCE($3, planned_hours)
            END,
            notes = COALESCE($6, notes)
        WHERE inquiry_id = $1 AND employee_id = $2
        "#,
    )
    .bind(id)
    .bind(emp_id)
    .bind(body.planned_hours)
    .bind(body.clock_in)
    .bind(body.clock_out)
    .bind(&body.notes)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Zuweisung nicht gefunden".into()));
    }

    #[derive(sqlx::FromRow)]
    struct Updated {
        planned_hours: f64,
        clock_in: Option<chrono::DateTime<chrono::Utc>>,
        clock_out: Option<chrono::DateTime<chrono::Utc>>,
        actual_hours: Option<f64>,
        notes: Option<String>,
    }

    let row: Updated = sqlx::query_as(
        r#"
        SELECT planned_hours::float8 AS planned_hours,
               clock_in,
               clock_out,
               CASE WHEN clock_out IS NOT NULL AND clock_in IS NOT NULL
                    THEN (EXTRACT(EPOCH FROM (clock_out - clock_in)) / 3600.0)::float8
                    ELSE NULL END AS actual_hours,
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
        "clock_in": row.clock_in,
        "clock_out": row.clock_out,
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
pub(crate) async fn remove_assignment(
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
