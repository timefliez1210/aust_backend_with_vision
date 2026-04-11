#![allow(dead_code)]

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
        company: CompanyConfig::default(),
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

    AppState::new(config, pool, llm, storage, None)
}

/// Creates a test AppState with a mock vision service at the given URL.
pub async fn test_app_state_with_vision(pool: PgPool, vision_url: &str) -> AppState {
    let config = test_config();
    let llm: Arc<dyn aust_llm_providers::LlmProvider> =
        Arc::new(MockLlmProvider::new("{}"));
    let storage: Arc<dyn aust_storage::StorageProvider> =
        Arc::new(LocalStorage::new("/tmp/aust-test-uploads").expect("create test storage"));
    let vision = VisionServiceClient::new(vision_url, Some(vision_url), Some(vision_url), 30, 0)
        .expect("create test vision client");

    AppState::new(config, pool, llm, storage, Some(vision))
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

/// Insert a test inquiry with default status 'pending' and return its ID.
pub async fn insert_test_quote(pool: &PgPool) -> uuid::Uuid {
    insert_test_quote_with_status(pool, "pending").await
}

/// Insert a test inquiry with specified status and return its ID.
pub async fn insert_test_quote_with_status(pool: &PgPool, status: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let customer_id = insert_test_customer(pool).await;
    let origin_id =
        insert_test_address(pool, "Musterstr. 1", "Hildesheim", "31134", Some(2), Some(true))
            .await;
    let dest_id =
        insert_test_address(pool, "Zielstr. 5", "Hannover", "30159", Some(0), Some(false)).await;

    sqlx::query(
        "INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
         status, scheduled_date, estimated_volume_m3, notes, services, source, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, '2026-04-01', 20.0, 'Halteverbot Auszug', '{}', 'direct_email', NOW(), NOW())",
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test inquiry");
    id
}

/// Insert a test offer and return its ID.
pub async fn insert_test_offer(
    pool: &PgPool,
    inquiry_id: uuid::Uuid,
    status: &str,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO offers (id, inquiry_id, status, price_cents, currency,
         valid_until, persons, hours_estimated, rate_per_hour_cents, pdf_storage_key, created_at)
         VALUES ($1, $2, $3, 50000, 'EUR', NOW() + interval '14 days',
         2, 4.0, 3500, 'test.pdf', NOW())",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert test offer");
    id
}

/// Insert a test offer with a specific line_items_json and persons count.
/// Used to test LatestOfferPricing endpoint rendering (flat_total, labor, regular items).
pub async fn insert_test_offer_with_line_items(
    pool: &PgPool,
    inquiry_id: uuid::Uuid,
    status: &str,
    persons: i32,
    price_cents: i64,
    line_items: serde_json::Value,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO offers (id, inquiry_id, status, price_cents, currency,
         valid_until, persons, hours_estimated, rate_per_hour_cents, pdf_storage_key,
         line_items_json, created_at)
         VALUES ($1, $2, $3, $4, 'EUR', NOW() + interval '14 days',
         $5, 8.0, 3500, 'test.pdf', $6, NOW())",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(status)
    .bind(price_cents)
    .bind(persons)
    .bind(line_items)
    .execute(pool)
    .await
    .expect("insert test offer with line items");
    id
}

/// Insert a test inquiry with addresses but with distance_km = 0 (the pre-calculation state).
/// Useful for testing distance auto-calculation logic.
pub async fn insert_test_quote_no_distance(pool: &PgPool, volume_m3: f64) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let customer_id = insert_test_customer(pool).await;
    let origin_id =
        insert_test_address(pool, "Musterstr. 1", "Hildesheim", "31134", Some(0), None).await;
    let dest_id =
        insert_test_address(pool, "Zielstr. 5", "Hannover", "30159", Some(0), None).await;

    sqlx::query(
        "INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
         status, estimated_volume_m3, distance_km, notes, services, source, created_at, updated_at)
         VALUES ($1, $2, $3, $4, 'estimated', $5, 0.0, NULL, '{}', 'direct_email', NOW(), NOW())",
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(volume_m3)
    .execute(pool)
    .await
    .expect("insert test inquiry no distance");
    id
}

/// Insert a test volume estimation and return its ID.
pub async fn insert_test_estimation(
    pool: &PgPool,
    inquiry_id: uuid::Uuid,
    method: &str,
    volume: f64,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO volume_estimations (id, inquiry_id, method, total_volume_m3,
         confidence_score, result_data, created_at)
         VALUES ($1, $2, $3, $4, 0.85, '{\"items\": []}', NOW())",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(method)
    .bind(volume)
    .execute(pool)
    .await
    .expect("insert test estimation");
    id
}

/// Helper to get an inquiry's status from DB.
pub async fn get_quote_status(pool: &PgPool, inquiry_id: uuid::Uuid) -> String {
    let row: (String,) = sqlx::query_as("SELECT status FROM inquiries WHERE id = $1")
        .bind(inquiry_id)
        .fetch_one(pool)
        .await
        .expect("get inquiry status");
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

/// Clean up all test data. Call at the start of tests for isolation.
pub async fn clean_test_data(pool: &PgPool) {
    // Delete in order respecting foreign keys
    for table in &[
        "volume_estimations",
        "offers",
        "inquiry_day_employees",
        "inquiry_days",
        "inquiry_employees",
        "inquiries",
        "addresses",
        "customers",
        "employees",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(pool)
            .await
            .ok();
    }
}

/// Insert a test employee and return its ID.
pub async fn insert_test_employee(pool: &PgPool, first_name: &str, last_name: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO employees (id, first_name, last_name, email, monthly_hours_target, active, created_at, updated_at)
         VALUES ($1, $2, $3, $4, 160.0, true, NOW(), NOW())",
    )
    .bind(id)
    .bind(first_name)
    .bind(last_name)
    .bind(format!("{}.{}@test.de", first_name.to_lowercase(), last_name.to_lowercase()))
    .execute(pool)
    .await
    .expect("insert test employee");
    id
}

/// Insert an inquiry day and return the day ID.
pub async fn insert_test_inquiry_day(
    pool: &PgPool,
    inquiry_id: uuid::Uuid,
    day_number: i16,
    day_date: chrono::NaiveDate,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO inquiry_days (id, inquiry_id, day_date, day_number, start_time, end_time)
         VALUES ($1, $2, $3, $4, '08:00'::time, '17:00'::time)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(day_date)
    .bind(day_number)
    .execute(pool)
    .await
    .expect("insert test inquiry_day");
    id
}

/// Insert a day-employee assignment and return the assignment ID.
pub async fn insert_test_day_employee(
    pool: &PgPool,
    inquiry_day_id: uuid::Uuid,
    employee_id: uuid::Uuid,
    planned_hours: f64,
) {
    sqlx::query(
        "INSERT INTO inquiry_day_employees (inquiry_day_id, employee_id, planned_hours)
         VALUES ($1, $2, $3)",
    )
    .bind(inquiry_day_id)
    .bind(employee_id)
    .bind(planned_hours)
    .execute(pool)
    .await
    .expect("insert test day_employee");
}

/// Insert a test customer with a specific customer_type and return its ID.
pub async fn insert_test_customer_with_type(pool: &PgPool, customer_type: &str, company_name: Option<&str>) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let email = format!("test-{}@example.com", id);
    sqlx::query(
        "INSERT INTO customers (id, name, email, phone, customer_type, company_name, created_at, updated_at)
         VALUES ($1, 'Test Kunde', $2, '+4915112345678', $3, $4, NOW(), NOW())",
    )
    .bind(id)
    .bind(&email)
    .bind(customer_type)
    .bind(company_name)
    .execute(pool)
    .await
    .expect("insert test customer");
    id
}

/// Insert a test address with parking_ban and return its ID.
pub async fn insert_test_address_full(
    pool: &PgPool,
    street: &str,
    house_number: Option<&str>,
    city: &str,
    postal_code: &str,
    floor: Option<i32>,
    elevator: Option<bool>,
    parking_ban: Option<bool>,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO addresses (id, street, house_number, city, postal_code, country, floor, elevator, parking_ban, created_at)
         VALUES ($1, $2, $3, $4, $5, 'Deutschland', $6, $7, $8, NOW())",
    )
    .bind(id)
    .bind(street)
    .bind(house_number)
    .bind(city)
    .bind(postal_code)
    .bind(floor)
    .bind(elevator)
    .bind(parking_ban)
    .execute(pool)
    .await
    .expect("insert test address");
    id
}

/// Insert a test inquiry with full fields and return its ID.
pub async fn insert_test_inquiry_full(
    pool: &PgPool,
    customer_id: uuid::Uuid,
    origin_id: uuid::Uuid,
    dest_id: uuid::Uuid,
    status: &str,
    submission_mode: &str,
    service_type: Option<&str>,
) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id,
         status, submission_mode, service_type, notes, services, source, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'test notes', '{}', 'test', NOW(), NOW())",
    )
    .bind(id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(status)
    .bind(submission_mode)
    .bind(service_type)
    .execute(pool)
    .await
    .expect("insert test inquiry");
    id
}
