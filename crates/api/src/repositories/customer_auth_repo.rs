//! Customer auth repository — centralised queries for `customer_otps` and `customer_sessions` tables.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Count recent OTP requests for rate limiting.
///
/// **Caller**: `customer::request_otp`
/// **Why**: Enforces max 3 OTPs per email in 10 minutes.
pub(crate) async fn count_recent_otps(
    pool: &PgPool,
    email: &str,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM customer_otps WHERE email = $1 AND created_at > NOW() - INTERVAL '10 minutes'",
    )
    .bind(email)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Insert a new OTP code for a customer.
///
/// **Caller**: `customer::request_otp`
/// **Why**: Persists the generated 6-digit code with expiration.
pub(crate) async fn insert_otp(
    pool: &PgPool,
    email: &str,
    code: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO customer_otps (email, code, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(email)
    .bind(code)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Find a valid (unused, non-expired) OTP by email and code.
///
/// **Caller**: `customer::verify_otp`
/// **Why**: Validates the OTP code during verification.
///
/// # Returns
/// The OTP row ID if found, `None` otherwise.
pub(crate) async fn find_valid_otp(
    pool: &PgPool,
    email: &str,
    code: &str,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT id FROM customer_otps
        WHERE email = $1 AND code = $2 AND used = FALSE AND expires_at > $3
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(email)
    .bind(code)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Mark an OTP as used.
///
/// **Caller**: `customer::verify_otp`
/// **Why**: Prevents OTP reuse.
pub(crate) async fn mark_otp_used(
    pool: &PgPool,
    otp_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE customer_otps SET used = TRUE WHERE id = $1")
        .bind(otp_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Upsert a customer by email (minimal — only sets updated_at on conflict) and return all fields.
///
/// **Caller**: `customer::verify_otp`
/// **Why**: Creates or touches the customer record during OTP verification.
///
/// # Returns
/// Tuple of (id, email, name, salutation, first_name, last_name, phone).
pub(crate) async fn upsert_customer_minimal(
    pool: &PgPool,
    email: &str,
    now: DateTime<Utc>,
) -> Result<(Uuid, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>), sqlx::Error> {
    sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, created_at, updated_at)
        VALUES ($1, $2, $3, $3)
        ON CONFLICT (email) DO UPDATE SET updated_at = $3
        RETURNING id, email, name, salutation, first_name, last_name, phone
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Create a new customer session token.
///
/// **Caller**: `customer::verify_otp`
/// **Why**: Persists the session for the authenticated customer.
pub(crate) async fn create_session(
    pool: &PgPool,
    customer_id: Uuid,
    token: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO customer_sessions (customer_id, token, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(customer_id)
    .bind(token)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a customer profile by ID (for the /me endpoint).
///
/// **Caller**: `customer::get_profile`
/// **Why**: Returns all customer fields needed for the profile response.
pub(crate) async fn fetch_customer_profile(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Option<(Uuid, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)>, sqlx::Error> {
    sqlx::query_as("SELECT id, email, name, salutation, first_name, last_name, phone FROM customers WHERE id = $1")
        .bind(customer_id)
        .fetch_optional(pool)
        .await
}

/// List customer inquiries with latest offer price.
///
/// **Caller**: `customer::list_inquiries`
/// **Why**: Returns inquiry summary with joined address cities and subquery offer price.
pub(crate) async fn list_customer_inquiries(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Vec<(Uuid, String, Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>, Option<String>, Option<String>, Option<f64>, Option<i64>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            q.id, q.status, q.preferred_date, q.created_at,
            oa.city AS origin_city,
            da.city AS destination_city,
            q.estimated_volume_m3,
            (SELECT o.price_cents FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1)
        FROM inquiries q
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE q.customer_id = $1
        ORDER BY q.created_at DESC
        "#,
    )
    .bind(customer_id)
    .fetch_all(pool)
    .await
}

/// Fetch inquiry detail with ownership check.
///
/// **Caller**: `customer::get_inquiry_detail`
/// **Why**: Validates that the inquiry belongs to the authenticated customer.
pub(crate) async fn fetch_inquiry_owned(
    pool: &PgPool,
    inquiry_id: Uuid,
    customer_id: Uuid,
) -> Result<Option<(Uuid, String, Option<f64>, Option<f64>, Option<chrono::DateTime<Utc>>, Option<Uuid>, Option<Uuid>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, status, estimated_volume_m3, distance_km, preferred_date,
               origin_address_id, destination_address_id
        FROM inquiries
        WHERE id = $1 AND customer_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .fetch_optional(pool)
    .await
}

/// Fetch address info for customer display (COALESCE for empty strings).
///
/// **Caller**: `customer::get_inquiry_detail`
/// **Why**: Customer-facing address display needs guaranteed non-null fields.
pub(crate) async fn fetch_address_display(
    pool: &PgPool,
    address_id: Uuid,
) -> Result<Option<(String, String, String, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT COALESCE(street, ''), COALESCE(city, ''), COALESCE(postal_code, ''), floor FROM addresses WHERE id = $1",
    )
    .bind(address_id)
    .fetch_optional(pool)
    .await
}

/// Fetch latest estimation for customer display.
///
/// **Caller**: `customer::fetch_estimation`
/// **Why**: Returns volume, confidence, and result_data for item parsing.
pub(crate) async fn fetch_latest_estimation(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(f64, f64, Option<serde_json::Value>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT total_volume_m3, confidence_score, result_data
        FROM volume_estimations
        WHERE inquiry_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Fetch offers for an inquiry (customer view).
///
/// **Caller**: `customer::get_inquiry_detail`
/// **Why**: Lists all offers for the inquiry with pricing and status info.
pub(crate) async fn fetch_inquiry_offers(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<(Uuid, i64, String, Option<chrono::NaiveDate>, Option<i32>, Option<f64>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, price_cents, status, valid_until, persons, hours_estimated
        FROM offers
        WHERE inquiry_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Validate inquiry ownership and return inquiry_id + customer display name.
///
/// **Caller**: `customer::accept_inquiry`, `customer::reject_inquiry`
/// **Why**: Ownership validation for accept/reject actions.
pub(crate) async fn validate_inquiry_ownership(
    pool: &PgPool,
    inquiry_id: Uuid,
    customer_id: Uuid,
) -> Result<Option<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT q.id, COALESCE(c.name, c.email)
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.id = $1 AND q.customer_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .fetch_optional(pool)
    .await
}

/// Fetch active offer (non-rejected, non-cancelled) with status.
///
/// **Caller**: `customer::accept_inquiry`, `customer::reject_inquiry`
/// **Why**: Finds the offer to accept/reject.
pub(crate) async fn fetch_active_offer_with_status(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, status
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Update offer status to a given value.
///
/// **Caller**: `customer::accept_inquiry`, `customer::reject_inquiry`
/// **Why**: Sets offer status on accept/reject.
pub(crate) async fn update_offer_status(
    pool: &PgPool,
    offer_id: Uuid,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&format!("UPDATE offers SET status = '{status}' WHERE id = $1"))
        .bind(offer_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Check inquiry ownership (exists check only).
///
/// **Caller**: `customer::download_inquiry_pdf`
/// **Why**: Validates that the inquiry belongs to the customer before PDF download.
pub(crate) async fn check_inquiry_ownership(
    pool: &PgPool,
    inquiry_id: Uuid,
    customer_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM inquiries WHERE id = $1 AND customer_id = $2",
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}
