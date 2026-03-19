pub mod admin;
pub mod auth;
pub mod calendar;
pub mod calendar_items;
pub mod customer;
pub mod distance;
pub mod employee;
pub mod estimates;
pub mod health;
pub mod inquiries;
pub mod invoices;
pub mod offers;
pub(crate) mod shared;

use crate::AppState;
use axum::{routing::post, Router};
use std::sync::Arc;

/// Public API routes (no authentication required).
pub fn public_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/submit", inquiries::submit_router())
        .nest("/customer", customer::auth_router())
        .nest("/employee", employee::auth_router())
        .nest("/estimates", estimates::public_router())
        .nest("/media", estimates::public_router())
        .route("/distance/calculate", post(distance::calculate))
}

/// Protected API routes (require admin JWT).
pub fn protected_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/inquiries", inquiries::router())
        .nest("/calendar", calendar::router())
        .nest("/estimates", estimates::protected_router())
}
