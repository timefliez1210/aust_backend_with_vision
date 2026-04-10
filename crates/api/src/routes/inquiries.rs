//! Inquiry endpoints — main CRUD resource for the unified inquiry lifecycle.
//!
//! Action handlers (estimate triggers, offer generation, employee assignments) live in
//! `inquiry_actions.rs`. Public submission handlers live in `submissions.rs`.

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Extension, Json, Router,
};
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::repositories::{address_repo, customer_repo, estimation_repo, inquiry_repo, offer_repo};
use crate::routes::inquiry_actions::{
    assign_employee, generate_inquiry_offer, list_inquiry_employees, remove_assignment,
    trigger_estimate, trigger_estimate_upload, trigger_video_upload, update_assignment,
    update_inquiry_items,
};
use crate::services::inquiry_builder;
use crate::{ApiError, AppState};
use aust_core::models::{
    InquiryListItem, InquiryResponse as InquiryResponseModel, InquiryStatus, Services, TokenClaims,
};

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
        .route("/{id}/estimate/depth", post(trigger_estimate_upload))
        .route("/{id}/estimate/video", post(trigger_video_upload))
        .route("/{id}/estimate/{method}", post(trigger_estimate))
        .route("/{id}/generate-offer", post(generate_inquiry_offer))
        .route("/{id}/emails", get(get_inquiry_emails))
        .route(
            "/{id}/employees",
            get(list_inquiry_employees).post(assign_employee),
        )
        .route(
            "/{id}/employees/{emp_id}",
            axum::routing::patch(update_assignment)
                .delete(remove_assignment),
        )
        .merge(super::invoices::router())
        .merge(super::calendar::inquiry_days_router())
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AddressInput {
    street: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateInquiryRequest {
    customer_id: Uuid,
    origin: Option<AddressInput>,
    destination: Option<AddressInput>,
    services: Option<Services>,
    notes: Option<String>,
    scheduled_date: Option<String>,
    distance_km: Option<f64>,
    estimated_volume_m3: Option<f64>,
    items_list: Option<String>,
    service_type: Option<String>,
    submission_mode: Option<String>,
    recipient_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
    custom_fields: Option<serde_json::Value>,
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
    scheduled_date: Option<String>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
    service_type: Option<String>,
    submission_mode: Option<String>,
    recipient_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
    custom_fields: Option<serde_json::Value>,
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
    let now = chrono::Utc::now();

    // Verify customer exists
    if !customer_repo::exists(&state.db, request.customer_id).await? {
        return Err(ApiError::Validation("Kunde nicht gefunden".into()));
    }

    // Create origin address if provided
    let origin_id = if let Some(ref addr) = request.origin {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let id = address_repo::create(
                &state.db,
                &street,
                &city,
                addr.postal_code.as_deref(),
                addr.floor.as_deref(),
                addr.elevator,
        None,
        None,
            )
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Create destination address if provided
    let dest_id = if let Some(ref addr) = request.destination {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let id = address_repo::create(
                &state.db,
                &street,
                &city,
                addr.postal_code.as_deref(),
                addr.floor.as_deref(),
                addr.elevator,
        None,
        None,
            )
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    let scheduled_date = request.scheduled_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    });

    let services_json = serde_json::to_value(request.services.unwrap_or_default())
        .unwrap_or(serde_json::json!({}));

    // If volume is provided manually, start as estimated; otherwise pending
    let initial_status = if request.estimated_volume_m3.is_some() {
        InquiryStatus::Estimated
    } else {
        InquiryStatus::Pending
    };

    let inquiry_id = Uuid::now_v7();
    inquiry_repo::create(
        &state.db,
        inquiry_id,
        request.customer_id,
        origin_id,
        dest_id,
        None,
        initial_status.as_str(),
        request.estimated_volume_m3,
        request.distance_km,
        scheduled_date,
        request.notes.as_deref(),
        &services_json,
        "admin_dashboard",
        request.service_type.as_deref(),
        request.submission_mode.as_deref(),
        request.recipient_id,
        request.billing_address_id,
        request.custom_fields.as_ref().unwrap_or(&serde_json::json!({})),
        now,
    )
    .await?;

    // Store manual volume estimation if provided
    if let Some(volume) = request.estimated_volume_m3 {
        let estimation_id = Uuid::now_v7();
        let source_data = serde_json::json!({"source": "admin_dashboard"});
        let items_text = request.items_list.as_deref().unwrap_or("");
        let result_data = serde_json::json!({"items_text": items_text});
        estimation_repo::insert_no_return(
            &state.db,
            estimation_id,
            inquiry_id,
            "inventory",
            &source_data,
            Some(&result_data),
            volume,
            0.9,
            now,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Estimation insert failed: {e}")))?;
    }

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

    // Serialize services if provided
    let services_json = request
        .services
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());

    let scheduled_date = request.scheduled_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    });

    inquiry_repo::update_fields(
        &state.db,
        id,
        request.status.as_deref(),
        request.notes.as_deref(),
        services_json.as_ref(),
        request.estimated_volume_m3,
        request.distance_km,
        request.start_time,
        request.end_time,
        request.origin_address_id,
        scheduled_date,
        request.destination_address_id,
        request.service_type.as_deref(),
        request.submission_mode.as_deref(),
        request.recipient_id,
        request.billing_address_id,
        request.custom_fields.as_ref(),
        now,
    )
    .await?;

    // Auto-update billing address from origin → destination on completion
    if request.status.as_deref() == Some("completed") {
        let _ = inquiry_repo::auto_update_billing_on_completed(&state.db, id).await;
    }

    let response = inquiry_builder::build_inquiry_response(&state.db, id).await?;
    Ok(Json(response))
}

/// `DELETE /api/v1/inquiries/{id}` -- Hard-delete inquiry and all related records.
///
/// **Caller**: Admin dashboard trash-bin button.
/// **Why**: Permanently removes the inquiry. FK constraints with CASCADE DELETE
/// handle offers, estimations, items, email threads, and bookings automatically.
async fn delete_inquiry(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if !claims.role.is_admin() {
        return Err(ApiError::Forbidden(
            "Diese Aktion erfordert Administrator-Berechtigungen".into(),
        ));
    }
    let rows_affected = inquiry_repo::hard_delete(&state.db, id).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(format!("Inquiry {id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
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
    let (offer_id, storage_key) =
        offer_repo::fetch_active_pdf_key(&state.db, id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

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
    let filename = if let Ok(Some((offer_num, last_name))) =
        offer_repo::fetch_offer_filename_parts(&state.db, offer_id).await
    {
        offer_repo::build_offer_filename(&offer_num, &last_name, ext)
    } else {
        format!("Angebot-{offer_id}.{ext}")
    };

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

/// `GET /api/v1/inquiries/{id}/emails` -- Fetch email thread for this inquiry.
///
/// **Caller**: Admin dashboard inquiry detail email tab.
/// **Why**: Shows linked email conversations for the inquiry.
async fn get_inquiry_emails(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<Vec<inquiry_repo::EmailThreadSummary>>, ApiError> {
    let threads = inquiry_repo::fetch_email_threads(&state.db, inquiry_id).await?;
    Ok(Json(threads))
}
