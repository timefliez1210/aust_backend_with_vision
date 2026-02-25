use sqlx::FromRow;
use uuid::Uuid;
use aust_core::models::{Quote, QuoteStatus};

#[derive(Debug, FromRow)]
pub(crate) struct QuoteRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub status: String,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub preferred_date: Option<chrono::DateTime<chrono::Utc>>,
    pub notes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<QuoteRow> for Quote {
    fn from(row: QuoteRow) -> Self {
        let status = match row.status.as_str() {
            "pending" => QuoteStatus::Pending,
            "info_requested" => QuoteStatus::InfoRequested,
            "volume_estimated" => QuoteStatus::VolumeEstimated,
            "offer_generated" => QuoteStatus::OfferGenerated,
            "offer_sent" => QuoteStatus::OfferSent,
            "accepted" => QuoteStatus::Accepted,
            "rejected" => QuoteStatus::Rejected,
            "expired" => QuoteStatus::Expired,
            "cancelled" => QuoteStatus::Cancelled,
            "done" => QuoteStatus::Done,
            "paid" => QuoteStatus::Paid,
            _ => QuoteStatus::Pending,
        };

        Quote {
            id: row.id,
            customer_id: row.customer_id,
            origin_address_id: row.origin_address_id,
            destination_address_id: row.destination_address_id,
            status,
            estimated_volume_m3: row.estimated_volume_m3,
            distance_km: row.distance_km,
            preferred_date: row.preferred_date,
            notes: row.notes,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_quote_row(status: &str) -> QuoteRow {
        let dummy_id = Uuid::nil();
        let now = chrono::DateTime::from_timestamp(0, 0).unwrap();
        QuoteRow {
            id: dummy_id,
            customer_id: dummy_id,
            origin_address_id: None,
            destination_address_id: None,
            status: status.to_string(),
            estimated_volume_m3: None,
            distance_km: None,
            preferred_date: None,
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn quote_row_status_mapping() {
        let cases = [
            ("pending", QuoteStatus::Pending),
            ("info_requested", QuoteStatus::InfoRequested),
            ("volume_estimated", QuoteStatus::VolumeEstimated),
            ("offer_generated", QuoteStatus::OfferGenerated),
            ("offer_sent", QuoteStatus::OfferSent),
            ("accepted", QuoteStatus::Accepted),
            ("rejected", QuoteStatus::Rejected),
            ("expired", QuoteStatus::Expired),
            ("cancelled", QuoteStatus::Cancelled),
            ("done", QuoteStatus::Done),
            ("paid", QuoteStatus::Paid),
            ("unknown_value", QuoteStatus::Pending),
        ];

        for (status_str, expected) in cases {
            let row = make_test_quote_row(status_str);
            let quote = Quote::from(row);
            assert_eq!(
                quote.status, expected,
                "status '{}' should map to {:?}",
                status_str, expected
            );
        }
    }
}
