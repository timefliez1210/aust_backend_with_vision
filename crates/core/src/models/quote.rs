use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::inquiry::InquiryStatus;
use super::snapshots::Services;

/// Re-export InquiryStatus as QuoteStatus for backward compatibility.
///
/// **Why**: Existing code throughout the codebase references `QuoteStatus`.
/// This alias keeps that code compiling while the migration to `InquiryStatus`
/// proceeds incrementally.
pub type QuoteStatus = InquiryStatus;

/// A moving quote request -- the central record linking customer, addresses,
/// volume estimation, and generated offers.
///
/// Created by the orchestrator when a complete or near-complete `MovingInquiry`
/// is received. All downstream records (volume estimations, offers, bookings)
/// reference this record via `inquiry_id`.
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
    pub status: InquiryStatus,
    /// Agreed total moving volume in cubic metres; set after volume estimation
    /// completes or when the inventory form is used.
    pub estimated_volume_m3: Option<f64>,
    /// Total driving distance in kilometres for the full route (depot->origin->
    /// [stop]->destination); `None` until the distance calculator runs.
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>,
    /// Free-text customer message (e.g., `"Bitte vorsichtig mit dem Klavier"`).
    pub notes: Option<String>,
    /// How this inquiry entered the system (email_form, contact_form, direct_email, photo_api, etc.).
    pub source: Option<String>,
    /// Structured service flags; replaces comma-separated notes parsing in the new schema.
    pub services: Option<Services>,
    /// Timestamp when the offer was emailed to the customer.
    pub offer_sent_at: Option<DateTime<Utc>>,
    /// Timestamp when the customer accepted the offer.
    pub accepted_at: Option<DateTime<Utc>>,
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
    pub status: Option<InquiryStatus>,
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
            (InquiryStatus::Pending, "pending"),
            (InquiryStatus::InfoRequested, "info_requested"),
            (InquiryStatus::Estimating, "estimating"),
            (InquiryStatus::Estimated, "estimated"),
            (InquiryStatus::OfferReady, "offer_ready"),
            (InquiryStatus::OfferSent, "offer_sent"),
            (InquiryStatus::Accepted, "accepted"),
            (InquiryStatus::Rejected, "rejected"),
            (InquiryStatus::Expired, "expired"),
            (InquiryStatus::Cancelled, "cancelled"),
            (InquiryStatus::Scheduled, "scheduled"),
            (InquiryStatus::Completed, "completed"),
            (InquiryStatus::Invoiced, "invoiced"),
            (InquiryStatus::Paid, "paid"),
        ];
        for (status, expected_str) in variants {
            assert_eq!(status.as_str(), expected_str, "as_str for {:?}", status);
            // Roundtrip via serde
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: InquiryStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, status, "roundtrip for {:?}", status);
        }
    }

    #[test]
    fn quote_status_default() {
        assert_eq!(InquiryStatus::default(), InquiryStatus::Pending);
    }

    #[test]
    fn quote_status_serde_snake_case() {
        let status: InquiryStatus = serde_json::from_str("\"offer_ready\"").unwrap();
        assert_eq!(status, InquiryStatus::OfferReady);
    }
}
