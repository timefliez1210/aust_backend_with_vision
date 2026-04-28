use crate::DistanceError;
use aust_core::models::{DistanceResult, GeoLocation};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

pub struct DistanceRouter {
    client: Client,
    api_key: String,
}

impl DistanceRouter {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client with timeout"),
            api_key,
        }
    }

    /// Calculate driving distance and duration between two points
    /// using OpenRouteService Directions API.
    pub async fn calculate_distance(
        &self,
        origin: &GeoLocation,
        destination: &GeoLocation,
    ) -> Result<DistanceResult, DistanceError> {
        // ORS directions expects: start=lon,lat&end=lon,lat
        let url = format!(
            "https://api.openrouteservice.org/v2/directions/driving-car?api_key={}&start={},{}&end={},{}",
            self.api_key,
            origin.longitude, origin.latitude,
            destination.longitude, destination.latitude,
        );

        debug!(
            "Calculating distance: ({}, {}) -> ({}, {})",
            origin.latitude, origin.longitude, destination.latitude, destination.longitude
        );

        let raw_response = self
            .client
            .get(&url)
            .header("Accept", "application/geo+json;charset=UTF-8")
            .send()
            .await?;

        let status = raw_response.status();
        if !status.is_success() {
            let text = raw_response.text().await.unwrap_or_default();
            return Err(DistanceError::Api(format!(
                "ORS API error {status}: {text}"
            )));
        }

        let response: OrsDirectionsResponse = raw_response
            .json()
            .await
            .map_err(|e| DistanceError::Api(format!("Directions parse error: {e}")))?;

        if let Some(feature) = response.features.first() {
            let summary = &feature.properties.summary;
            let distance_km = summary.distance / 1000.0;
            let duration_minutes = (summary.duration / 60.0) as u32;

            let geometry = feature
                .geometry
                .coordinates
                .iter()
                .filter_map(|c| {
                    if c.len() >= 2 {
                        Some([c[0], c[1]])
                    } else {
                        None
                    }
                })
                .collect();

            debug!("Distance: {distance_km:.1} km, Duration: {duration_minutes} min");

            return Ok(DistanceResult {
                distance_km,
                duration_minutes: Some(duration_minutes),
                origin: origin.clone(),
                destination: destination.clone(),
                geometry,
            });
        }

        Err(DistanceError::Routing(
            "Konnte Entfernung nicht berechnen".into(),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct OrsDirectionsResponse {
    features: Vec<OrsDirectionsFeature>,
}

#[derive(Debug, Deserialize)]
struct OrsDirectionsFeature {
    geometry: OrsDirectionsGeometry,
    properties: OrsDirectionsProperties,
}

#[derive(Debug, Deserialize)]
struct OrsDirectionsGeometry {
    coordinates: Vec<Vec<f64>>,
}

/// ORS returns segments + summary. We only need summary.
/// Use `deny_unknown_fields` is NOT set — extra fields are ignored.
#[derive(Debug, Deserialize)]
struct OrsDirectionsProperties {
    summary: OrsDirectionsSummary,
    // segments, way_points etc. are ignored
}

#[derive(Debug, Deserialize)]
struct OrsDirectionsSummary {
    distance: f64, // meters
    duration: f64, // seconds
}
