use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Address {
    pub id: Uuid,
    pub street: String,
    pub city: String,
    pub postal_code: Option<String>,
    pub country: String,
    pub floor: Option<String>,
    pub needs_parking_ban: bool,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateAddress {
    #[validate(length(min = 1, message = "Straße darf nicht leer sein"))]
    pub street: String,
    #[validate(length(min = 1, message = "Stadt darf nicht leer sein"))]
    pub city: String,
    pub postal_code: Option<String>,
    #[serde(default = "default_country")]
    pub country: String,
    pub floor: Option<String>,
    #[serde(default)]
    pub needs_parking_ban: bool,
}

fn default_country() -> String {
    "Österreich".to_string()
}

/// Valid floor options matching the website form.
pub const FLOOR_OPTIONS: &[&str] = &[
    "Erdgeschoss",
    "Hochparterre",
    "1. Stock",
    "2. Stock",
    "3. Stock",
    "4. Stock",
    "5. Stock",
    "6. Stock",
    "Höher als 6. Stock",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    pub latitude: f64,
    pub longitude: f64,
}

/// Single leg between two consecutive addresses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteLeg {
    pub from_address: String,
    pub to_address: String,
    pub from_location: GeoLocation,
    pub to_location: GeoLocation,
    pub distance_km: f64,
    pub duration_minutes: u32,
}

/// Full multi-stop route calculation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    pub addresses: Vec<String>,
    pub legs: Vec<RouteLeg>,
    pub total_distance_km: f64,
    pub total_duration_minutes: u32,
    pub price_cents: i64,
    pub price_per_km_cents: i64,
}

/// Legacy single-pair result (kept for backward compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistanceResult {
    pub distance_km: f64,
    pub duration_minutes: Option<u32>,
    pub origin: GeoLocation,
    pub destination: GeoLocation,
}
