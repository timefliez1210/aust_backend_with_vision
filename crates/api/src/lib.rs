pub mod error;
pub mod middleware;
pub mod orchestrator;
pub mod routes;
pub mod services;
pub mod state;

#[cfg(test)]
pub(crate) mod test_helpers;

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

    let protected_api = routes::protected_api_router()
        .route_layer(axum::middleware::from_fn_with_state(
            shared_state.clone(),
            middleware::require_auth,
        ));

    Router::new()
        .merge(routes::health::router())
        .nest(
            "/api/v1",
            routes::public_api_router()
                .merge(protected_api)
                .merge(admin_routes)
                .merge(customer_routes)
                .layer(axum::extract::DefaultBodyLimit::max(250 * 1024 * 1024)),
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

#[cfg(test)]
mod auth_tests {
    use super::*;
    use axum::body::Body;
    use hyper::Request;
    use test_helpers::*;
    use tower::ServiceExt;

    async fn build_test_router() -> Router {
        let state = test_app_state().await;
        create_router(state)
    }

    #[tokio::test]
    async fn quotes_list_requires_auth() {
        let app = build_test_router().await;
        let resp = app
            .oneshot(
                Request::get("/api/v1/quotes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn quotes_list_works_with_valid_jwt() {
        let app = build_test_router().await;
        let token = generate_test_jwt();
        let resp = app
            .oneshot(
                Request::get("/api/v1/quotes")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn inquiry_photo_is_public() {
        let app = build_test_router().await;
        let resp = app
            .oneshot(
                Request::post("/api/v1/inquiries/photo")
                    .header("Content-Type", "multipart/form-data; boundary=test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Should NOT be 401 (may be 400/422 for bad request, but not auth error)
        assert_ne!(resp.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn image_proxy_is_public() {
        let app = build_test_router().await;
        let resp = app
            .oneshot(
                Request::get("/api/v1/estimates/images/test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Should NOT be 401 (may be 404 for missing image, but not auth error)
        assert_ne!(resp.status().as_u16(), 401);
    }

    #[tokio::test]
    async fn offer_generate_requires_auth() {
        let app = build_test_router().await;
        let resp = app
            .oneshot(
                Request::post("/api/v1/offers/generate")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }
}
