use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// A postal address for a moving job (origin, destination, or intermediate stop).
///
/// Addresses are stored normalised in the `addresses` table and referenced by
/// `quotes` through `origin_address_id`, `destination_address_id`, and
/// `stop_address_id`. Geocoordinates are populated by the distance calculator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Address {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// Street name and house number (e.g., `"Kaiserstr. 32"`).
    pub street: String,
    pub city: String,
    /// Postal code (PLZ), optional for free-text addresses parsed from email.
    pub postal_code: Option<String>,
    /// Country name (default: `"Österreich"`).
    pub country: String,
    /// Floor number or description (e.g., `"2. Stock"`, `"Erdgeschoss"`).
    /// Used by the pricing engine to calculate floor surcharges.
    pub floor: Option<String>,
    /// Whether a Halteverbotszone (temporary parking ban) needs to be arranged
    /// at this address. Adds a line item to the offer.
    pub needs_parking_ban: bool,
    /// WGS-84 latitude; populated after geocoding via OpenRouteService.
    pub latitude: Option<f64>,
    /// WGS-84 longitude; populated after geocoding via OpenRouteService.
    pub longitude: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Input for creating a new address record.
///
/// **Caller**: `orchestrator.rs` calls the address repository with this struct
/// when building a quote from a `MovingInquiry`.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateAddress {
    #[validate(length(min = 1, message = "Straße darf nicht leer sein"))]
    pub street: String,
    #[validate(length(min = 1, message = "Stadt darf nicht leer sein"))]
    pub city: String,
    pub postal_code: Option<String>,
    /// Defaults to `"Österreich"` when not supplied.
    #[serde(default = "default_country")]
    pub country: String,
    pub floor: Option<String>,
    #[serde(default)]
    pub needs_parking_ban: bool,
}

fn default_country() -> String {
    "Österreich".to_string()
}

/// WGS-84 coordinate pair returned by the geocoder.
///
/// **Caller**: `RouteCalculator` and `DistanceRouter` pass these between
/// the geocoding and routing steps of the distance calculation pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    pub latitude: f64,
    pub longitude: f64,
}

/// Single driving leg between two consecutive addresses in a multi-stop route.
///
/// **Why**: Multi-stop moves (with an intermediate Zwischenstopp) are broken
/// into legs so each segment's distance and duration can be shown separately in
/// the API response and used for per-segment pricing if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteLeg {
    pub from_address: String,
    pub to_address: String,
    pub from_location: GeoLocation,
    pub to_location: GeoLocation,
    /// Driving distance for this leg in kilometres.
    pub distance_km: f64,
    /// Estimated driving time for this leg in minutes.
    pub duration_minutes: u32,
    /// GeoJSON LineString coordinates `[[lng, lat], ...]` for the driving route.
    /// Omitted from serialisation when empty to keep API responses compact.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub geometry: Vec<[f64; 2]>,
}

/// Full multi-stop route calculation result returned by `RouteCalculator::calculate`.
///
/// **Caller**: The `POST /api/v1/distance/calculate` route handler returns this
/// as JSON. The orchestrator reads `total_distance_km` for offer pricing.
///
/// # Math
/// `price_cents = ceil(total_distance_km) × price_per_km_cents`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    /// Ordered list of input addresses (same order as the request).
    pub addresses: Vec<String>,
    /// One entry per consecutive address pair.
    pub legs: Vec<RouteLeg>,
    /// Sum of all leg distances in kilometres.
    pub total_distance_km: f64,
    /// Sum of all leg durations in minutes.
    pub total_duration_minutes: u32,
    /// Total distance price in euro cents (ceiled km × rate).
    pub price_cents: i64,
    /// Rate used for calculation, in euro cents per km.
    pub price_per_km_cents: i64,
}

/// Legacy single-pair distance result, kept for backward compatibility.
///
/// **Why**: Earlier versions of the API calculated only origin→destination.
/// Existing callers that consume this shape are supported without a breaking change.
/// New code should prefer `RouteResult` from `RouteCalculator`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistanceResult {
    /// Driving distance in kilometres.
    pub distance_km: f64,
    /// Estimated driving duration in minutes; `None` if the router did not return it.
    pub duration_minutes: Option<u32>,
    pub origin: GeoLocation,
    pub destination: GeoLocation,
    /// GeoJSON LineString coordinates `[[lng, lat], ...]` for the driving route.
    /// Omitted from serialisation when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub geometry: Vec<[f64; 2]>,
}
