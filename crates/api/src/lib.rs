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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::body::Body;
    use hyper::Request;
    use test_helpers::*;
    use tower::ServiceExt;

    /// Helper: build a test router with a DB pool and return (router, pool).
    /// Does NOT clean data — tests must be self-contained and not depend on empty tables.
    async fn setup() -> (Router, sqlx::PgPool) {
        let pool = test_helpers::test_db_pool().await;
        let state = test_helpers::test_app_state_with_pool(pool.clone()).await;
        (create_router(state), pool)
    }

    /// Helper: read response body as JSON value.
    async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Helper: create an authenticated GET request.
    fn authed_get(path: &str) -> Request<Body> {
        let token = generate_test_jwt();
        Request::get(path)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    /// Helper: create an authenticated POST request with JSON body.
    fn authed_post(path: &str, body: serde_json::Value) -> Request<Body> {
        let token = generate_test_jwt();
        Request::post(path)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap()
    }

    /// Helper: create an authenticated PATCH request with JSON body.
    fn authed_patch(path: &str, body: serde_json::Value) -> Request<Body> {
        let token = generate_test_jwt();
        Request::patch(path)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap()
    }

    /// Helper: create an authenticated DELETE request.
    fn authed_delete(path: &str) -> Request<Body> {
        let token = generate_test_jwt();
        Request::delete(path)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    /// Helper: create an authenticated PUT request with JSON body.
    fn authed_put(path: &str, body: serde_json::Value) -> Request<Body> {
        let token = generate_test_jwt();
        Request::put(path)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap()
    }

    // ========== Health Endpoints ==========

    #[tokio::test]
    async fn health_returns_200() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "healthy");
    }

    #[tokio::test]
    async fn ready_returns_200_with_db() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "ready");
        assert_eq!(body["database"], "connected");
    }

    // ========== Quotes CRUD ==========

    #[tokio::test]
    async fn create_and_get_quote() {
        let (app, pool) = setup().await;
        let customer_id = insert_test_customer(&pool).await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/quotes",
                serde_json::json!({
                    "customer_id": customer_id,
                    "notes": "Testumzug"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "create quote should succeed");
        let body = body_json(resp).await;
        let quote_id = body["id"].as_str().unwrap();
        assert_eq!(body["status"], "pending");
        assert_eq!(body["notes"], "Testumzug");

        // GET the created quote
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["quote"]["id"], quote_id);
        assert_eq!(body["customer"]["id"], customer_id.to_string());
    }

    #[tokio::test]
    async fn list_quotes_returns_200() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/quotes"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["total"].is_number());
        assert!(body["quotes"].is_array());
    }

    #[tokio::test]
    async fn list_quotes_filter_by_customer() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        // Get the customer_id for this quote
        let (customer_id,): (uuid::Uuid,) =
            sqlx::query_as("SELECT customer_id FROM quotes WHERE id = $1")
                .bind(quote_id)
                .fetch_one(&pool)
                .await
                .unwrap();

        let resp = app
            .oneshot(authed_get(&format!(
                "/api/v1/quotes?customer_id={customer_id}"
            )))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["total"].as_i64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn update_quote_status() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/quotes/{quote_id}"),
                serde_json::json!({ "status": "volume_estimated" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "volume_estimated");
    }

    #[tokio::test]
    async fn update_quote_volume() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/quotes/{quote_id}"),
                serde_json::json!({ "estimated_volume_m3": 35.5 }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!((body["estimated_volume_m3"].as_f64().unwrap() - 35.5).abs() < 0.01);
    }

    #[tokio::test]
    async fn soft_delete_quote() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_delete(&format!("/api/v1/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "cancelled");
    }

    #[tokio::test]
    async fn get_nonexistent_quote_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/quotes/{fake_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    // ========== Calendar Endpoints ==========

    #[tokio::test]
    async fn calendar_availability() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/calendar/availability?date=2026-06-15"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body.get("requested_date_available").is_some());
        assert!(body.get("requested_date").is_some());
    }

    #[tokio::test]
    async fn calendar_schedule_range() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get(
                "/api/v1/calendar/schedule?from=2026-06-01&to=2026-06-07",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        let entries = body.as_array().unwrap();
        assert_eq!(entries.len(), 7, "7 days in range");
    }

    #[tokio::test]
    async fn calendar_schedule_rejects_over_90_days() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get(
                "/api/v1/calendar/schedule?from=2026-01-01&to=2026-12-31",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn calendar_booking_crud() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        // Create booking
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/calendar/bookings",
                serde_json::json!({
                    "booking_date": "2026-07-15",
                    "quote_id": quote_id,
                    "customer_name": "Test Kunde",
                    "customer_email": "test@example.com",
                    "status": "tentative"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        let booking_id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["status"], "tentative");

        // Get booking
        let resp = app
            .clone()
            .oneshot(authed_get(&format!(
                "/api/v1/calendar/bookings/{booking_id}"
            )))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Confirm booking
        let resp = app
            .clone()
            .oneshot(authed_patch(
                &format!("/api/v1/calendar/bookings/{booking_id}"),
                serde_json::json!({ "status": "confirmed" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "confirmed");

        // Cancel booking
        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/calendar/bookings/{booking_id}"),
                serde_json::json!({ "status": "cancelled" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "cancelled");
    }

    #[tokio::test]
    async fn calendar_invalid_status_transition() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, quote_id, "tentative").await;

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/calendar/bookings/{booking_id}"),
                serde_json::json!({ "status": "invalid_status" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn calendar_set_capacity() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_put(
                "/api/v1/calendar/capacity/2026-08-01",
                serde_json::json!({ "capacity": 5 }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["capacity"], 5);
    }

    #[tokio::test]
    async fn calendar_negative_capacity_rejected() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_put(
                "/api/v1/calendar/capacity/2026-08-01",
                serde_json::json!({ "capacity": -1 }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn calendar_delete_booking() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, quote_id, "tentative").await;

        let resp = app
            .oneshot(authed_delete(&format!(
                "/api/v1/calendar/bookings/{booking_id}"
            )))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // ========== Offers Endpoints ==========

    #[tokio::test]
    async fn get_nonexistent_offer_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/offers/{fake_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn get_offer_returns_inserted() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        let offer_id = insert_test_offer(&pool, quote_id, "draft").await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/offers/{offer_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["id"], offer_id.to_string());
        assert_eq!(body["status"], "draft");
        assert_eq!(body["price_cents"], 50000);
    }

    #[tokio::test]
    async fn get_offer_pdf_missing_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/offers/{fake_id}/pdf")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    // ========== Estimates Endpoints ==========

    #[tokio::test]
    async fn get_nonexistent_estimate_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/estimates/{fake_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn get_estimate_returns_inserted() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        let est_id = insert_test_estimation(&pool, quote_id, "vision", 15.0).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/estimates/{est_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["id"], est_id.to_string());
        assert_eq!(body["method"], "vision");
    }

    #[tokio::test]
    async fn inventory_estimate_creates_estimation() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_post(
                "/api/v1/estimates/inventory",
                serde_json::json!({
                    "quote_id": quote_id,
                    "inventory": {
                        "items": [
                            { "name": "Sofa", "quantity": 1, "volume_m3": 1.5 },
                            { "name": "Tisch", "quantity": 2, "volume_m3": 0.5 }
                        ]
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["method"], "inventory");
        assert!(body["total_volume_m3"].as_f64().unwrap() > 0.0);
    }

    // ========== Auth Endpoints ==========

    #[tokio::test]
    async fn login_with_empty_fields_returns_422() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "email": "",
                            "password": ""
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 422);
    }

    #[tokio::test]
    async fn login_with_bad_credentials_returns_401() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(
                Request::post("/api/v1/auth/login")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "email": "nobody@test.com",
                            "password": "wrongpassword"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    // ========== Quote with Enriched Data ==========

    #[tokio::test]
    async fn get_quote_includes_addresses_and_customer() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        // Enriched response includes customer and addresses
        assert!(body.get("customer").is_some(), "should have customer");
        assert!(body.get("origin_address").is_some(), "should have origin");
        assert!(
            body.get("destination_address").is_some(),
            "should have destination"
        );
        // Verify address data populated by insert_test_quote
        assert!(!body["customer"]["email"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_quote_includes_linked_offers() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        insert_test_offer(&pool, quote_id, "draft").await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        let offers = body["offers"].as_array().unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0]["status"], "draft");
    }

    // ========== Cross-Endpoint Workflows ==========

    #[tokio::test]
    async fn booking_confirm_syncs_quote_status() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote_with_status(&pool, "offer_sent").await;
        insert_test_offer(&pool, quote_id, "sent").await;
        let booking_id = insert_test_booking(&pool, quote_id, "tentative").await;

        // Confirm booking via API
        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/calendar/bookings/{booking_id}"),
                serde_json::json!({ "status": "confirmed" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Quote should have synced to "accepted"
        let status = get_quote_status(&pool, quote_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn booking_cancel_syncs_quote_to_rejected() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote_with_status(&pool, "accepted").await;
        insert_test_offer(&pool, quote_id, "sent").await;
        let booking_id = insert_test_booking(&pool, quote_id, "confirmed").await;

        // Cancel booking via API
        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/calendar/bookings/{booking_id}"),
                serde_json::json!({ "status": "cancelled" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Quote should sync to "rejected" (no active bookings remain)
        let status = get_quote_status(&pool, quote_id).await;
        assert_eq!(status, "rejected");
    }

    // ========== Error Handling ==========

    #[tokio::test]
    async fn invalid_uuid_returns_400() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/quotes/not-a-uuid"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn malformed_json_returns_422_or_400() {
        let (app, _pool) = setup().await;
        let token = generate_test_jwt();
        let resp = app
            .oneshot(
                Request::post("/api/v1/quotes")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{invalid json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status().as_u16();
        assert!(
            status == 400 || status == 422,
            "malformed JSON should return 400 or 422, got {status}"
        );
    }

    #[tokio::test]
    async fn create_quote_with_bad_customer_id_returns_error() {
        let (app, _pool) = setup().await;
        let fake_customer = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_post(
                "/api/v1/quotes",
                serde_json::json!({
                    "customer_id": fake_customer,
                }),
            ))
            .await
            .unwrap();
        // Should fail due to FK constraint
        assert_eq!(resp.status(), 500);
    }

    // ========== Pagination ==========

    #[tokio::test]
    async fn list_quotes_respects_limit_and_offset() {
        let (app, _pool) = setup().await;

        // Request with limit=2 — should return at most 2
        let resp = app
            .clone()
            .oneshot(authed_get("/api/v1/quotes?limit=2&offset=0"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["quotes"].as_array().unwrap().len() <= 2);
        assert_eq!(body["limit"], 2);
        assert_eq!(body["offset"], 0);

        // Request with offset — should return offset in response
        let resp = app
            .oneshot(authed_get("/api/v1/quotes?limit=2&offset=100"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["offset"], 100);
    }
}
