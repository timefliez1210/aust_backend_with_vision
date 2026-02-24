pub mod error;
pub mod middleware;
pub mod orchestrator;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use orchestrator::{run_offer_event_handler, try_auto_generate_offer};
pub use state::AppState;

use axum::{http::HeaderValue, Router};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub fn create_router(state: AppState) -> Router {
    let shared_state = Arc::new(state);

    let allowed_origins = [
        "https://www.aust-umzuege.de",
        "https://aust-umzuege.de",
        "http://localhost:5173",
        "capacitor://localhost",
        "http://localhost",
    ]
    .into_iter()
    .filter_map(|o| o.parse::<HeaderValue>().ok())
    .collect::<Vec<_>>();

    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
        .expose_headers([axum::http::header::CONTENT_DISPOSITION]);

    let admin_routes = Router::new()
        .nest("/admin", routes::admin::router())
        .nest("/auth", routes::auth::protected_router())
        .route_layer(axum::middleware::from_fn_with_state(
            shared_state.clone(),
            middleware::require_auth,
        ));

    let customer_routes = Router::new()
        .nest("/customer", routes::customer::protected_router())
        .route_layer(axum::middleware::from_fn_with_state(
            shared_state.clone(),
            middleware::require_customer_auth,
        ));

    Router::new()
        .merge(routes::health::router())
        .nest(
            "/api/v1",
            routes::api_router()
                .merge(admin_routes)
                .merge(customer_routes),
        )
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(shared_state)
}

pub async fn create_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
}
