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

    // Aust's sites, local dev and the app — plus every host listed in a tenant's
    // `domains` (registered at startup), so other companies' sites can call too.
    const FIXED_ORIGINS: [&str; 6] = [
        "https://www.aust-umzuege.de",
        "https://aust-umzuege.de",
        "http://localhost:5173",
        "http://localhost:4173",
        "capacitor://localhost",
        "http://localhost",
    ];
    let allow_origin = tower_http::cors::AllowOrigin::predicate(|origin: &HeaderValue, _| {
        let Ok(origin) = origin.to_str() else { return false };
        FIXED_ORIGINS.contains(&origin)
            || (origin.starts_with("https://") && aust_core::tenant::by_origin(origin).is_some())
    });

    let cors = CorsLayer::new()
        .allow_origin(allow_origin)
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
        .nest("/admin/profit", routes::profit::router())
        .nest("/admin/tenant", routes::tenant::admin_router())
        .nest("/platform", routes::platform::router())
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

    // Submissions get their own, looser bucket: five per IP per hour. A customer
    // submits one inquiry; anything past that is someone filling the dashboard with
    // junk and running up the vision and LLM bill.
    let submit_limiter = Arc::new(middleware::RateLimiter::new(5, Duration::from_secs(3600)));
    let submit_routes = routes::submit_api_router().layer(axum::middleware::from_fn(
        move |req: Request, next: Next| {
            let limiter = submit_limiter.clone();
            async move { middleware::apply_rate_limit(limiter, req, next).await }
        },
    ));

    Router::new()
        .merge(routes::health::router())
        .nest(
            "/api/v1",
            routes::public_api_router()
                .merge(submit_routes)
                .merge(auth_routes)
                .merge(protected_api)
                .merge(admin_routes)
                .merge(customer_routes)
                .merge(employee_routes)
                .layer(axum::extract::DefaultBodyLimit::max(250 * 1024 * 1024)),
        )
        // Layer order (outermost → innermost): cors → security_headers → request_id →
        // trace → tenant by origin (authenticated routes scope again inside)
        .layer(axum::middleware::from_fn(middleware::scope_by_origin))
        .layer(axum::middleware::from_fn(middleware::set_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(middleware::set_security_headers))
        .layer(cors)
        .with_state(shared_state)
}

/// Open the application pool.
///
/// Every connection handed out carries the caller's tenant in the session setting
/// `app.tenant_id` (see `aust_core::tenant`): set when a connection is opened and
/// again each time an idle one is reused, so a connection never keeps the previous
/// task's tenant.
pub async fn create_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    tenant_aware(sqlx::postgres::PgPoolOptions::new().max_connections(max_connections))
        .connect(database_url)
        .await
}

/// Pool options whose connections carry the caller's tenant (see [`create_pool`]).
/// Tests that depend on the tenant build their pool with this too.
pub fn tenant_aware(options: sqlx::postgres::PgPoolOptions) -> sqlx::postgres::PgPoolOptions {
    options
        .after_connect(|conn, _meta| Box::pin(async move { set_session_tenant(conn).await }))
        .before_acquire(|conn, _meta| {
            Box::pin(async move { set_session_tenant(conn).await.map(|()| true) })
        })
}

async fn set_session_tenant(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, false)")
        .bind(aust_core::tenant::session_value())
        .execute(conn)
        .await
        .map(|_| ())
}



#[cfg(test)]
mod rls_tests;

#[cfg(test)]
mod tenant_pool_tests {
    use aust_core::tenant::{self, TenantId, AUST};

    async fn setting(pool: &sqlx::PgPool) -> (String, uuid::Uuid) {
        sqlx::query_as("SELECT current_setting('app.tenant_id', true), current_tenant_id()")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// One connection, reused across three tasks: each sees its own tenant, never the
    /// previous task's.
    #[tokio::test]
    async fn every_acquire_carries_the_callers_tenant() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://aust:aust_dev_password@localhost/aust_backend_test".into());
        let pool = crate::create_pool(&url, 1).await.unwrap();
        crate::test_helpers::test_db_pool().await; // migrations → current_tenant_id()

        let other = TenantId(uuid::Uuid::now_v7());
        let fresh = tenant::scope(other, setting(&pool)).await;
        assert_eq!(fresh, (other.0.to_string(), other.0));

        let reused = tenant::scope(AUST, setting(&pool)).await;
        assert_eq!(reused, (AUST.0.to_string(), AUST.0));

        let mut tx = tenant::scope(other, pool.begin()).await.unwrap();
        let in_tx: (uuid::Uuid,) = sqlx::query_as("SELECT current_tenant_id()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(in_tx.0, other.0, "a transaction keeps the tenant it was opened for");
        tx.rollback().await.unwrap();

        // Outside any scope: unset, and the database falls back to Aust.
        assert_eq!(setting(&pool).await, (String::new(), AUST.0));
    }
}
