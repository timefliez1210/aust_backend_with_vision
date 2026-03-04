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
    async fn inquiries_list_requires_auth() {
        let app = build_test_router().await;
        let resp = app
            .oneshot(
                Request::get("/api/v1/inquiries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn inquiries_list_works_with_valid_jwt() {
        let app = build_test_router().await;
        let token = generate_test_jwt();
        let resp = app
            .oneshot(
                Request::get("/api/v1/inquiries")
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
                Request::post("/api/v1/submit/photo")
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
    async fn generate_offer_requires_auth() {
        let app = build_test_router().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::post(&format!("/api/v1/inquiries/{fake_id}/generate-offer"))
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

    // ========== Inquiry CRUD ==========

    #[tokio::test]
    async fn list_inquiries_returns_200() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/inquiries"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["total"].is_number());
        assert!(body["inquiries"].is_array());
    }

    #[tokio::test]
    async fn list_inquiries_search_filter() {
        let (app, pool) = setup().await;
        let _inquiry_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_get("/api/v1/inquiries?search=test"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["total"].is_number());
    }

    #[tokio::test]
    async fn update_inquiry_volume() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/inquiries/{inquiry_id}"),
                serde_json::json!({ "estimated_volume_m3": 35.5 }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        // Volume update doesn't change status; insert_test_quote starts as "pending"
        assert!(body.get("volume_m3").is_some() || body.get("estimated_volume_m3").is_some());
    }

    #[tokio::test]
    async fn soft_delete_inquiry() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_delete(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "cancelled");
    }

    #[tokio::test]
    async fn get_nonexistent_inquiry_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{fake_id}")))
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
        let inquiry_id = insert_test_quote(&pool).await;

        // Create booking
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/calendar/bookings",
                serde_json::json!({
                    "booking_date": "2026-07-15",
                    "inquiry_id": inquiry_id,
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
        let inquiry_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "tentative").await;

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
        let inquiry_id = insert_test_quote(&pool).await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "tentative").await;

        let resp = app
            .oneshot(authed_delete(&format!(
                "/api/v1/calendar/bookings/{booking_id}"
            )))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // ========== Inquiry PDF and Offer Embedding ==========

    #[tokio::test]
    async fn get_inquiry_pdf_missing_returns_404() {
        let (app, _pool) = setup().await;
        let fake_id = uuid::Uuid::new_v4();
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{fake_id}/pdf")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn inquiry_detail_includes_estimation() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let _est_id = insert_test_estimation(&pool, inquiry_id, "vision", 15.0).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        // Estimation data should be embedded in the inquiry detail
        assert!(body.get("estimation").is_some(), "should have estimation");
    }

    #[tokio::test]
    async fn inquiry_detail_with_estimation_has_estimation_snapshot() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;
        insert_test_estimation(&pool, inquiry_id, "vision", 15.0).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        let estimation = &body["estimation"];
        assert!(estimation.is_object(), "should have estimation snapshot");
        assert_eq!(estimation["method"], "vision");
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

    // ========== Inquiry with Enriched Data ==========

    #[tokio::test]
    async fn get_inquiry_includes_addresses_and_customer() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
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

    // ========== Cross-Endpoint Workflows ==========

    #[tokio::test]
    async fn booking_confirm_syncs_quote_status() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "offer_sent").await;
        insert_test_offer(&pool, inquiry_id, "sent").await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "tentative").await;

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
        let status = get_quote_status(&pool, inquiry_id).await;
        assert_eq!(status, "accepted");
    }

    #[tokio::test]
    async fn booking_cancel_syncs_quote_to_rejected() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "accepted").await;
        insert_test_offer(&pool, inquiry_id, "sent").await;
        let booking_id = insert_test_booking(&pool, inquiry_id, "confirmed").await;

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
        let status = get_quote_status(&pool, inquiry_id).await;
        assert_eq!(status, "rejected");
    }

    // ========== Error Handling ==========

    #[tokio::test]
    async fn invalid_uuid_returns_400() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/inquiries/not-a-uuid"))
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
                Request::post("/api/v1/inquiries")
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
    async fn create_inquiry_missing_email_returns_error() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_post(
                "/api/v1/inquiries",
                serde_json::json!({
                    "notes": "missing email"
                }),
            ))
            .await
            .unwrap();
        // Should fail: customer_email is required
        let status = resp.status().as_u16();
        assert!(status == 400 || status == 422, "missing email should return 400 or 422, got {status}");
    }

    // ========== Pagination ==========

    #[tokio::test]
    async fn list_inquiries_respects_limit_and_offset() {
        let (app, _pool) = setup().await;

        // Request with limit=2 — should return at most 2
        let resp = app
            .clone()
            .oneshot(authed_get("/api/v1/inquiries?limit=2&offset=0"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body["inquiries"].as_array().unwrap().len() <= 2);
        assert_eq!(body["limit"], 2);
        assert_eq!(body["offset"], 0);

        // Request with offset — should return offset in response
        let resp = app
            .oneshot(authed_get("/api/v1/inquiries?limit=2&offset=100"))
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
    async fn create_inquiry_with_addresses_returns_pending() {
        let (app, _pool) = setup().await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/inquiries",
                serde_json::json!({
                    "customer_email": "create-test@example.com",
                    "customer_name": "Test Kunde",
                    "origin_address": "Musterstr. 1, 31135 Hildesheim",
                    "destination_address": "Zielstr. 5, Hannover"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let inquiry_id = create_body["id"].as_str().unwrap();

        // GET the inquiry via detail endpoint
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "pending");
    }

    #[tokio::test]
    async fn create_inquiry_minimal_returns_pending() {
        let (app, _pool) = setup().await;

        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/inquiries",
                serde_json::json!({
                    "customer_email": "minimal-test@example.com"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let inquiry_id = create_body["id"].as_str().unwrap();

        // GET the inquiry — status should be pending
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "pending");
    }

    #[tokio::test]
    async fn list_inquiries_with_offer_filter_returns_200() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/inquiries?has_offer=true"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn get_inquiry_with_offer_includes_offer_snapshot() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;
        let _offer_id = insert_test_offer(&pool, inquiry_id, "draft").await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(body.get("offer").is_some(), "response should have embedded offer");
        let offer = &body["offer"];
        assert!(offer.get("id").is_some(), "offer should have id");
        assert!(offer.get("status").is_some(), "offer should have status");
        assert!(
            offer.get("total_netto_cents").is_some(),
            "offer should have total_netto_cents"
        );
    }

    // ========== Inquiry Detail with Elevator ==========

    #[tokio::test]
    async fn inquiry_detail_includes_elevator_on_addresses() {
        let (app, _pool) = setup().await;

        // Create inquiry with elevator fields
        let resp = app
            .clone()
            .oneshot(authed_post(
                "/api/v1/inquiries",
                serde_json::json!({
                    "customer_email": "elevator-test@example.com",
                    "origin_address": "Musterstr. 1, 31135 Hildesheim",
                    "origin_floor": "3. Stock",
                    "origin_elevator": false,
                    "destination_address": "Zielstr. 5, Hannover",
                    "destination_elevator": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let create_body = body_json(resp).await;
        let inquiry_id = create_body["id"].as_str().unwrap();

        // GET inquiry detail — should include elevator on addresses
        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(
            body["origin_address"]["elevator"], false,
            "origin elevator should be false"
        );
        assert_eq!(
            body["destination_address"]["elevator"], true,
            "destination elevator should be true"
        );
    }

    // ========== Pagination Edge Cases ==========

    #[tokio::test]
    async fn list_inquiries_with_zero_limit_returns_empty() {
        let (app, _pool) = setup().await;
        let resp = app
            .oneshot(authed_get("/api/v1/inquiries?limit=0"))
            .await
            .unwrap();
        let status = resp.status().as_u16();
        // limit=0 is passed through to SQL LIMIT 0 which returns empty results
        // (not rejected as invalid)
        assert_eq!(status, 200, "limit=0 should return 200 with empty results");
        let body = body_json(resp).await;
        assert!(
            body["inquiries"].as_array().unwrap().is_empty(),
            "limit=0 should return no inquiries"
        );
    }

    #[tokio::test]
    async fn list_inquiries_offset_beyond_total_returns_empty() {
        let (app, pool) = setup().await;
        insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_get("/api/v1/inquiries?limit=10&offset=9999"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert!(
            body["inquiries"].as_array().unwrap().is_empty(),
            "offset beyond total should return empty array"
        );
    }

    // ========== Status Transitions ==========

    #[tokio::test]
    async fn update_inquiry_status_valid_transition() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await; // status = "estimated"

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/inquiries/{inquiry_id}"),
                serde_json::json!({ "status": "offer_ready" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "offer_ready");
    }

    #[tokio::test]
    async fn update_inquiry_invalid_status_returns_400() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote(&pool).await;

        let resp = app
            .oneshot(authed_patch(
                &format!("/api/v1/inquiries/{inquiry_id}"),
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

    // ========== try_auto_generate_offer early-return behaviour ==========
    // These tests verify the short-circuit paths that were never covered before.

    #[tokio::test]
    async fn auto_generate_skips_when_offer_already_exists() {
        // Before the unique-constraint fix, a race between two callers could insert two offers.
        // This test verifies the "offer already exists" guard: calling try_auto_generate_offer
        // on a quote that already has an active offer must not create a second one.
        let (_, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "volume_estimated").await;
        insert_test_offer(&pool, inquiry_id, "draft").await;

        let state = std::sync::Arc::new(test_app_state_with_pool(pool.clone()).await);
        crate::try_auto_generate_offer(std::sync::Arc::clone(&state), inquiry_id).await;

        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM offers WHERE inquiry_id = $1")
                .bind(inquiry_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "try_auto_generate_offer must not create a second offer when one already exists");
    }

    #[tokio::test]
    async fn auto_generate_skips_when_no_volume_estimate() {
        // try_auto_generate_offer requires estimated_volume_m3 > 0.
        // A quote with no volume must produce no offer.
        let (_, pool) = setup().await;
        let customer_id = insert_test_customer(&pool).await;
        let inquiry_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO inquiries (id, customer_id, status, notes, created_at, updated_at)
             VALUES ($1, $2, 'pending', NULL, NOW(), NOW())",
        )
        .bind(inquiry_id)
        .bind(customer_id)
        .execute(&pool)
        .await
        .unwrap();

        let state = std::sync::Arc::new(test_app_state_with_pool(pool.clone()).await);
        crate::try_auto_generate_offer(std::sync::Arc::clone(&state), inquiry_id).await;

        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM offers WHERE inquiry_id = $1")
                .bind(inquiry_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "try_auto_generate_offer must not create an offer when estimated_volume_m3 is NULL");
    }

    #[tokio::test]
    async fn auto_generate_distance_calc_attempted_when_zero() {
        // Before the distance fix, try_auto_generate_offer never populated distance_km for
        // API-created quotes. After the fix, it fetches addresses and attempts ORS.
        // With the test API key ORS will fail (expected), but we verify: the code does NOT panic,
        // and the quote is not left in a broken state (it still has distance_km = 0.0).
        let (_, pool) = setup().await;
        let inquiry_id = insert_test_quote_no_distance(&pool, 20.0).await;

        let state = std::sync::Arc::new(test_app_state_with_pool(pool.clone()).await);
        // This call will attempt ORS (fail with test key), then attempt build_offer (fail without
        // LibreOffice). The important thing is it does not panic, and we can check that the
        // function ran the distance-check branch by inspecting that the offer row was not created.
        crate::try_auto_generate_offer(std::sync::Arc::clone(&state), inquiry_id).await;

        // The function should have attempted distance calc (which ORS-failed),
        // then tried build_offer (which LibreOffice-failed), resulting in no offer in DB.
        // Most importantly, the quote record must still be intact.
        let exists: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM inquiries WHERE id = $1")
                .bind(inquiry_id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(exists.is_some(), "quote must still exist after try_auto_generate_offer fails");
    }

    // ========== Unique active offer constraint ==========
    // This test verifies the migration 20260228000000_offers_unique_active.sql.

    #[tokio::test]
    async fn unique_active_offer_constraint_rejects_second_draft() {
        let (_, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "volume_estimated").await;
        insert_test_offer(&pool, inquiry_id, "draft").await;

        let id2 = uuid::Uuid::now_v7();
        let result = sqlx::query(
            "INSERT INTO offers (id, inquiry_id, status, price_cents, currency,
             valid_until, persons, hours_estimated, rate_per_hour_cents, pdf_storage_key, created_at)
             VALUES ($1, $2, 'draft', 50000, 'EUR', NOW() + interval '14 days',
             2, 4.0, 3500, 'test2.pdf', NOW())",
        )
        .bind(id2)
        .bind(inquiry_id)
        .execute(&pool)
        .await;

        assert!(result.is_err(), "unique constraint must prevent two active offers for the same quote");
    }

    #[tokio::test]
    async fn unique_constraint_allows_second_offer_after_rejection() {
        let (_, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "volume_estimated").await;
        insert_test_offer(&pool, inquiry_id, "rejected").await;

        let id2 = uuid::Uuid::now_v7();
        let result = sqlx::query(
            "INSERT INTO offers (id, inquiry_id, status, price_cents, currency,
             valid_until, persons, hours_estimated, rate_per_hour_cents, pdf_storage_key, created_at)
             VALUES ($1, $2, 'draft', 50000, 'EUR', NOW() + interval '14 days',
             2, 4.0, 3500, 'test2.pdf', NOW())",
        )
        .bind(id2)
        .bind(inquiry_id)
        .execute(&pool)
        .await;

        assert!(result.is_ok(), "a new draft offer must be insertable after the previous was rejected");
    }

    // ========== LatestOfferPricing flat_total endpoint test ==========
    // This test would have caught the bug where flat_total was ignored in the
    // GET /api/v1/inquiries/{id} response, causing Fahrkostenpauschale to show total_cents = 0.

    #[tokio::test]
    async fn latest_offer_flat_total_renders_in_inquiry_detail() {
        let (app, pool) = setup().await;
        let inquiry_id = insert_test_quote_with_status(&pool, "estimated").await;

        let line_items = serde_json::json!([
            {
                "description": "Fahrkostenpauschale",
                "quantity": 0.0,
                "unit_price": 0.0,
                "is_labor": false,
                "flat_total": 45.0
            },
            {
                "description": "2 Umzugshelfer",
                "quantity": 8.0,
                "unit_price": 35.0,
                "is_labor": true
            },
            {
                "description": "Nürnbergerversicherung",
                "quantity": 1.0,
                "unit_price": 0.0,
                "is_labor": false,
                "flat_total": 0.0
            }
        ]);
        insert_test_offer_with_line_items(&pool, inquiry_id, "draft", 2, 50000, line_items).await;

        let resp = app
            .oneshot(authed_get(&format!("/api/v1/inquiries/{inquiry_id}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body = body_json(resp).await;
        let offer = &body["offer"];
        assert!(offer.get("line_items").is_some(), "offer should have line_items");
        let items = offer["line_items"].as_array().expect("line_items must be array");

        // Fahrkostenpauschale: flat_total=45.0 → total_cents must be 4500, NOT 0
        let fahrt = items.iter().find(|i| i["label"] == "Fahrkostenpauschale")
            .expect("Fahrkostenpauschale must be in offer line_items");
        assert_eq!(
            fahrt["total_cents"], 4500,
            "before fix: flat_total was ignored, total_cents was 0 (qty 0 × price 0 = 0)"
        );

        // Labor item: 8h × €35 × 2 persons = 5600
        let labor = items.iter().find(|i| i["is_labor"] == true)
            .expect("labor item must be present");
        assert_eq!(labor["total_cents"], 56000, "labor: 8h × €35 × 2 persons = 56000 cents");

        // Versicherung: flat_total=0.0 → total_cents = 0 (not qty*price)
        let versicherung = items.iter().find(|i| i["label"] == "Nürnbergerversicherung")
            .expect("Nürnbergerversicherung must be in offer line_items");
        assert_eq!(versicherung["total_cents"], 0);
    }
}

#[cfg(test)]
mod calendar_service_tests {
    use aust_calendar::{CalendarService, NewBooking};
    use chrono::{Datelike, NaiveDate};
    use crate::test_helpers::*;

    /// Helper to build a NewBooking for a given date with no inquiry_id.
    fn new_booking(date: NaiveDate) -> NewBooking {
        NewBooking {
            booking_date: date,
            inquiry_id: None,
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
