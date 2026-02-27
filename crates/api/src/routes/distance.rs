use axum::{extract::State, routing::post, Json, Router};
use std::sync::Arc;

use crate::{ApiError, AppState};
use aust_core::models::RouteResult;
use aust_distance_calculator::{RouteCalculator, RouteRequest};

/// Register the distance calculation route.
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Exposes the multi-stop route calculation endpoint backed by OpenRouteService.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/calculate", post(calculate_route))
}

/// `POST /api/v1/distance/calculate` — Calculate the total route distance for a list of addresses.
///
/// **Caller**: Axum router / admin dashboard distance calculator and any client needing
/// route data before offer generation.
/// **Why**: Calls `RouteCalculator` (OpenRouteService geocoding + directions) for an
/// ordered list of addresses and returns total distance in km. Requires at least 2 addresses.
///
/// # Parameters
/// - `state` — shared AppState (config for ORS API key)
/// - `request` — `RouteRequest` JSON body with `addresses` array (ordered waypoints as strings)
///
/// # Returns
/// `200 OK` with `RouteResult` JSON (total_distance_km, per-segment breakdown).
///
/// # Errors
/// - `400` if fewer than 2 addresses are provided
/// - `500` if ORS geocoding or routing fails
async fn calculate_route(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RouteRequest>,
) -> Result<Json<RouteResult>, ApiError> {
    if request.addresses.len() < 2 {
        return Err(ApiError::Validation(
            "Mindestens 2 Adressen erforderlich".into(),
        ));
    }

    let calculator = RouteCalculator::new(state.config.maps.api_key.clone());

    let result = calculator
        .calculate(&request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(result))
}
