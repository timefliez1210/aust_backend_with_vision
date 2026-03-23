use sqlx::FromRow;
use uuid::Uuid;
use aust_core::models::{Inquiry, InquiryStatus, Services};

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
    pub preferred_date: Option<chrono::DateTime<chrono::Utc>>,
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

        let services: Option<Services> =
            serde_json::from_value(row.services).ok();

        Inquiry {
            id: row.id,
            customer_id: row.customer_id,
            origin_address_id: row.origin_address_id,
            destination_address_id: row.destination_address_id,
            stop_address_id: row.stop_address_id,
            status,
            estimated_volume_m3: row.estimated_volume_m3,
            distance_km: row.distance_km,
            preferred_date: row.preferred_date,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_inquiry_row(status: &str) -> InquiryRow {
        let dummy_id = Uuid::nil();
        let now = chrono::DateTime::from_timestamp(0, 0).unwrap();
        InquiryRow {
            id: dummy_id,
            customer_id: dummy_id,
            origin_address_id: None,
            destination_address_id: None,
            stop_address_id: None,
            status: status.to_string(),
            estimated_volume_m3: None,
            distance_km: None,
            preferred_date: None,
            notes: None,
            services: serde_json::json!({}),
            source: "direct_email".to_string(),
            offer_sent_at: None,
            accepted_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn inquiry_row_status_mapping() {
        let cases = [
            ("pending", InquiryStatus::Pending),
            ("info_requested", InquiryStatus::InfoRequested),
            ("estimating", InquiryStatus::Estimating),
            ("estimated", InquiryStatus::Estimated),
            ("offer_ready", InquiryStatus::OfferReady),
            ("offer_sent", InquiryStatus::OfferSent),
            ("accepted", InquiryStatus::Accepted),
            ("rejected", InquiryStatus::Rejected),
            ("expired", InquiryStatus::Expired),
            ("cancelled", InquiryStatus::Cancelled),
            ("scheduled", InquiryStatus::Scheduled),
            ("completed", InquiryStatus::Completed),
            ("invoiced", InquiryStatus::Invoiced),
            ("paid", InquiryStatus::Paid),
            ("unknown_value", InquiryStatus::Pending),
        ];

        for (status_str, expected) in cases {
            let row = make_test_inquiry_row(status_str);
            let inquiry = Inquiry::from(row);
            assert_eq!(
                inquiry.status, expected,
                "status '{}' should map to {:?}",
                status_str, expected
            );
        }
    }
}
