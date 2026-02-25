pub mod admin;
pub mod auth;
pub mod calendar;
pub mod customer;
pub mod distance;
pub mod estimates;
pub mod health;
pub mod inquiries;
pub mod offers;
pub mod quotes;
pub(crate) mod shared;

use crate::AppState;
use axum::Router;
use std::sync::Arc;

/// Public API routes (no authentication required).
pub fn public_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/inquiries", inquiries::router())
        .nest("/customer", customer::auth_router())
        .nest("/estimates", estimates::public_router())
}

/// Protected API routes (require admin JWT).
pub fn protected_api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/quotes", quotes::router())
        .nest("/estimates", estimates::protected_router())
        .nest("/distance", distance::router())
        .nest("/offers", offers::router())
        .nest("/calendar", calendar::router())
}
