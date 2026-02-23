use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Serialize;
use std::sync::Arc;

use aust_core::models::TokenClaims;

use crate::AppState;

#[derive(Serialize)]
struct AuthError {
    error: String,
    message: String,
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(AuthError {
            error: "unauthorized".to_string(),
            message: msg.to_string(),
        }),
    )
        .into_response()
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| unauthorized("Kein Authorization-Header"))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| unauthorized("Ungültiges Token-Format"))?;

    let secret = &state.config.auth.jwt_secret;

    let token_data = decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| {
        tracing::debug!("JWT validation failed: {e}");
        unauthorized("Ungültiges oder abgelaufenes Token")
    })?;

    request.extensions_mut().insert(token_data.claims);
    Ok(next.run(request).await)
}
