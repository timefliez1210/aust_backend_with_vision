use axum::{extract::State, routing::post, Json, Router};
use std::sync::Arc;

use crate::{ApiError, AppState};
use aust_core::models::RouteResult;
use aust_distance_calculator::{RouteCalculator, RouteRequest};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/calculate", post(calculate_route))
}

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
