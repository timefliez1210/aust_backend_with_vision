use axum::{routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", post(login))
        .route("/refresh", post(refresh_token))
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

async fn login(Json(request): Json<LoginRequest>) -> Result<Json<TokenResponse>, ApiError> {
    // TODO: Implement actual authentication
    if request.email.is_empty() || request.password.is_empty() {
        return Err(ApiError::Validation(
            "Email und Passwort sind erforderlich".into(),
        ));
    }

    // Placeholder response
    Ok(Json(TokenResponse {
        access_token: "placeholder_token".to_string(),
        token_type: "Bearer".to_string(),
        expires_in: 3600,
    }))
}

async fn refresh_token() -> Result<Json<TokenResponse>, ApiError> {
    // TODO: Implement token refresh
    Ok(Json(TokenResponse {
        access_token: "refreshed_token".to_string(),
        token_type: "Bearer".to_string(),
        expires_in: 3600,
    }))
}
