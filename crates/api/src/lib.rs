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

    // ========== Error Path Tests ==========

    #[tokio::test]
    async fn get_nonexistent_customer_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/admin/customers/{fake_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn update_nonexistent_address_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/admin/addresses/{fake_id}"),
                serde_json::json!({ "street": "Test" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn create_quote_with_volume_sets_status_volume_estimated() {
        let (app, pool) = setup().await;
        let customer_id = insert_test_customer(&pool).await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/admin/quotes",
                serde_json::json!({
                    "customer_id": customer_id,
                    "origin": {
                        "street": "Musterstr. 1",
                        "city": "Hildesheim",
                        "postal_code": "31135"
                    },
                    "destination": {
                        "street": "Zielstr. 5",
                        "city": "Hannover"
                    },
                    "estimated_volume_m3": 20.0
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let quote_id = create_body["id"].as_str().unwrap();

        // GET the quote via admin detail endpoint
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/admin/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "volume_estimated");
    }

    #[tokio::test]
    async fn create_quote_without_volume_sets_status_pending() {
        let (app, pool) = setup().await;
        let customer_id = insert_test_customer(&pool).await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/admin/quotes",
                serde_json::json!({
                    "customer_id": customer_id,
                    "origin": {
                        "street": "Musterstr. 1",
                        "city": "Hildesheim"
                    },
                    "destination": {
                        "street": "Zielstr. 5",
                        "city": "Hannover"
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let quote_id = create_body["id"].as_str().unwrap();

        // GET the quote — status should be pending
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/admin/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "pending");
    }

    #[tokio::test]
    async fn list_admin_offers_returns_200() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/admin/offers"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn get_offer_detail_returns_correct_structure() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;
        let offer_id = insert_test_offer(&pool, quote_id, "draft").await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/admin/offers/{offer_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body.get("id").is_some(), "response should have id");
        assert!(body.get("status").is_some(), "response should have status");
        assert!(
            body.get("total_netto_cents").is_some(),
            "response should have total_netto_cents"
        );
    }

    // ========== Quote Detail with Elevator ==========

    #[tokio::test]
    async fn quote_detail_includes_elevator_on_addresses() {
        let (app, pool) = setup().await;
        let customer_id = insert_test_customer(&pool).await;

        // Create quote with elevator fields via admin endpoint
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/admin/quotes",
                serde_json::json!({
                    "customer_id": customer_id,
                    "origin": {
                        "street": "Musterstr. 1",
                        "city": "Hildesheim",
                        "postal_code": "31135",
                        "floor": "3. Stock",
                        "elevator": false
                    },
                    "destination": {
                        "street": "Zielstr. 5",
                        "city": "Hannover",
                        "elevator": true
                    }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let quote_id = create_body["id"].as_str().unwrap();

        // GET quote detail — should include elevator on addresses
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/admin/quotes/{quote_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(
            body["origin"]["elevator"], false,
            "origin elevator should be false"
        );
        assert_eq!(
            body["destination"]["elevator"], true,
            "destination elevator should be true"
        );
    }

    // ========== Pagination Edge Cases ==========

    #[tokio::test]
    async fn list_quotes_with_zero_limit_returns_empty() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/quotes?limit=0"))
            .await
            .unwrap();
        let status = resp.status().as_u16();
        // limit=0 is passed through to SQL LIMIT 0 which returns empty results
        // (not rejected as invalid)
        assert_eq!(status, 200, "limit=0 should return 200 with empty results");
        let body = body_json(resp).await;
        assert!(
            body["quotes"].as_array().unwrap().is_empty(),
            "limit=0 should return no quotes"
        );
    }

    #[tokio::test]
    async fn list_quotes_offset_beyond_total_returns_empty() {
        let (app, pool) = setup().await;
        insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_get("/api/v1/quotes?limit=10&offset=9999"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(
            body["quotes"].as_array().unwrap().is_empty(),
            "offset beyond total should return empty array"
        );
    }

    // ========== Status Transitions ==========

    #[tokio::test]
    async fn set_quote_status_to_done() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_post(
                &format!("/api/v1/admin/quotes/{quote_id}/status"),
                serde_json::json!({ "status": "done" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn set_quote_status_invalid_returns_400() {
        let (app, pool) = setup().await;
        let quote_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_post(
                &format!("/api/v1/admin/quotes/{quote_id}/status"),
                serde_json::json!({ "status": "flying_monkeys" }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "invalid status should return 400 Bad Request"
        );
    }
}

#[cfg(test)]
mod calendar_service_tests {
    use aust_calendar::{CalendarService, NewBooking};
    use chrono::{Datelike, NaiveDate};
    use crate::test_helpers::*;

    /// Helper to build a NewBooking for a given date with no quote_id.
    fn new_booking(date: NaiveDate) -> NewBooking {
        NewBooking {
            booking_date: date,
            quote_id: None,
            customer_name: Some("Test Kunde".to_string()),
            customer_email: Some("cal-test@example.com".to_string()),
            departure_address: None,
            arrival_address: None,
            volume_m3: None,
            distance_km: None,
            description: None,
            status: "confirmed".to_string(),
        }
    }

    /// Clean all bookings for a specific date to ensure test isolation.
    async fn clean_date(pool: &sqlx::PgPool, date: NaiveDate) {
        sqlx::query("DELETE FROM calendar_bookings WHERE booking_date = $1")
            .bind(date)
            .execute(pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn availability_empty_db_is_available() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 2).unwrap(); // Monday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 3, 3, 30);

        let result = svc.check_availability(date).await.unwrap();

        assert!(result.requested_date_available);
        assert_eq!(result.requested_date_info.capacity, 3);
        assert_eq!(result.requested_date_info.booked, 0);
    }

    #[tokio::test]
    async fn availability_at_capacity_is_unavailable() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 3).unwrap(); // Tuesday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 1, 3, 30);

        svc.create_booking(new_booking(date)).await.unwrap();

        let result = svc.check_availability(date).await.unwrap();
        assert!(!result.requested_date_available);
        assert_eq!(result.requested_date_info.remaining, 0);
    }

    #[tokio::test]
    async fn availability_suggests_alternatives_when_full() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 4).unwrap(); // Wednesday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 1, 3, 30);

        svc.create_booking(new_booking(date)).await.unwrap();

        let result = svc.check_availability(date).await.unwrap();
        assert!(!result.requested_date_available);
        assert!(!result.alternatives.is_empty(), "should suggest alternatives");
        for alt in &result.alternatives {
            assert!(alt.available);
        }
    }

    #[tokio::test]
    async fn create_booking_succeeds_when_capacity_available() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 9).unwrap(); // Monday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 2, 3, 30);

        let result = svc.create_booking(new_booking(date)).await;
        assert!(result.is_ok());
        let booking = result.unwrap();
        assert_eq!(booking.booking_date, date);
        assert_eq!(booking.status, "confirmed");
    }

    #[tokio::test]
    async fn create_booking_fails_when_at_capacity() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 10).unwrap(); // Tuesday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 1, 3, 30);

        svc.create_booking(new_booking(date)).await.unwrap();

        let result = svc.create_booking(new_booking(date)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, aust_calendar::CalendarError::FullyBooked(_)),
            "expected FullyBooked error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn force_create_booking_bypasses_capacity() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 11).unwrap(); // Wednesday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 1, 3, 30);

        svc.create_booking(new_booking(date)).await.unwrap();

        // force_create should succeed even though capacity=1 and 1 booking exists
        let result = svc.force_create_booking(new_booking(date)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cancel_booking_sets_status() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 12).unwrap(); // Thursday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 3, 3, 30);

        let booking = svc.create_booking(new_booking(date)).await.unwrap();

        svc.cancel_booking(booking.id).await.unwrap();

        let status = get_booking_status(&pool, booking.id).await;
        assert_eq!(status, "cancelled");
    }

    #[tokio::test]
    async fn confirm_booking_sets_status() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 16).unwrap(); // Monday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 3, 3, 30);

        let mut nb = new_booking(date);
        nb.status = "tentative".to_string();
        let booking = svc.create_booking(nb).await.unwrap();

        svc.confirm_booking(booking.id).await.unwrap();

        let status = get_booking_status(&pool, booking.id).await;
        assert_eq!(status, "confirmed");
    }

    #[tokio::test]
    async fn set_capacity_override_takes_effect() {
        let pool = test_db_pool().await;
        let date = NaiveDate::from_ymd_opt(2098, 6, 17).unwrap(); // Tuesday
        clean_date(&pool, date).await;
        let svc = CalendarService::new(pool.clone(), 3, 3, 30);

        svc.set_capacity(date, 0).await.unwrap();

        let result = svc.check_availability(date).await.unwrap();
        assert!(!result.requested_date_available);
        assert_eq!(result.requested_date_info.capacity, 0);
    }

    #[tokio::test]
    async fn find_nearest_available_skips_sundays() {
        let pool = test_db_pool().await;
        // 2098-07-05 = Saturday, 2098-07-06 = Sunday, 2098-07-07 = Monday
        let sat = NaiveDate::from_ymd_opt(2098, 7, 5).unwrap();
        let mon = NaiveDate::from_ymd_opt(2098, 7, 7).unwrap();
        let tue = NaiveDate::from_ymd_opt(2098, 7, 8).unwrap();
        clean_date(&pool, sat).await;
        clean_date(&pool, mon).await;
        clean_date(&pool, tue).await;
        assert_eq!(sat.weekday(), chrono::Weekday::Sat);
        assert_eq!(mon.weekday(), chrono::Weekday::Mon);
        assert_eq!(tue.weekday(), chrono::Weekday::Tue);

        let svc = CalendarService::new(pool.clone(), 1, 3, 30);

        svc.create_booking(new_booking(sat)).await.unwrap();
        svc.create_booking(new_booking(mon)).await.unwrap();
        svc.create_booking(new_booking(tue)).await.unwrap();

        let alternatives = svc.find_nearest_available(sat, 3).await.unwrap();
        for alt in &alternatives {
            assert_ne!(
                alt.date.weekday(),
                chrono::Weekday::Sun,
                "alternative {} is a Sunday",
                alt.date
            );
        }
    }

    #[tokio::test]
    async fn get_schedule_returns_correct_range() {
        let pool = test_db_pool().await;
        let svc = CalendarService::new(pool.clone(), 3, 3, 30);

        let from = NaiveDate::from_ymd_opt(2098, 7, 14).unwrap(); // Monday
        let to = NaiveDate::from_ymd_opt(2098, 7, 16).unwrap(); // Wednesday

        let schedule = svc.get_schedule(from, to).await.unwrap();
        assert_eq!(schedule.len(), 3, "3-day window should produce 3 entries");
        assert_eq!(schedule[0].date, from);
        assert_eq!(schedule[1].date, NaiveDate::from_ymd_opt(2098, 7, 15).unwrap());
        assert_eq!(schedule[2].date, to);
    }
}
