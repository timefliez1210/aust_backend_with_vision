use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteStatus {
    Pending,
    InfoRequested,
    VolumeEstimated,
    OfferGenerated,
    OfferSent,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
    Done,
    Paid,
}

impl Default for QuoteStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl QuoteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InfoRequested => "info_requested",
            Self::VolumeEstimated => "volume_estimated",
            Self::OfferGenerated => "offer_generated",
            Self::OfferSent => "offer_sent",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Done => "done",
            Self::Paid => "paid",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub stop_address_id: Option<Uuid>,
    pub status: QuoteStatus,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateQuote {
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub preferred_date: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateQuote {
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub status: Option<QuoteStatus>,
    pub estimated_volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_status_as_str_roundtrip() {
        let variants = [
            (QuoteStatus::Pending, "pending"),
            (QuoteStatus::InfoRequested, "info_requested"),
            (QuoteStatus::VolumeEstimated, "volume_estimated"),
            (QuoteStatus::OfferGenerated, "offer_generated"),
            (QuoteStatus::OfferSent, "offer_sent"),
            (QuoteStatus::Accepted, "accepted"),
            (QuoteStatus::Rejected, "rejected"),
            (QuoteStatus::Expired, "expired"),
            (QuoteStatus::Cancelled, "cancelled"),
            (QuoteStatus::Done, "done"),
            (QuoteStatus::Paid, "paid"),
        ];
        for (status, expected_str) in variants {
            assert_eq!(status.as_str(), expected_str, "as_str for {:?}", status);
            // Roundtrip via serde
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: QuoteStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, status, "roundtrip for {:?}", status);
        }
    }

    #[test]
    fn quote_status_default() {
        assert_eq!(QuoteStatus::default(), QuoteStatus::Pending);
    }

    #[test]
    fn quote_status_serde_snake_case() {
        let status: QuoteStatus = serde_json::from_str("\"offer_generated\"").unwrap();
        assert_eq!(status, QuoteStatus::OfferGenerated);
    }
}
