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

use crate::repositories::{address_repo, calendar_repo, customer_repo, estimation_repo, inquiry_repo, offer_repo};
use crate::routes::estimates::collect_estimation_s3_keys;
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
            get(list_inquiry_employees).post(assign_employee).put(put_inquiry_employees),
        )
        .route(
            "/{id}/employees/{emp_id}",
            axum::routing::patch(update_assignment)
                .delete(remove_assignment),
        )
        .route("/{id}/employees/{emp_id}/travel-expenses", get(generate_travel_expenses))
        .merge(super::invoices::router())
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct AddressInput {
    pub(crate) street: Option<String>,
    pub(crate) city: Option<String>,
    pub(crate) postal_code: Option<String>,
    pub(crate) floor: Option<String>,
    pub(crate) elevator: Option<bool>,
    /// @notice Legacy data (pre-2026-05) may have house numbers embedded in
    ///         `street` (e.g. "Musterstr. 1") with `house_number` = NULL.
    ///         The admin frontend (AddressEditor.svelte) extracts the number
    ///         for display. See `split_street_house_number()` in submissions.rs.
    pub(crate) house_number: Option<String>,
    pub(crate) parking_ban: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateInquiryRequest {
    customer_id: Uuid,
    origin: Option<AddressInput>,
    destination: Option<AddressInput>,
    /// Inline stop address — if provided, creates an address record and sets stop_address_id.
    stop: Option<AddressInput>,
    services: Option<Services>,
    notes: Option<String>,
    scheduled_date: Option<String>,
    distance_km: Option<f64>,
    estimated_volume_m3: Option<f64>,
    items_list: Option<String>,
    service_type: Option<String>,
    submission_mode: Option<String>,
    recipient_id: Option<Uuid>,
    /// Inline billing address — if provided, creates an address record and sets billing_address_id.
    /// If both billing_address and billing_address_id are provided, billing_address_id takes priority.
    billing_address: Option<AddressInput>,
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
    /// Inline billing address — if provided, creates an address record and sets billing_address_id.
    /// If both billing_address and billing_address_id are provided, billing_address_id takes priority.
    billing_address: Option<AddressInput>,
    billing_address_id: Option<Uuid>,
    /// Set to `null` explicitly to clear the billing address override.
    clear_billing_address: Option<bool>,
    custom_fields: Option<serde_json::Value>,
    employee_notes: Option<String>,
    /// Set end_date (YYYY-MM-DD) for multi-day inquiries. Set to `null` to clear (single-day).
    end_date: Option<String>,
    /// Set to true to explicitly clear end_date (single-day inquiry).
    clear_end_date: Option<bool>,
    /// Enable travel daily allowance (Verpflegungspauschale) for multi-day trips.
    has_pauschale: Option<bool>,
    /// Inline stop address — if provided, creates an address record and sets stop_address_id.
    stop_address: Option<AddressInput>,
    /// Link to an existing stop address by UUID.
    stop_address_id: Option<Uuid>,
    /// Set to `true` to explicitly clear the stop address.
    clear_stop_address: Option<bool>,
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
                addr.house_number.as_deref(),
                addr.parking_ban,
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
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Create stop address if provided inline
    let stop_id = if let Some(ref addr) = request.stop {
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
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(id)
        } else {
            None
        }
    } else {
        None
    };

    // Create billing address if provided inline
    let billing_address_id = if let Some(ref addr) = request.billing_address {
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
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(id)
        } else {
            request.billing_address_id
        }
    } else {
        request.billing_address_id
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
        stop_id,
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
        billing_address_id,
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

    // Fetch current inquiry for status checks
    let current_inquiry = inquiry_repo::fetch_by_id(&state.db, id).await?;
    let current_status: InquiryStatus = current_inquiry.status.parse()
        .map_err(|_| ApiError::Internal(format!("Invalid status in DB: {}", current_inquiry.status)))?;

    // M3: Gate mutable fields on current status
    // Once an inquiry has an offer (offer_ready or beyond), volume, services,
    // distance, and addresses are locked — changing them would break the accepted offer.
    if current_status.is_locked_for_modifications() {
        let locked_fields_modified = request.estimated_volume_m3.is_some()
            || request.services.is_some()
            || request.distance_km.is_some()
            || request.origin_address_id.is_some()
            || request.destination_address_id.is_some()
            || request.stop_address_id.is_some()
            || request.stop_address.is_some()
            || request.clear_stop_address.unwrap_or(false);
        if locked_fields_modified {
            return Err(ApiError::Validation(
                "Inquiry mit vorhandenem Angebot kann nicht mehr inhaltlich geändert werden (Volumen, Services, Entfernung, Adressen). Bitte Angebot neu erstellen.".into(),
            ));
        }
    }

    // Validate status transition if status is being changed
    if let Some(ref new_status) = request.status {
        let target_status: InquiryStatus = new_status.parse()
            .map_err(|e| ApiError::Validation(format!("Ungueltiger Status: {e}")))?;
        if !current_status.can_transition_to(&target_status) {
            return Err(ApiError::Validation(format!(
                "Statuswechsel von '{}' nach '{}' ist nicht erlaubt",
                current_status, target_status
            )));
        }
    }

    // Serialize services if provided
    let services_json = request
        .services
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());

    // Create billing address if provided inline
    let resolved_billing_address_id = if let Some(ref addr) = request.billing_address {
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
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(id)
        } else {
            request.billing_address_id
        }
    } else {
        request.billing_address_id
    };

    // If clear_billing_address is true, set to None (fall back to resolution chain)
    let billing_address_id = if request.clear_billing_address.unwrap_or(false) {
        None
    } else {
        resolved_billing_address_id
    };

    // Resolve stop address.
    // None = leave unchanged; Some(None) = clear; Some(Some(id)) = set.
    let stop_address_id: Option<Option<Uuid>> = if request.clear_stop_address.unwrap_or(false) {
        Some(None)
    } else if let Some(ref addr) = request.stop_address {
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
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(Some(id))
        } else {
            request.stop_address_id.map(Some)
        }
    } else {
        request.stop_address_id.map(Some)
    };

    let scheduled_date = request.scheduled_date.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    });

    let end_date: Option<Option<chrono::NaiveDate>> = if request.clear_end_date.unwrap_or(false) {
        Some(None)
    } else {
        request.end_date.as_deref().and_then(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
        }).map(Some)
    };

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
        stop_address_id,
        request.service_type.as_deref(),
        request.submission_mode.as_deref(),
        request.recipient_id,
        billing_address_id,
        request.custom_fields.as_ref(),
        request.employee_notes.as_deref(),
        end_date,
        request.has_pauschale,
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

    // 0. Check for active bookings — prevent hard-delete if employees are assigned
    let (day_count, emp_count) = inquiry_repo::count_active_days_and_employees(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(format!("Buchungs-Abfrage fehlgeschlagen: {e}")))?;

    if day_count > 0 || emp_count > 0 {
        return Err(ApiError::Validation(
            format!("Inquiry {id} hat aktive Buchungen ({day_count} Tage, {emp_count} Mitarbeiterzuweisungen). Bitte zuerst alle Buchungen entfernen."),
        ));
    }

    // 1. Collect S3 keys before deleting DB rows (CASCADE would remove them)
    //    Track S3 deletion results for retry/logging (L2)
    let mut s3_delete_failures: Vec<String> = Vec::new();

    //    Offer PDFs
    let offer_keys = offer_repo::fetch_all_pdf_keys(&state.db, id).await.unwrap_or_default();
    for key in &offer_keys {
        if let Err(e) = state.storage.delete(key).await {
            tracing::warn!(inquiry_id = %id, key = %key, error = %e, "Failed to delete offer PDF from S3");
            s3_delete_failures.push(key.clone());
        }
    }
    //    Estimation images and depth maps
    let estimations = estimation_repo::fetch_all_estimations_for_inquiry(&state.db, id).await.unwrap_or_default();
    for (source_data, result_data) in &estimations {
        let keys = collect_estimation_s3_keys(source_data, result_data.as_ref());
        for key in keys {
            if let Err(e) = state.storage.delete(&key).await {
                tracing::warn!(inquiry_id = %id, key = %key, error = %e, "Failed to delete estimation file from S3");
                s3_delete_failures.push(key);
            }
        }
    }
    if !s3_delete_failures.is_empty() {
        tracing::error!(
            inquiry_id = %id,
            failed_count = s3_delete_failures.len(),
            failed_keys = ?s3_delete_failures,
            "S3 orphan keys remaining after inquiry deletion — manual cleanup may be needed"
        );
    }

    // 2. Delete DB rows (CASCADE handles offers, estimations, etc.)
    let rows_affected = inquiry_repo::hard_delete(&state.db, id).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(format!("Inquiry {id} not found")));
    }

    tracing::info!(admin = %claims.sub, admin_email = %claims.email, inquiry_id = %id, "Admin hard-deleted inquiry");
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
        .map_err(|e| match e {
            aust_storage::StorageError::NotFound(_) => {
                ApiError::NotFound("Angebot-PDF nicht gefunden.".into())
            }
            _ => ApiError::Internal(format!("Download fehlgeschlagen: {e}")),
        })?;

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

/// `GET /api/v1/inquiries/{id}/employees/{emp_id}/travel-expenses` — Generate travel expense XLSX.
///
/// **Caller**: Admin dashboard download button on multi-day inquiries with has_pauschale=true.
/// **Why**: Produces a Reisekostenabrechnung for one employee on this trip.
async fn generate_travel_expenses(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, emp_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Fetch inquiry with end_date
    let inquiry = inquiry_repo::fetch_by_id(&state.db, id).await?;
    if !inquiry.has_pauschale {
        return Err(ApiError::BadRequest(
            "Verpflegungspauschale ist für diese Anfrage nicht aktiviert".into(),
        ));
    }
    let Some(start_date) = inquiry.scheduled_date else {
        return Err(ApiError::BadRequest("Kein Startdatum gesetzt".into()));
    };
    let end_date = inquiry.end_date.unwrap_or(start_date);
    let total_days = (end_date - start_date).num_days() + 1;
    if total_days < 2 {
        return Err(ApiError::BadRequest(
            "Verpflegungspauschale nur für mehrtägige Termine".into(),
        ));
    }

    // 2. Fetch employee assignments for this inquiry + employee
    let assignments = calendar_repo::fetch_inquiry_employees(&state.db, id).await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let emp_rows: Vec<_> = assignments.into_iter()
        .filter(|a| a.employee_id == emp_id)
        .collect();
    if emp_rows.is_empty() {
        return Err(ApiError::NotFound("Mitarbeiter nicht zugewiesen".into()));
    }

    // 3. Compute small / large days based on actual hours
    let mut small_days = 0i32;
    let mut large_days = 0i32;
    for a in &emp_rows {
        let is_first = a.job_date == start_date;
        let is_last = a.job_date == end_date;
        let hours = a.actual_hours.unwrap_or(8.0);
        if is_first || is_last {
            // >8h on first/last day → large allowance; otherwise small
            if hours > 8.0 {
                large_days += 1;
            } else {
                small_days += 1;
            }
        } else {
            large_days += 1;
        }
    }

    // 4. Build destination / reason strings
    let dest = if let Some(dest_addr_id) = inquiry.destination_address_id {
        address_repo::fetch_by_id(&state.db, dest_addr_id).await
            .ok()
            .flatten()
            .map(|a| format!("{}, {}", a.street, a.city))
            .unwrap_or_else(|| "—".into())
    } else {
        "—".into()
    };
    let reason = inquiry.notes.clone().unwrap_or_else(|| "Umzug".into());

    // 5. Aggregate per-employee travel fields (take from first row, they're the same across days)
    let first = &emp_rows[0];
    let travel_costs_eur = first.travel_costs_cents.map(|c| c as f64 / 100.0).unwrap_or(0.0);
    let accommodation_eur = first.accommodation_cents.map(|c| c as f64 / 100.0).unwrap_or(0.0);
    let misc_costs_eur = first.misc_costs_cents.map(|c| c as f64 / 100.0).unwrap_or(0.0);

    // 6. Meal deductions (simplified)
    let mut breakfast_deduction = 0.0;
    let mut meal_deduction = 0.0;
    if let Some(ref md) = first.meal_deduction {
        if md.contains("breakfast") {
            breakfast_deduction = small_days as f64 * 14.0 * 0.20 + large_days as f64 * 28.0 * 0.20;
        }
        if md.contains("lunch") || md.contains("dinner") {
            meal_deduction = small_days as f64 * 14.0 * 0.40 + large_days as f64 * 28.0 * 0.40;
        }
    }

    // 7. Generate XLSX
    let xlsx_bytes = aust_offer_generator::generate_travel_expense_xlsx(
        &aust_offer_generator::TravelExpenseData {
            employee_first_name: first.first_name.clone(),
            employee_last_name: first.last_name.clone(),
            start_date,
            start_time: inquiry.start_time,
            end_date,
            end_time: inquiry.end_time,
            destination: dest,
            reason,
            transport_mode: first.transport_mode.clone(),
            travel_costs_eur,
            small_days,
            large_days,
            breakfast_deduction_eur: breakfast_deduction,
            meal_deduction_eur: meal_deduction,
            accommodation_eur,
            misc_costs_eur,
        }
    )
    .map_err(|e| ApiError::Internal(format!("XLSX generation failed: {e}")))?;

    let filename = format!(
        "Reisekosten_{}_{}.xlsx",
        first.last_name, start_date
    );

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename)),
        ],
        xlsx_bytes,
    ))
}

#[derive(Debug, Deserialize)]
struct BulkEmployeeAssignmentBody {
    employee_id: Uuid,
    job_date: chrono::NaiveDate,
    notes: Option<String>,
    start_time: Option<NaiveTime>,
    end_time: Option<NaiveTime>,
    clock_in: Option<NaiveTime>,
    clock_out: Option<NaiveTime>,
    break_minutes: Option<i32>,
    actual_hours: Option<f64>,
    transport_mode: Option<String>,
    travel_costs_cents: Option<i64>,
    accommodation_cents: Option<i64>,
    misc_costs_cents: Option<i64>,
    meal_deduction: Option<String>,
}

/// `PUT /api/v1/inquiries/{id}/employees` — Full-replace all employee assignments.
///
/// **Caller**: Admin calendar side panel (multi-day scheduling).
/// **Why**: Atomic replace of the entire assignment set — deletes all existing rows for this
///          inquiry and inserts the provided flat array (one row per employee per job_date).
async fn put_inquiry_employees(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<Vec<BulkEmployeeAssignmentBody>>,
) -> Result<Json<Vec<calendar_repo::EmployeeAssignmentRow>>, ApiError> {
    let inputs: Vec<calendar_repo::EmployeeAssignmentInput> = body.into_iter().map(|b| {
        calendar_repo::EmployeeAssignmentInput {
            employee_id: b.employee_id,
            job_date: b.job_date,
            notes: b.notes,
            start_time: b.start_time,
            end_time: b.end_time,
            clock_in: b.clock_in,
            clock_out: b.clock_out,
            break_minutes: b.break_minutes.unwrap_or(0),
            actual_hours: b.actual_hours,
            transport_mode: b.transport_mode,
            travel_costs_cents: b.travel_costs_cents,
            accommodation_cents: b.accommodation_cents,
            misc_costs_cents: b.misc_costs_cents,
            meal_deduction: b.meal_deduction,
        }
    }).collect();

    calendar_repo::put_inquiry_employees(&state.db, id, &inputs)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let rows = calendar_repo::fetch_inquiry_employees(&state.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(rows))
}
