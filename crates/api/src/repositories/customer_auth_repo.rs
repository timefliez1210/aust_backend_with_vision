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
    code_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO customer_otps (email, code, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(email)
    .bind(code_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Every code still live for this address: id and Argon2 hash, newest first.
///
/// **Caller**: `otp_service::handle_verify_otp`, which verifies the submitted code
/// against each hash.
/// **Why**: The code used to be stored and matched in plaintext, so a read-only replica,
/// a backup or a dump handed out working login codes. It is hashed now, and a salted
/// hash cannot be looked up by equality — the candidates come back and the service
/// checks them.
///
/// `attempts` is bounded by `MAX_OTP_ATTEMPTS` and issuance is rate-limited, so this is
/// a handful of rows at most.
pub(crate) async fn find_live_otps(
    pool: &PgPool,
    email: &str,
    now: DateTime<Utc>,
) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, code FROM customer_otps
        WHERE email = $1 AND used = FALSE AND expires_at > $2
          AND attempts < $3
        ORDER BY created_at DESC
        "#,
    )
    .bind(email)
    .bind(now)
    .bind(crate::services::otp_service::MAX_OTP_ATTEMPTS)
    .fetch_all(pool)
    .await
}

/// Count one failed guess against every code currently live for this address.
///
/// **Caller**: `customer::verify_otp`, on every rejected code.
/// **Why**: Without a counter the six-digit space is grindable inside the code's own
/// ten-minute window. The count sits on the code, so the lockout dies with it and
/// nobody can be locked out for longer than their own code would have lasted.
///
/// Returns the number of live codes that were counted against.
pub(crate) async fn record_failed_otp_attempt(
    pool: &PgPool,
    email: &str,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE customer_otps SET attempts = attempts + 1
          WHERE email = $1 AND used = FALSE AND expires_at > NOW()",
    )
    .bind(email)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
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
) -> Result<Vec<(Uuid, String, Option<chrono::NaiveDate>, chrono::DateTime<Utc>, Option<String>, Option<String>, Option<f64>, Option<i64>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            q.id, q.status, q.scheduled_date, q.created_at,
            oa.city AS origin_city,
            da.city AS destination_city,
            q.estimated_volume_m3,
            (SELECT o.price_cents FROM offers o WHERE o.inquiry_id = q.id AND o.status NOT IN ('rejected', 'cancelled', 'superseded') ORDER BY o.created_at DESC LIMIT 1)
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
) -> Result<Option<(Uuid, String, Option<f64>, Option<f64>, Option<chrono::NaiveDate>, Option<Uuid>, Option<Uuid>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, status, estimated_volume_m3, distance_km, scheduled_date,
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
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled', 'superseded')
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
    sqlx::query("UPDATE offers SET status = $2 WHERE id = $1")
        .bind(offer_id)
        .bind(status)
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


/// Revoke one session by its token — "abmelden" on this device.
///
/// **Caller**: `customer::logout`
/// **Why**: Sessions live 30 days and nothing ever deleted one, so a token taken from a
/// lost or shared phone stayed valid for a month with no way to cut it off.
pub(crate) async fn delete_session(pool: &PgPool, token: &str) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM customer_sessions WHERE token = $1")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Revoke every session for one account — "überall abmelden".
///
/// **Caller**: `customer::logout_everywhere`
pub(crate) async fn delete_all_sessions(pool: &PgPool, customer_id: Uuid) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM customer_sessions WHERE customer_id = $1")
        .bind(customer_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::otp_service::{hash_otp, MAX_OTP_ATTEMPTS};

    /// Five wrong guesses kill the code. Without this the six-digit space is grindable
    /// inside the ten minutes the code lives, and success hands out a 30-day session
    /// carrying the customer's inquiries, addresses and offer PDFs.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_code_dies_after_too_many_wrong_guesses(pool: PgPool) {
        let email = "kundin@example.de";
        let expires = Utc::now() + chrono::Duration::minutes(10);
        let hash = hash_otp("123456").expect("hash");
        insert_otp(&pool, email, &hash, expires).await.expect("insert otp");

        // The right code works while the budget lasts.
        for _ in 0..MAX_OTP_ATTEMPTS {
            assert_eq!(
                find_live_otps(&pool, email, Utc::now()).await.expect("lookup").len(),
                1,
                "the real code must stay a candidate below the limit"
            );
            record_failed_otp_attempt(&pool, email).await.expect("record miss");
        }

        // One guess past the limit and even the right code is dead.
        assert!(
            find_live_otps(&pool, email, Utc::now()).await.expect("lookup").is_empty(),
            "the code must stop being a candidate once the attempt budget is spent"
        );
    }

    /// The lockout is per code, so requesting a new one restores access — an attacker
    /// grinding an address cannot lock its owner out for longer than a code's own life.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_fresh_code_is_not_affected_by_the_old_ones_misses(pool: PgPool) {
        let email = "kundin@example.de";
        let expires = Utc::now() + chrono::Duration::minutes(10);
        let first = hash_otp("111111").expect("hash");
        insert_otp(&pool, email, &first, expires).await.expect("insert first");
        for _ in 0..MAX_OTP_ATTEMPTS {
            record_failed_otp_attempt(&pool, email).await.expect("record miss");
        }

        let second = hash_otp("222222").expect("hash");
        insert_otp(&pool, email, &second, expires).await.expect("insert second");
        assert_eq!(
            find_live_otps(&pool, email, Utc::now()).await.expect("lookup").len(),
            1,
            "a newly requested code must be a candidate again"
        );
    }

    /// The column must never hold a code anyone could read back out. A replica, a
    /// nightly backup or a dump used to be a list of working login codes.
    #[sqlx::test(migrations = "../../migrations")]
    async fn the_stored_code_is_a_hash_not_the_code(pool: PgPool) {
        let email = "kundin@example.de";
        let hash = hash_otp("424242").expect("hash");
        insert_otp(&pool, email, &hash, Utc::now() + chrono::Duration::minutes(10))
            .await
            .expect("insert otp");

        let (stored,): (String,) =
            sqlx::query_as("SELECT code FROM customer_otps WHERE email = $1")
                .bind(email)
                .fetch_one(&pool)
                .await
                .expect("read back");

        assert!(!stored.contains("424242"), "the plaintext code reached the database");
        assert!(stored.starts_with("$argon2"), "expected an Argon2 hash, got {stored}");
    }
}
