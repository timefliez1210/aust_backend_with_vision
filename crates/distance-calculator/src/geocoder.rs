use crate::DistanceError;
use aust_core::models::GeoLocation;
use reqwest::Client;
use serde::Deserialize;
use tracing::debug;

/// Converts free-text German address strings to WGS-84 coordinates using the
/// OpenRouteService Geocode Search API.
///
/// **Caller**: `RouteCalculator::calculate` calls `geocode()` once for each
/// address in the `RouteRequest` before routing.
/// **Why**: Separating geocoding from routing makes each step independently
/// testable and replaceable (e.g., swap ORS for Nominatim without touching routing).
pub struct Geocoder {
    client: Client,
    api_key: String,
}

impl Geocoder {
    /// Creates a new `Geocoder` with the given OpenRouteService API key.
    ///
    /// # Parameters
    /// - `api_key` — ORS API key passed as `?api_key=` query parameter.
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    /// Geocode a free-text address string to a WGS-84 `GeoLocation`.
    ///
    /// **Caller**: `RouteCalculator::calculate` calls this for every address in
    /// the route request.
    ///
    /// # Parameters
    /// - `address` — Free-text German address (e.g., `"Kaiserstr. 32, 31134 Hildesheim"`).
    ///
    /// # Returns
    /// The best matching coordinate pair from ORS, restricted to DE/AT.
    ///
    /// # Errors
    /// - `DistanceError::Network` — connection or TLS failure to ORS.
    /// - `DistanceError::Api` — ORS returned an unparseable response body.
    /// - `DistanceError::Geocoding` — no results found for the address.
    pub async fn geocode(&self, address: &str) -> Result<GeoLocation, DistanceError> {
        let url = format!(
            "https://api.openrouteservice.org/geocode/search?api_key={}&text={}&size=1&boundary.country=DE,AT",
            self.api_key,
            encode_uri_component(address),
        );

        debug!("Geocoding: {address}");

        let response: OrsGeocodeResponse = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await?
            .json()
            .await
            .map_err(|e| DistanceError::Api(format!("Geocode parse error: {e}")))?;

        if let Some(feature) = response.features.first() {
            // GeoJSON: coordinates are [longitude, latitude]
            let coords = &feature.geometry.coordinates;
            if coords.len() >= 2 {
                let label = feature
                    .properties
                    .label
                    .as_deref()
                    .unwrap_or("unknown");
                debug!("Geocoded '{address}' -> {label} ({}, {})", coords[1], coords[0]);

                return Ok(GeoLocation {
                    latitude: coords[1],
                    longitude: coords[0],
                });
            }
        }

        Err(DistanceError::Geocoding(format!(
            "Keine Ergebnisse für Adresse: {address}"
        )))
    }
}

/// GeoJSON FeatureCollection returned by the ORS Geocode Search endpoint.
#[derive(Debug, Deserialize)]
struct OrsGeocodeResponse {
    features: Vec<OrsFeature>,
}

/// A single geocoding result feature.
#[derive(Debug, Deserialize)]
struct OrsFeature {
    geometry: OrsGeometry,
    properties: OrsProperties,
}

/// GeoJSON geometry containing `[longitude, latitude]` coordinates.
#[derive(Debug, Deserialize)]
struct OrsGeometry {
    coordinates: Vec<f64>,
}

/// Human-readable label for a geocoding result (e.g., `"Kaiserstr 32, Hildesheim"`).
#[derive(Debug, Deserialize)]
struct OrsProperties {
    label: Option<String>,
}

/// Percent-encodes a string for use in a URL query parameter.
/// Encodes all characters except unreserved ones (`A-Z a-z 0-9 - _ . ~`).
fn encode_uri_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else {
                c.encode_utf8(&mut [0; 4])
                    .bytes()
                    .map(|b| format!("%{:02X}", b))
                    .collect()
            }
        })
        .collect()
}
