use crate::DistanceError;
use aust_core::models::GeoLocation;
use reqwest::Client;
use serde::Deserialize;
use tracing::debug;

pub struct Geocoder {
    client: Client,
    api_key: String,
}

impl Geocoder {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    /// Geocode an address string to lat/lng using OpenRouteService.
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

#[derive(Debug, Deserialize)]
struct OrsGeocodeResponse {
    features: Vec<OrsFeature>,
}

#[derive(Debug, Deserialize)]
struct OrsFeature {
    geometry: OrsGeometry,
    properties: OrsProperties,
}

#[derive(Debug, Deserialize)]
struct OrsGeometry {
    coordinates: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct OrsProperties {
    label: Option<String>,
}

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
