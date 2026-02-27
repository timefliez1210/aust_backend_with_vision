use crate::error::DistanceError;
use crate::geocoder::Geocoder;
use crate::router::DistanceRouter;
use aust_core::models::{RouteLeg, RouteResult};
use serde::Deserialize;
use tracing::info;

/// Price rate charged per driven kilometre, in euro cents.
/// €1.00 per km → 100 cents. Change here to adjust the distance pricing.
const PRICE_PER_KM_CENTS: i64 = 100; // €1.00 per km

/// High-level orchestrator for multi-stop route calculation.
///
/// Chains `Geocoder` (address → coordinates) and `DistanceRouter` (coordinates
/// → driving km + minutes) for an ordered list of N addresses, then totals the
/// legs and computes a price.
///
/// **Caller**: Instantiated once at startup in `main.rs` and shared via
/// `Arc<RouteCalculator>` between the distance API route handler and the
/// orchestrator.
pub struct RouteCalculator {
    geocoder: Geocoder,
    router: DistanceRouter,
}

/// Input for a multi-stop route calculation.
///
/// **Caller**: `POST /api/v1/distance/calculate` deserialises the request body
/// into this struct. The orchestrator also constructs it directly when
/// calculating the move distance after address creation.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteRequest {
    /// Ordered list of addresses (minimum 2).
    /// Route is calculated sequentially: `addresses[0] → addresses[1] → … → addresses[N-1]`.
    pub addresses: Vec<String>,
}

impl RouteCalculator {
    /// Creates a new `RouteCalculator` with a shared OpenRouteService API key.
    ///
    /// # Parameters
    /// - `api_key` — ORS API key used for both geocoding and routing requests.
    pub fn new(api_key: String) -> Self {
        Self {
            geocoder: Geocoder::new(api_key.clone()),
            router: DistanceRouter::new(api_key),
        }
    }

    /// Calculate the full driving route through all addresses in order.
    ///
    /// **Caller**: Distance API route handler and orchestrator.
    /// **Why**: Moving jobs often have an intermediate Zwischenstopp, so the
    /// route is N stops, not just origin → destination.
    ///
    /// # Parameters
    /// - `request` — Ordered list of at least 2 free-text address strings.
    ///
    /// # Returns
    /// A `RouteResult` with per-leg distances/durations and aggregated totals.
    ///
    /// # Errors
    /// - `DistanceError::Routing` — fewer than 2 addresses supplied.
    /// - `DistanceError::Geocoding` — any address could not be resolved.
    /// - `DistanceError::Api` / `DistanceError::Network` — ORS request failures.
    ///
    /// # Math
    /// `price_cents = ceil(total_distance_km) × PRICE_PER_KM_CENTS`
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
