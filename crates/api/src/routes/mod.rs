pub mod auth;
pub mod distance;
pub mod estimates;
pub mod health;
pub mod offers;
pub mod quotes;

use crate::AppState;
use axum::Router;
use std::sync::Arc;

pub fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/quotes", quotes::router())
        .nest("/estimates", estimates::router())
        .nest("/distance", distance::router())
        .nest("/offers", offers::router())
}
