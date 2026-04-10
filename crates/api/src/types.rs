//! Shared types used by both route handlers and service/repository layers.
//!
//! Types here sit below `routes/` in the dependency hierarchy so that services
//! and repositories can import them without creating a circular dependency
//! (services → routes).

use aust_core::models::{Inquiry, InquiryStatus, Services};
use sqlx::FromRow;
use uuid::Uuid;

/// SQLx projection of the `inquiries` table used by offer generation and offer-repo queries.
///
/// **Callers**: `repositories::offer_repo::fetch_inquiry_for_offer`,
///              `services::offer_builder::build_offer_with_overrides`
/// **Why**: A lightweight row struct that both the repository and service layers can share
/// without pulling in any route-level types.
#[derive(Debug, FromRow)]
pub(crate) struct InquiryRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    #[sqlx(default)]
    pub stop_address_id: Option<Uuid>,
    pub status: String,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub scheduled_date: Option<chrono::NaiveDate>,
    pub notes: Option<String>,
    #[sqlx(default)]
    pub services: serde_json::Value,
    #[sqlx(default)]
    pub source: String,
    #[sqlx(default)]
    pub service_type: Option<String>,
    #[sqlx(default)]
    pub submission_mode: Option<String>,
    #[sqlx(default)]
    pub recipient_id: Option<Uuid>,
    #[sqlx(default)]
    pub inquiry_billing_address_id: Option<Uuid>,
    #[sqlx(default)]
    pub custom_fields: serde_json::Value,
    #[sqlx(default)]
    pub offer_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    #[sqlx(default)]
    pub accepted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<InquiryRow> for Inquiry {
    fn from(row: InquiryRow) -> Self {
        let status: InquiryStatus = row.status.parse().unwrap_or_default();

        let services: Option<Services> = serde_json::from_value(row.services).ok();

        Inquiry {
            id: row.id,
            customer_id: row.customer_id,
            origin_address_id: row.origin_address_id,
            destination_address_id: row.destination_address_id,
            stop_address_id: row.stop_address_id,
            service_type: row.service_type,
            submission_mode: row.submission_mode,
            recipient_id: row.recipient_id,
            billing_address_id: row.inquiry_billing_address_id,
            custom_fields: row.custom_fields,
            status,
            estimated_volume_m3: row.estimated_volume_m3,
            distance_km: row.distance_km,
            preferred_date: None,
            scheduled_date: row.scheduled_date,
            notes: row.notes,
            source: Some(row.source),
            services,
            offer_sent_at: row.offer_sent_at,
            accepted_at: row.accepted_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Resolve the billing address for an inquiry.
///
/// Logic:
/// - If `inquiry_billing_address_id` is set → use it (explicit override).
/// - If inquiry status >= "completed" → use destination (they've moved).
/// - Otherwise → use origin (they still live there).
///
/// This function does NOT auto-mutate the billing address — that happens
/// in the status-update handler when transitioning to "completed".
pub(crate) fn resolve_billing_address_id(
    inquiry_billing_address_id: Option<Uuid>,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
    status: &str,
) -> Option<Uuid> {
    // Explicit billing address always wins
    if inquiry_billing_address_id.is_some() {
        return inquiry_billing_address_id;
    }
    // After move completion, billing goes to destination
    if matches!(status, "completed" | "invoiced" | "paid") {
        return destination_address_id;
    }
    // Default: they still live at origin
    origin_address_id
}
