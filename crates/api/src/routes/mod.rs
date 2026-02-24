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

use crate::AppState;
use axum::extract::DefaultBodyLimit;
use axum::Router;
use std::sync::Arc;

pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/quotes", quotes::router())
        .nest("/estimates", estimates::router())
        .nest("/distance", distance::router())
        .nest("/offers", offers::router())
        .nest("/calendar", calendar::router())
        .nest("/inquiries", inquiries::router())
        .nest("/customer", customer::auth_router())
        .layer(DefaultBodyLimit::max(250 * 1024 * 1024)) // 250MB for image uploads
}
