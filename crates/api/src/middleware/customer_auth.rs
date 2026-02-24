use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// Claims injected into request extensions after customer auth.
#[derive(Debug, Clone)]
pub struct CustomerClaims {
    pub customer_id: Uuid,
    pub email: String,
}

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

/// Middleware that validates customer session tokens.
/// Extracts `Authorization: Bearer <token>`, looks up `customer_sessions`,
/// and injects `CustomerClaims` into request extensions.
pub async fn require_customer_auth(
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

    let now = Utc::now();

    let row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT cs.customer_id, c.email
        FROM customer_sessions cs
        JOIN customers c ON cs.customer_id = c.id
        WHERE cs.token = $1 AND cs.expires_at > $2
        "#,
    )
    .bind(token)
    .bind(now)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("Customer session lookup failed: {e}");
        unauthorized("Authentifizierung fehlgeschlagen")
    })?;

    let (customer_id, email) =
        row.ok_or_else(|| unauthorized("Ungültiges oder abgelaufenes Token"))?;

    request
        .extensions_mut()
        .insert(CustomerClaims { customer_id, email });

    Ok(next.run(request).await)
}
