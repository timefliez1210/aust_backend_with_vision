//! Public flash-contact endpoint — ultra-quick callback form.
//!
//! POST /api/v1/flash-contact
//! Body: { name, phone, time_preference }
//!
//! No authentication required. Alex receives an immediate Telegram ping.

use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

use crate::ApiError;
use crate::AppState;
use aust_flash_contact::{format_immediate_message, insert, CreateFlashContact, TimePreference};

#[derive(Deserialize)]
pub struct FlashContactRequest {
    pub name: String,
    pub phone: String,
    pub time_preference: TimePreference,
}

pub fn router() -> Router<Arc<AppState>> {
    // Public + unauthenticated, and every call fires an immediate Telegram ping to Alex.
    // Rate-limit per client IP (10 req/60s) so it can't be used to spam him. Uses its own
    // limiter instance (not the shared auth bucket) so the two limits stay independent.
    let limiter = Arc::new(crate::middleware::RateLimiter::new(
        10,
        std::time::Duration::from_secs(60),
    ));
    Router::new()
        .route("/flash-contact", post(create_flash_contact))
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let limiter = limiter.clone();
                async move { crate::middleware::apply_rate_limit(limiter, req, next).await }
            },
        ))
}

async fn create_flash_contact(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FlashContactRequest>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    // Basic validation — don't store completely empty strings.
    let name = req.name.trim().to_string();
    let phone = req.phone.trim().to_string();

    if name.is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    if phone.is_empty() {
        return Err(ApiError::BadRequest("phone is required".into()));
    }
    if name.chars().count() > 120 {
        return Err(ApiError::BadRequest("name too long".into()));
    }
    if phone.chars().count() > 40 {
        return Err(ApiError::BadRequest("phone too long".into()));
    }

    let input = CreateFlashContact {
        name,
        phone,
        time_preference: req.time_preference,
    };

    let contact = insert(&state.db, &input).await.map_err(ApiError::from)?;

    // Immediate Telegram notification
    let message = format_immediate_message(&contact);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client builder");
    crate::services::telegram_service::send_telegram_message(
        &client,
        &state.config.telegram.bot_token,
        state.config.telegram.admin_chat_id,
        &message,
    )
    .await;

    info!("Flash contact created: id={}", contact.id);

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": contact.id,
            "message": "Vielen Dank! Wir melden uns bei Ihnen."
        })),
    ))
}
