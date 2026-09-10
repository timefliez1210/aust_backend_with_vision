pub mod admin;
pub mod agent_activity;
pub(crate) mod admin_customers;
pub(crate) mod admin_emails;
pub mod auth;
pub mod calendar;
pub mod calendar_items;
pub mod customer;
pub mod distance;
pub mod employee;
pub mod estimates;
pub mod flash_contact;
pub mod health;
pub mod inquiries;
pub mod inquiry_actions;
pub mod inquiry_appointments;
pub mod invoices;
pub mod offers;
pub(crate) mod shared;
pub mod storage;
pub mod submissions;
pub mod vehicles;

use crate::AppState;
use axum::{routing::post, Router};
use std::sync::Arc;

/// Auth-only public routes — rate-limited in `lib.rs`.
///
/// **Why**: These endpoints accept credentials/OTP codes without requiring a token.
///          They are isolated here so `lib.rs` can wrap them with a rate-limit layer
///          without touching non-auth public endpoints (media proxy, submissions).
pub fn auth_public_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/customer", customer::auth_router())
        .nest("/employee", employee::auth_router())
}

/// Non-auth public routes that carry no rate limit.
///
/// **Why**: The media proxy is hit once per `<img>` on a dashboard page, so a per-IP
/// limit here would throttle one admin loading one estimation. It serves only
/// `estimates/` keys (see `estimates::serve_image`).
pub fn public_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/estimates", estimates::public_router())
        .nest("/media", estimates::public_router())
        .route("/distance/calculate", post(distance::calculate))
        .merge(flash_contact::router())
}

/// Unauthenticated form submissions, kept separate so `lib.rs` can rate-limit them.
///
/// **Why**: These five endpoints create customers and inquiries and kick off paid
/// vision and LLM work, and they take uploads. Unlimited and unauthenticated, one
/// caller can fill the dashboard with junk inquiries and run up the model bill. A real
/// customer submits once, so the limit can be tight.
pub fn submit_api_router() -> Router<Arc<AppState>> {
    Router::new().nest("/submit", submissions::submit_router())
}

/// Protected API routes (require admin JWT).
pub fn protected_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/inquiries", inquiries::router())
        .nest("/calendar", calendar::router())
        .nest("/estimates", estimates::protected_router())
}
