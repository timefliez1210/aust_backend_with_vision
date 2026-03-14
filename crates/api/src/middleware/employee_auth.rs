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

/// Claims injected into request extensions after employee session auth.
#[derive(Debug, Clone)]
pub struct EmployeeClaims {
    pub employee_id: Uuid,
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

/// Middleware that validates employee session tokens.
///
/// **Caller**: `lib.rs` — applied as `route_layer` to the employee protected router.
/// **Why**: Employees authenticate via OTP magic link, not JWT. Sessions are stored
///          in `employee_sessions` and expire after 30 days.
///
/// Extracts `Authorization: Bearer <token>`, looks up `employee_sessions` joined to
/// `employees`, validates expiry, and injects `EmployeeClaims` into request extensions.
pub async fn require_employee_auth(
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
        SELECT es.employee_id, e.email
        FROM employee_sessions es
        JOIN employees e ON es.employee_id = e.id
        WHERE es.token = $1 AND es.expires_at > $2
        "#,
    )
    .bind(token)
    .bind(now)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("Employee session lookup failed: {e}");
        unauthorized("Authentifizierung fehlgeschlagen")
    })?;

    let (employee_id, email) =
        row.ok_or_else(|| unauthorized("Ungültiges oder abgelaufenes Token"))?;

    request
        .extensions_mut()
        .insert(EmployeeClaims { employee_id, email });

    Ok(next.run(request).await)
}
