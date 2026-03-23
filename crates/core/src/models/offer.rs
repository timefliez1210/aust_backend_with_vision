use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state of a generated price offer.
///
/// Transitions: `Draft` → `Sent` → `Viewed` → `Accepted` | `Rejected` | `Expired`.
/// Alex can also reject an offer before sending via the Telegram workflow, which
/// sets the status directly to `Rejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    /// Generated but not yet reviewed by Alex in Telegram.
    Draft,
    /// Approved by Alex and emailed to the customer.
    Sent,
    /// Customer opened the email (tracked via read receipt if available).
    Viewed,
    /// Customer accepted the offer.
    Accepted,
    /// Customer or Alex rejected the offer.
    Rejected,
    /// Offer validity period has passed without a response.
    Expired,
}

impl Default for OfferStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl OfferStatus {
    /// Returns the lowercase string stored in the database `offers.status` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::Viewed => "viewed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

impl std::fmt::Display for OfferStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OfferStatus {
    type Err = String;

    /// Parse the lowercase database string back into an `OfferStatus` variant.
    ///
    /// **Caller**: DB row-to-model converters (`offer_builder`, `offer_repo`).
    /// **Why**: Replaces duplicated manual `match row.status.as_str()` blocks.
    ///
    /// # Errors
    /// Returns `Err(String)` for any unrecognised status string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "sent" => Ok(Self::Sent),
            "viewed" => Ok(Self::Viewed),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            other => Err(format!("unknown OfferStatus: {other}")),
        }
    }
}

/// A generated price offer for a moving job.
///
/// Created by `offer-generator` after the orchestrator has assembled a quote.
/// The corresponding PDF is stored in object storage under `pdf_storage_key`.
/// The Telegram approval workflow reads and updates this record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// The quote this offer is based on.
    pub inquiry_id: Uuid,
    /// Total offer price in euro **cents** (e.g., `35000` = €350.00 brutto).
    /// Alex always thinks in brutto; netto is `price_cents / 1.19`.
    pub price_cents: i64,
    /// ISO 4217 currency code (always `"EUR"`).
    pub currency: String,
    /// Date after which the offer is considered expired.
    pub valid_until: Option<NaiveDate>,
    /// S3 / local storage key for the generated PDF file.
    pub pdf_storage_key: Option<String>,
    pub status: OfferStatus,
    pub created_at: DateTime<Utc>,
    /// Timestamp when Alex approved and the offer email was dispatched.
    pub sent_at: Option<DateTime<Utc>>,
    /// Sequential human-readable offer number (e.g., `"2025-0042"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer_number: Option<String>,
    /// Number of moving helpers (Umzugshelfer) in the offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persons: Option<i32>,
    /// Estimated number of hours for the move.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hours_estimated: Option<f64>,
    /// Hourly rate per helper in euro cents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_per_hour_cents: Option<i64>,
    /// Serialised line items as a JSON array; used to re-render the XLSX without
    /// running the pricing engine again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_items_json: Option<serde_json::Value>,
}

/// Input for inserting a new offer record into the database.
///
/// **Caller**: `offer-generator` creates this after generating the PDF and
/// uploading it to storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOffer {
    pub inquiry_id: Uuid,
    /// Total price in euro cents.
    pub price_cents: i64,
    /// Defaults to `"EUR"` when `None`.
    pub currency: Option<String>,
    pub valid_until: Option<NaiveDate>,
}

/// Inputs fed into the pricing engine to calculate offer line items.
///
/// **Caller**: The `offers/generate` route handler constructs this from the
/// database quote and its linked addresses before calling `PricingEngine::calculate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingInput {
    /// Total moving volume in cubic metres; drives helper count and hours.
    pub volume_m3: f64,
    /// Route distance in kilometres (depot → origin → [stop] → destination).
    pub distance_km: f64,
    /// Requested moving date; used to apply date-based surcharges (e.g., Saturday +€50).
    pub preferred_date: Option<DateTime<Utc>>,
    /// Floor number at the departure address (0 = ground floor).
    /// Used to calculate stair-carrying time and floor surcharge.
    pub floor_origin: Option<u32>,
    /// Floor number at the destination address.
    pub floor_destination: Option<u32>,
    /// Whether the departure building has an elevator (reduces floor surcharge).
    pub has_elevator_origin: Option<bool>,
    /// Whether the destination building has an elevator.
    pub has_elevator_destination: Option<bool>,
    /// Floor number at an intermediate stop (Zwischenstopp), if present.
    pub floor_stop: Option<u32>,
    /// Whether the intermediate stop building has an elevator.
    pub has_elevator_stop: Option<bool>,
}

/// Output of the pricing engine with a breakdown of cost components.
///
/// **Caller**: The offer generator reads this to populate the XLSX template
/// line items and compute `total_price_cents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingResult {
    /// Total offer price in euro cents (sum of all breakdown components).
    pub total_price_cents: i64,
    pub breakdown: PricingBreakdown,
    /// Recommended number of moving helpers for this job size.
    pub estimated_helpers: u32,
    /// Estimated hours required based on volume and floor logistics.
    pub estimated_hours: f64,
}

/// Itemised cost components produced by the pricing engine.
///
/// **Why**: Stored separately so that the Telegram edit workflow can display
/// individual components and allow Alex to override specific ones (e.g., raise
/// the labor rate without changing the distance charge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBreakdown {
    /// Labour cost in euro cents: `persons × hours × rate_per_hour`.
    pub base_labor_cents: i64,
    /// Distance-based Anfahrt surcharge in euro cents.
    pub distance_cents: i64,
    /// Additional charge for carrying items up/down stairs without an elevator.
    pub floor_surcharge_cents: i64,
    /// Date-based adjustment in euro cents (e.g., Saturday surcharge).
    pub date_adjustment_cents: i64,
}
