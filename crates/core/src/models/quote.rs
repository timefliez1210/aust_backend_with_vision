use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle status of a moving quote request.
///
/// Transitions follow the pipeline:
/// `Pending` → `InfoRequested` (if data missing) → `VolumeEstimated` →
/// `OfferGenerated` → `OfferSent` → `Accepted` | `Rejected` | `Expired` →
/// `Done` → `Paid`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteStatus {
    /// Initial state: inquiry received, no offer generated yet.
    Pending,
    /// The email agent sent a follow-up asking for missing fields.
    InfoRequested,
    /// Volume estimation pipeline completed successfully.
    VolumeEstimated,
    /// Offer PDF generated and awaiting Telegram approval.
    OfferGenerated,
    /// Offer approved by Alex and emailed to the customer.
    OfferSent,
    /// Customer accepted the offer.
    Accepted,
    /// Customer or Alex rejected the offer.
    Rejected,
    /// Quote timed out without a response.
    Expired,
    /// Quote was cancelled (e.g., customer withdrew).
    Cancelled,
    /// Move completed successfully.
    Done,
    /// Invoice paid by the customer.
    Paid,
}

impl Default for QuoteStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl QuoteStatus {
    /// Returns the lowercase snake_case string stored in the `quotes.status` column.
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

/// A moving quote request — the central record linking customer, addresses,
/// volume estimation, and generated offers.
///
/// Created by the orchestrator when a complete or near-complete `MovingInquiry`
/// is received. All downstream records (volume estimations, offers, bookings)
/// reference this record via `quote_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// Customer who submitted the inquiry.
    pub customer_id: Uuid,
    /// Departure address (Auszug); `None` until the orchestrator creates it.
    pub origin_address_id: Option<Uuid>,
    /// Destination address (Einzug); `None` until the orchestrator creates it.
    pub destination_address_id: Option<Uuid>,
    /// Optional intermediate stop (Zwischenstopp) address.
    pub stop_address_id: Option<Uuid>,
    pub status: QuoteStatus,
    /// Agreed total moving volume in cubic metres; set after volume estimation
    /// completes or when the inventory form is used.
    pub estimated_volume_m3: Option<f64>,
    /// Total driving distance in kilometres for the full route (depot→origin→
    /// [stop]→destination); `None` until the distance calculator runs.
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>,
    /// Free-text notes including comma-separated service flags parsed by
    /// `build_line_items()` (e.g., `"einpackservice,montage"`).
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new quote record.
///
/// **Caller**: `orchestrator.rs` constructs this after upserting the customer
/// and creating the address records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateQuote {
    pub customer_id: Uuid,
    pub origin_address_id: Option<Uuid>,
    pub destination_address_id: Option<Uuid>,
    pub preferred_date: Option<DateTime<Utc>>,
    /// Free-text notes, including comma-separated Zusatzleistungen service flags.
    pub notes: Option<String>,
}

/// Partial update applied to an existing quote.
///
/// **Caller**: Admin `PATCH /api/v1/quotes/{id}` and the orchestrator when new
/// information arrives (e.g., distance calculated, volume confirmed).
/// **Why**: All fields are `Option` so callers send only the fields they want
/// to change; `None` fields are excluded from the SQL UPDATE.
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
