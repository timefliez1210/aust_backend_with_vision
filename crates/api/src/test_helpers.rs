#![cfg(test)]

use aust_calendar::CalendarService;
use aust_core::config::*;
use aust_core::Config;
use aust_llm_providers::MockLlmProvider;
use aust_storage::LocalStorage;
use aust_volume_estimator::VisionServiceClient;
use sqlx::PgPool;
use std::sync::Arc;

use crate::AppState;

/// Creates a test database pool and runs all migrations.
pub async fn test_db_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://aust:aust_dev_password@localhost/aust_backend_test".into());
    let pool = PgPool::connect(&url)
        .await
        .expect("Failed to connect to test database. Make sure aust_backend_test exists.");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations on test database");
    pool
}

/// Creates a test Config with sensible defaults for testing.
pub fn test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            url: "postgres://aust:aust_dev_password@localhost/aust_backend_test".to_string(),
            max_connections: 5,
        },
        storage: StorageConfig {
            provider: "local".to_string(),
            bucket: "/tmp/aust-test-uploads".to_string(),
            endpoint: None,
            region: None,
        },
        email: EmailConfig {
            imap_host: "localhost".to_string(),
            imap_port: 993,
            smtp_host: "localhost".to_string(),
            smtp_port: 587,
            username: "test@test.com".to_string(),
            password: "test".to_string(),
            poll_interval_secs: 60,
            from_address: "test@aust-umzuege.de".to_string(),
            from_name: "AUST Test".to_string(),
        },
        llm: LlmConfig {
            default_provider: "mock".to_string(),
            claude: None,
            openai: None,
            ollama: None,
        },
        maps: MapsConfig {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
        },
        telegram: TelegramConfig {
            bot_token: "test-bot-token".to_string(),
            admin_chat_id: 0,
        },
        auth: AuthConfig {
            jwt_secret: "test-jwt-secret-that-is-long-enough-for-validation".to_string(),
            jwt_expiry_hours: 24,
        },
        calendar: CalendarConfig::default(),
        vision_service: VisionServiceConfig::default(),
    }
}

/// Creates a test AppState with mock providers.
pub async fn test_app_state() -> AppState {
    let pool = test_db_pool().await;
    test_app_state_with_pool(pool).await
}

/// Creates a test AppState with a specific database pool.
pub async fn test_app_state_with_pool(pool: PgPool) -> AppState {
    let config = test_config();
    let llm: Arc<dyn aust_llm_providers::LlmProvider> =
        Arc::new(MockLlmProvider::new("{}"));
    let storage: Arc<dyn aust_storage::StorageProvider> =
        Arc::new(LocalStorage::new("/tmp/aust-test-uploads").expect("create test storage"));
    let calendar = Arc::new(CalendarService::new(
        pool.clone(),
        config.calendar.default_capacity,
        config.calendar.alternatives_count,
        config.calendar.search_window_days,
    ));

    AppState::new(config, pool, llm, storage, calendar, None)
}

/// Creates a test AppState with a mock vision service at the given URL.
pub async fn test_app_state_with_vision(pool: PgPool, vision_url: &str) -> AppState {
    let config = test_config();
    let llm: Arc<dyn aust_llm_providers::LlmProvider> =
        Arc::new(MockLlmProvider::new("{}"));
    let storage: Arc<dyn aust_storage::StorageProvider> =
        Arc::new(LocalStorage::new("/tmp/aust-test-uploads").expect("create test storage"));
    let calendar = Arc::new(CalendarService::new(
        pool.clone(),
        config.calendar.default_capacity,
        config.calendar.alternatives_count,
        config.calendar.search_window_days,
    ));
    let vision = VisionServiceClient::new(vision_url, Some(vision_url), 30, 0)
        .expect("create test vision client");

    AppState::new(config, pool, llm, storage, calendar, Some(vision))
}

/// Generate a valid JWT token for testing using the test config's secret.
pub fn generate_test_jwt() -> String {
    use aust_core::models::TokenClaims;
    use chrono::Utc;
    use jsonwebtoken::{encode, EncodingKey, Header};

    let claims = TokenClaims {
        sub: uuid::Uuid::new_v4(),
        email: "test@aust-umzuege.de".to_string(),
        role: aust_core::models::UserRole::Admin,
        exp: (Utc::now().timestamp() + 86400) as usize,
        iat: Utc::now().timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(
            "test-jwt-secret-that-is-long-enough-for-validation".as_bytes(),
        ),
    )
    .expect("Failed to create test JWT")
}

/// Insert a test customer and return its ID.
pub async fn insert_test_customer(pool: &PgPool) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let email = format!("test-{}@example.com", id);
    sqlx::query(
        "INSERT INTO customers (id, name, email, phone, created_at, updated_at)
         VALUES ($1, 'Test Kunde', $2, '+4915112345678', NOW(), NOW())",
    )
    .bind(id)
    .bind(&email)
    .execute(pool)
    .await
    .expect("insert test customer");
    id
}

/// Insert a test address and return its ID.
pub async fn insert_test_address(
    pool: &PgPool,
    street: &str,
    city: &str,
    postal_code: &str,
    floor: Option<i32>,
    elevator: Option<bool>,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO addresses (id, street, city, postal_code, country, floor, elevator, created_at)
         VALUES ($1, $2, $3, $4, 'Deutschland', $5, $6, NOW())",
    )
    .bind(id)
    .bind(street)
    .bind(city)
    .bind(postal_code)
    .bind(floor)
    .bind(elevator)
    .execute(pool)
    .await
    .expect("insert test address");
    id
}

/// Insert a test quote with default status 'pending' and return its ID.
pub async fn insert_test_quote(pool: &PgPool) -> uuid::Uuid {
    insert_test_quote_with_status(pool, "pending").await
}

/// Insert a test quote with specified status and return its ID.
pub async fn insert_test_quote_with_status(pool: &PgPool, status: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let customer_id = insert_test_customer(pool).await;
    let origin_id =
        insert_test_address(pool, "Musterstr. 1", "Hildesheim", "31134", Some(2), Some(true))
            .await;
    let dest_id =
        insert_test_address(pool, "Zielstr. 5", "Hannover", "30159", Some(0), Some(false)).await;

    sqlx::query(
        "INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id,
         status, preferred_date, estimated_volume_m3, notes, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, '2026-04-01', 20.0, 'Halteverbot Auszug', NOW(), NOW())",
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test quote");
    id
}

/// Insert a test offer and return its ID.
pub async fn insert_test_offer(
    pool: &PgPool,
    quote_id: uuid::Uuid,
    status: &str,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO offers (id, quote_id, status, price_cents, currency,
         valid_until, persons, hours_estimated, rate_per_hour_cents, pdf_storage_key, created_at)
         VALUES ($1, $2, $3, 50000, 'EUR', NOW() + interval '14 days',
         2, 4.0, 3500, 'test.pdf', NOW())",
    )
    .bind(id)
    .bind(quote_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test offer");
    id
}

/// Insert a test booking and return its ID.
/// Note: calendar_bookings has a unique partial index on (quote_id) WHERE status != 'cancelled',
/// so only ONE active booking per quote is allowed.
pub async fn insert_test_booking(
    pool: &PgPool,
    quote_id: uuid::Uuid,
    status: &str,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO calendar_bookings (id, booking_date, quote_id, customer_name,
         customer_email, status, created_at, updated_at)
         VALUES ($1, '2026-04-01', $2, 'Test Kunde', 'test@example.com', $3, NOW(), NOW())",
    )
    .bind(id)
    .bind(quote_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test booking");
    id
}

/// Insert a test volume estimation and return its ID.
pub async fn insert_test_estimation(
    pool: &PgPool,
    quote_id: uuid::Uuid,
    method: &str,
    volume: f64,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO volume_estimations (id, quote_id, method, total_volume_m3,
         confidence_score, result_data, created_at)
         VALUES ($1, $2, $3, $4, 0.85, '{\"items\": []}', NOW())",
    )
    .bind(id)
    .bind(quote_id)
    .bind(method)
    .bind(volume)
    .execute(pool)
    .await
    .expect("insert test estimation");
    id
}

/// Helper to get a quote's status from DB.
pub async fn get_quote_status(pool: &PgPool, quote_id: uuid::Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM quotes WHERE id = $1")
        .bind(quote_id)
        .fetch_one(pool)
        .await
        .expect("get quote status");
    row.0
}

/// Helper to get an offer's status from DB.
pub async fn get_offer_status(pool: &PgPool, offer_id: uuid::Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM offers WHERE id = $1")
        .bind(offer_id)
        .fetch_one(pool)
        .await
        .expect("get offer status");
    row.0
}

/// Helper to get a booking's status from DB.
pub async fn get_booking_status(pool: &PgPool, booking_id: uuid::Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM calendar_bookings WHERE id = $1")
        .bind(booking_id)
        .fetch_one(pool)
        .await
        .expect("get booking status");
    row.0
}

/// Clean up all test data. Call at the start of tests for isolation.
pub async fn clean_test_data(pool: &PgPool) {
    // Delete in order respecting foreign keys
    for table in &[
        "calendar_bookings",
        "volume_estimations",
        "offers",
        "quotes",
        "addresses",
        "customers",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(pool)
            .await
            .ok();
    }
}
