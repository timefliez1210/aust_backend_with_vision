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

use crate::routes::inquiry_actions::{
    assign_employee, generate_inquiry_offer, list_inquiry_employees, remove_assignment,
    trigger_estimate, trigger_estimate_upload, trigger_video_upload, update_assignment,
    update_inquiry_items,
};
use crate::services::db::insert_estimation_no_return;
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
    preferred_date: Option<String>,
    distance_km: Option<f64>,
    estimated_volume_m3: Option<f64>,
    items_list: Option<String>,
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
    preferred_date: Option<String>,
    scheduled_date: Option<String>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow, Serialize)]
struct EmailThreadSummary {
    id: Uuid,
    subject: Option<String>,
    last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    message_count: i64,
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
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM customers WHERE id = $1)")
        .bind(request.customer_id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::Validation("Kunde nicht gefunden".into()));
    }

    // Create origin address if provided
    let origin_id = if let Some(ref addr) = request.origin {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&addr.postal_code)
            .bind(&addr.floor)
            .bind(addr.elevator)
            .fetch_one(&state.db)
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
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&addr.postal_code)
            .bind(&addr.floor)
            .bind(addr.elevator)
            .fetch_one(&state.db)
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    let preferred_date = request
        .preferred_date
        .as_deref()
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(10, 0, 0))
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    let services_json = serde_json::to_value(request.services.unwrap_or_default())
        .unwrap_or(serde_json::json!({}));

    // If volume is provided manually, start as estimated; otherwise pending
    let initial_status = if request.estimated_volume_m3.is_some() {
        InquiryStatus::Estimated
    } else {
        InquiryStatus::Pending
    };

    let inquiry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
                           status, estimated_volume_m3, preferred_date, notes, services,
                           distance_km, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)
        "#,
    )
    .bind(inquiry_id)
    .bind(request.customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(initial_status.as_str())
    .bind(request.estimated_volume_m3)
    .bind(preferred_date)
    .bind(&request.notes)
    .bind(&services_json)
    .bind(request.distance_km)
    .bind("admin_dashboard")
    .bind(now)
    .execute(&state.db)
    .await?;

    // Store manual volume estimation if provided
    if let Some(volume) = request.estimated_volume_m3 {
        let estimation_id = Uuid::now_v7();
        let source_data = serde_json::json!({"source": "admin_dashboard"});
        let items_text = request.items_list.as_deref().unwrap_or("");
        let result_data = serde_json::json!({"items_text": items_text});
        insert_estimation_no_return(
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


    // Parse preferred date if provided
    let preferred_date = request.preferred_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(10, 0, 0))
            .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc))
    });

    // Serialize services if provided
    let services_json = request
        .services
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());

    let scheduled_date = request.scheduled_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    });

    sqlx::query(
        r#"
        UPDATE inquiries SET
            status = COALESCE($2, status),
            notes = COALESCE($3, notes),
            services = COALESCE($4, services),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            preferred_date = COALESCE($7, preferred_date),
            scheduled_date = CASE WHEN $7 IS NOT NULL THEN NULL WHEN $11 IS NOT NULL THEN $11 ELSE scheduled_date END,
            start_time = COALESCE($8, start_time),
            end_time = COALESCE($9, end_time),
            origin_address_id = COALESCE($10, origin_address_id),
            destination_address_id = COALESCE($12, destination_address_id),
            updated_at = $13
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(request.status.as_deref())
    .bind(&request.notes)
    .bind(&services_json)
    .bind(request.estimated_volume_m3)
    .bind(request.distance_km)
    .bind(preferred_date)
    .bind(request.start_time)
    .bind(request.end_time)
    .bind(request.origin_address_id)
    .bind(scheduled_date)
    .bind(request.destination_address_id)
    .bind(now)
    .execute(&state.db)
    .await?;

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
    let rows_affected = sqlx::query("DELETE FROM inquiries WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();

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
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, pdf_storage_key FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (offer_id, storage_key) =
        row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

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
    let filename = format!("Angebot-{}.{ext}", offer_id);

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
) -> Result<Json<Vec<EmailThreadSummary>>, ApiError> {
    let threads: Vec<EmailThreadSummary> = sqlx::query_as(
        r#"
        SELECT
            et.id,
            et.subject,
            (SELECT MAX(em.created_at) FROM email_messages em WHERE em.thread_id = et.id) AS last_message_at,
            COALESCE((SELECT COUNT(*) FROM email_messages em WHERE em.thread_id = et.id), 0) AS message_count
        FROM email_threads et
        WHERE et.inquiry_id = $1
        ORDER BY et.created_at DESC
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(threads))
}
