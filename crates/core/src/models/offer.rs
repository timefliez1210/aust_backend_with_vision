use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Draft,
    Sent,
    Viewed,
    Accepted,
    Rejected,
    Expired,
}

impl Default for OfferStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl OfferStatus {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub id: Uuid,
    pub quote_id: Uuid,
    pub price_cents: i64,
    pub currency: String,
    pub valid_until: Option<NaiveDate>,
    pub pdf_storage_key: Option<String>,
    pub status: OfferStatus,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOffer {
    pub quote_id: Uuid,
    pub price_cents: i64,
    pub currency: Option<String>,
    pub valid_until: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingInput {
    pub volume_m3: f64,
    pub distance_km: f64,
    pub preferred_date: Option<DateTime<Utc>>,
    pub floor_origin: Option<u32>,
    pub floor_destination: Option<u32>,
    pub has_elevator_origin: Option<bool>,
    pub has_elevator_destination: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingResult {
    pub total_price_cents: i64,
    pub breakdown: PricingBreakdown,
    pub estimated_helpers: u32,
    pub estimated_hours: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingBreakdown {
    pub base_labor_cents: i64,
    pub distance_cents: i64,
    pub floor_surcharge_cents: i64,
    pub date_adjustment_cents: i64,
}
