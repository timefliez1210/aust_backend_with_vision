use crate::{ApiError, AppState};
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use axum::{Json, extract::State};
use serde::Serialize;
use std::sync::Arc;

/// Response for `POST /api/v1/distance/calculate`.
#[derive(Serialize)]
pub struct DistanceResponse {
    pub total_distance_km: f64,
    pub total_duration_minutes: u32,
    pub price_cents: i64,
    pub legs: Vec<LegResponse>,
}

/// Per-leg summary included in [`DistanceResponse`].
#[derive(Serialize)]
pub struct LegResponse {
    pub from_address: String,
    pub to_address: String,
    pub distance_km: f64,
    pub duration_minutes: u32,
    /// GeoJSON-style coordinate pairs `[lng, lat]` for map polyline rendering.
    pub geometry: Vec<[f64; 2]>,
}

/// POST /api/v1/distance/calculate — public route distance endpoint.
///
/// **Caller**: Admin inquiry detail page (RouteMap polyline).
/// **Why**: Frontend needs geocoded route geometry to render the interactive map.
///
/// Accepts `{"addresses": ["Addr 1", "Addr 2", ...]}` and returns the full
/// driving route via OpenRouteService including per-leg geometry for map rendering.
///
/// # Errors
/// - 422 — fewer than 2 addresses
/// - 503 — ORS API unreachable or address not geocodable
pub async fn calculate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RouteRequest>,
) -> Result<Json<DistanceResponse>, ApiError> {
    if request.addresses.len() < 2 {
        return Err(ApiError::Validation(
            "Mindestens 2 Adressen erforderlich".into(),
        ));
    }

    let api_key = state.config.maps.api_key.clone();
    let calculator = RouteCalculator::new(api_key);

    let result = calculator
        .calculate(&request)
        .await
        .map_err(|e| ApiError::Internal(format!("Routenberechnung fehlgeschlagen: {e}")))?;

    let legs = result
        .legs
        .into_iter()
        .map(|l| LegResponse {
            from_address: l.from_address,
            to_address: l.to_address,
            distance_km: l.distance_km,
            duration_minutes: l.duration_minutes,
            geometry: l.geometry,
        })
        .collect();

    Ok(Json(DistanceResponse {
        total_distance_km: result.total_distance_km,
        total_duration_minutes: result.total_duration_minutes,
        price_cents: result.price_cents,
        legs,
    }))
}
