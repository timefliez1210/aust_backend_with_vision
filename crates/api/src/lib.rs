pub mod error;
pub mod middleware;
pub mod orchestrator;
pub(crate) mod repositories;
pub mod routes;
pub mod services;
pub mod state;
pub(crate) mod types;

pub mod test_helpers;

pub use error::ApiError;
pub use orchestrator::run_offer_event_handler;
pub use services::offer_pipeline::try_auto_generate_offer;
pub use state::AppState;

use axum::{extract::Request, http::HeaderValue, middleware::Next, Router};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub fn create_router(state: AppState) -> Router {
    let shared_state = Arc::new(state);

    let allowed_origins = [
        "https://www.aust-umzuege.de",
        "https://aust-umzuege.de",
        "http://localhost:5173",
        "http://localhost:4173",
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
        .nest("/admin/agent-activity", routes::agent_activity::router())
        .nest("/admin/calendar-items", routes::calendar_items::router())
        .nest("/admin/vehicles", routes::vehicles::router())
        .nest("/admin/storage", routes::storage::router())
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

    let employee_routes = Router::new()
        .nest("/employee", routes::employee::protected_router())
        .route_layer(axum::middleware::from_fn_with_state(
            shared_state.clone(),
            middleware::require_employee_auth,
        ));

    let protected_api = routes::protected_api_router()
        .route_layer(axum::middleware::from_fn_with_state(
            shared_state.clone(),
            middleware::require_auth,
        ));

    // Auth endpoints are rate-limited to 10 req/min per IP to slow brute-force attacks.
    let rate_limiter = Arc::new(middleware::RateLimiter::new(10, Duration::from_secs(60)));
    let rl = rate_limiter.clone();
    let auth_routes = routes::auth_public_router().layer(axum::middleware::from_fn(
        move |req: Request, next: Next| {
            let limiter = rl.clone();
            async move { middleware::apply_rate_limit(limiter, req, next).await }
        },
    ));

    Router::new()
        .merge(routes::health::router())
        .nest(
            "/api/v1",
            routes::public_api_router()
                .merge(auth_routes)
                .merge(protected_api)
                .merge(admin_routes)
                .merge(customer_routes)
                .merge(employee_routes)
                .layer(axum::extract::DefaultBodyLimit::max(250 * 1024 * 1024)),
        )
        // Layer order (outermost → innermost): cors → security_headers → request_id → trace
        .layer(axum::middleware::from_fn(middleware::set_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(middleware::set_security_headers))
        .layer(cors)
        .with_state(shared_state)
}

pub async fn create_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(database_url)
        .await
}


