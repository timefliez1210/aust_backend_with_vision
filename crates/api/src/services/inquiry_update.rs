//! The one write path for editing an inquiry.
//!
//! `PATCH /api/v1/inquiries/{id}` and Josie's inquiry tools both land here, so a
//! change made in Telegram has exactly the side effects of the same change made
//! in the admin UI: the status-transition check, inline address creation, the
//! billing-address sync, the crew moving with a rescheduled job (inside
//! `inquiry_repo::update_fields`) and the `status.changed` domain event. Josie's
//! bridge used to write `inquiries.status` directly and skipped the last three.

use chrono::NaiveTime;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::events::EventEmitter;
use aust_core::models::{InquiryResponse, InquiryStatus, Services};

use crate::repositories::{address_repo, customer_repo, inquiry_repo};
use crate::services::inquiry_builder;
use crate::ApiError;

#[derive(Debug, Default, Deserialize)]
pub struct AddressInput {
    pub street: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
    /// @notice Legacy data (pre-2026-05) may have house numbers embedded in
    ///         `street` (e.g. "Musterstr. 1") with `house_number` = NULL.
    ///         The admin frontend (AddressEditor.svelte) extracts the number
    ///         for display. See `split_street_house_number()` in submissions.rs.
    pub house_number: Option<String>,
    pub parking_ban: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateInquiryRequest {
    pub status: Option<String>,
    pub notes: Option<String>,
    pub services: Option<Services>,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub scheduled_date: Option<String>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub service_type: Option<String>,
    pub submission_mode: Option<String>,
    pub recipient_id: Option<Uuid>,
    /// Inline billing address — if provided, creates an address record and sets billing_address_id.
    /// If both billing_address and billing_address_id are provided, billing_address_id takes priority.
    pub billing_address: Option<AddressInput>,
    pub billing_address_id: Option<Uuid>,
    /// Set to `null` explicitly to clear the billing address override.
    pub clear_billing_address: Option<bool>,
    pub custom_fields: Option<serde_json::Value>,
    pub employee_notes: Option<String>,
    /// Set end_date (YYYY-MM-DD) for multi-day inquiries. Set to `null` to clear (single-day).
    pub end_date: Option<String>,
    /// Set to true to explicitly clear end_date (single-day inquiry).
    pub clear_end_date: Option<bool>,
    /// Enable travel daily allowance (Verpflegungspauschale) for multi-day trips.
    pub has_pauschale: Option<bool>,
    /// Inline stop address — if provided, creates an address record and sets stop_address_id.
    pub stop_address: Option<AddressInput>,
    /// Link to an existing stop address by UUID.
    pub stop_address_id: Option<Uuid>,
    /// Set to `true` to explicitly clear the stop address.
    pub clear_stop_address: Option<bool>,
}

/// Apply a partial update to an inquiry and return the refreshed canonical response.
///
/// Only fields set in `request` change. A status change is validated against the
/// state machine before anything is written.
pub async fn update_inquiry(
    db: &PgPool,
    events: &EventEmitter,
    id: Uuid,
    request: UpdateInquiryRequest,
) -> Result<InquiryResponse, ApiError> {
    let now = chrono::Utc::now();

    // Fetch current inquiry for status checks
    let current_inquiry = inquiry_repo::fetch_by_id(db, id).await?;
    let current_status: InquiryStatus = current_inquiry.status.parse()
        .map_err(|_| ApiError::Internal(format!("Invalid status in DB: {}", current_inquiry.status)))?;

// Validate status transition if status is being changed
    if let Some(ref new_status) = request.status {
        let target_status: InquiryStatus = new_status.parse()
            .map_err(|e| ApiError::Validation(format!("Ungueltiger Status: {e}")))?;
        if !current_status.can_transition_to(&target_status) {
            return Err(ApiError::Validation(format!(
                "Statuswechsel von '{current_status}' nach '{target_status}' ist nicht erlaubt"
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
                db,
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
                db,
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
        db,
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
    // Overwrite the customer's billing address from the inquiry (feedback c7a8081e).
    // Only sync when a billing address was explicitly provided in this request.
    if billing_address_id.is_some() {
        let _ = customer_repo::sync_billing_address(db, current_inquiry.customer_id, billing_address_id).await;
    }

    // Auto-update billing address from origin → destination on completion, and
    // mirror the new address onto the customer: post-move it is the current one
    // (feedback c7a8081e). Best-effort — a failed sync must not fail the update.
    if request.status.as_deref() == Some("completed") {
        match inquiry_repo::auto_update_billing_on_completed(db, id).await {
            Ok(Some(new_billing_id)) => {
                if let Err(e) = customer_repo::sync_billing_address(
                    db,
                    current_inquiry.customer_id,
                    Some(new_billing_id),
                )
                .await
                {
                    tracing::warn!(
                        customer_id = %current_inquiry.customer_id,
                        inquiry_id = %id,
                        error = %e,
                        "Post-completion billing address sync to customer failed"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                inquiry_id = %id,
                error = %e,
                "Auto-update of billing address on completion failed"
            ),
        }
    }

    // Emit status.changed domain event when a status transition was requested (non-fatal).
    if let Some(ref new_status) = request.status {
        let emitter = events.clone();
        let old_status = current_inquiry.status.clone();
        let new_status = new_status.clone();
        let payload = serde_json::json!({
            "inquiry_id": id,
            "old_status": old_status,
            "new_status": new_status,
        });
        let aggregate = format!("inquiry:{id}");
        aust_core::tenant::spawn(async move {
            if let Err(e) = emitter.emit("status.changed", &aggregate, payload).await {
                tracing::warn!("Failed to emit status.changed event: {e}");
            }
        });
    }

    inquiry_builder::build_inquiry_response(db, id).await
}
