use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::inquiry::InquiryStatus;

/// Structured service flags for an inquiry.
/// Stored as JSONB in the inquiries table, replacing comma-separated notes parsing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Services {
    #[serde(default)]
    pub packing: bool,
    #[serde(default)]
    pub assembly: bool,
    #[serde(default)]
    pub disassembly: bool,
    #[serde(default)]
    pub storage: bool,
    #[serde(default)]
    pub disposal: bool,
    #[serde(default)]
    pub parking_ban_origin: bool,
    #[serde(default)]
    pub parking_ban_destination: bool,
}

/// Canonical detail response for a single inquiry.
/// Used by all detail endpoints (admin, customer, internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InquiryResponse {
    pub id: Uuid,
    pub status: InquiryStatus,
    pub source: String,
    pub services: Services,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_m3: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_date: Option<NaiveDate>,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer_sent_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<DateTime<Utc>>,

    // Related entities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer: Option<CustomerSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_address: Option<AddressSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_address: Option<AddressSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_address: Option<AddressSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimation: Option<EstimationSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<OfferSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub employees: Vec<EmployeeAssignmentSnapshot>,
}

/// Summary item for list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InquiryListItem {
    pub id: Uuid,
    pub customer_name: Option<String>,
    pub customer_email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salutation: Option<String>,
    pub origin_city: Option<String>,
    pub destination_city: Option<String>,
    pub volume_m3: Option<f64>,
    pub distance_km: Option<f64>,
    pub status: InquiryStatus,
    pub has_offer: bool,
    pub offer_status: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerSnapshot {
    pub id: Uuid,
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: String,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressSnapshot {
    pub id: Uuid,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevator: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_parking_ban: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimationSnapshot {
    pub id: Uuid,
    pub method: String,
    pub status: String,
    pub total_volume_m3: Option<f64>,
    pub confidence_score: Option<f64>,
    pub item_count: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_images: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_video: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemSnapshot {
    pub name: String,
    pub volume_m3: f64,
    pub quantity: i64,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop_s3_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox_image_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_in_images: Option<Vec<i32>>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub is_moveable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub packs_into_boxes: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferSnapshot {
    pub id: Uuid,
    pub offer_number: Option<String>,
    pub status: String,
    pub persons: i32,
    pub hours: f64,
    pub rate_cents: i64,
    pub total_netto_cents: i64,
    pub total_brutto_cents: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub line_items: Vec<LineItemSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItemSnapshot {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
    pub quantity: f64,
    pub unit_price_cents: i64,
    pub total_cents: i64,
    pub is_labor: bool,
    #[serde(default)]
    pub is_flat_total: bool,
}

/// Snapshot of an employee assignment on an inquiry.
///
/// **Caller**: `build_inquiry_response()` in `inquiry_builder.rs`
/// **Why**: Embeds assigned employee info in the canonical inquiry detail response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeAssignmentSnapshot {
    pub employee_id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub planned_hours: f64,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_services_default_all_false() {
        let s = Services::default();
        assert!(!s.packing);
        assert!(!s.assembly);
        assert!(!s.disassembly);
        assert!(!s.storage);
        assert!(!s.disposal);
        assert!(!s.parking_ban_origin);
        assert!(!s.parking_ban_destination);
    }

    #[test]
    fn test_services_serde_roundtrip() {
        let s = Services {
            packing: true,
            assembly: false,
            disassembly: true,
            storage: false,
            disposal: false,
            parking_ban_origin: true,
            parking_ban_destination: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        let deserialized: Services = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, s);
    }

    #[test]
    fn test_services_deserialize_missing_fields_default_false() {
        let json = r#"{"packing": true}"#;
        let s: Services = serde_json::from_str(json).unwrap();
        assert!(s.packing);
        assert!(!s.assembly);
        assert!(!s.disassembly);
        assert!(!s.storage);
        assert!(!s.disposal);
        assert!(!s.parking_ban_origin);
        assert!(!s.parking_ban_destination);
    }

    #[test]
    fn test_services_deserialize_empty_object() {
        let json = "{}";
        let s: Services = serde_json::from_str(json).unwrap();
        assert_eq!(s, Services::default());
    }
}
