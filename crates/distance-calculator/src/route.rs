use crate::error::DistanceError;
use crate::geocoder::Geocoder;
use crate::router::DistanceRouter;
use aust_core::models::{RouteLeg, RouteResult};
use serde::Deserialize;
use tracing::info;

const PRICE_PER_KM_CENTS: i64 = 100; // €1.00 per km

pub struct RouteCalculator {
    geocoder: Geocoder,
    router: DistanceRouter,
}

/// Input for multi-stop route calculation.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteRequest {
    /// Ordered list of addresses (minimum 2).
    pub addresses: Vec<String>,
}

impl RouteCalculator {
    pub fn new(api_key: String) -> Self {
        Self {
            geocoder: Geocoder::new(api_key.clone()),
            router: DistanceRouter::new(api_key),
        }
    }

    /// Calculate the full route through all addresses in order.
    /// Geocodes each address, then calculates distance for each consecutive pair.
    pub async fn calculate(&self, request: &RouteRequest) -> Result<RouteResult, DistanceError> {
        if request.addresses.len() < 2 {
            return Err(DistanceError::Routing(
                "Mindestens 2 Adressen erforderlich".into(),
            ));
        }

        info!(
            "Calculating route through {} addresses",
            request.addresses.len()
        );

        // Geocode all addresses
        let mut locations = Vec::with_capacity(request.addresses.len());
        for addr in &request.addresses {
            let loc = self.geocoder.geocode(addr).await?;
            locations.push(loc);
        }

        // Calculate distance for each consecutive pair
        let mut legs = Vec::with_capacity(request.addresses.len() - 1);
        let mut total_distance_km = 0.0;
        let mut total_duration_minutes = 0u32;

        for i in 0..locations.len() - 1 {
            let result = self
                .router
                .calculate_distance(&locations[i], &locations[i + 1])
                .await?;

            let duration = result.duration_minutes.unwrap_or(0);
            total_distance_km += result.distance_km;
            total_duration_minutes += duration;

            legs.push(RouteLeg {
                from_address: request.addresses[i].clone(),
                to_address: request.addresses[i + 1].clone(),
                from_location: locations[i].clone(),
                to_location: locations[i + 1].clone(),
                distance_km: result.distance_km,
                duration_minutes: duration,
                geometry: result.geometry,
            });
        }

        let price_cents = (total_distance_km.ceil() as i64) * PRICE_PER_KM_CENTS;

        info!(
            "Route calculated: {:.1} km, {} min, {} legs, €{:.2}",
            total_distance_km,
            total_duration_minutes,
            legs.len(),
            price_cents as f64 / 100.0
        );

        Ok(RouteResult {
            addresses: request.addresses.clone(),
            legs,
            total_distance_km,
            total_duration_minutes,
            price_cents,
            price_per_km_cents: PRICE_PER_KM_CENTS,
        })
    }
}
