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
