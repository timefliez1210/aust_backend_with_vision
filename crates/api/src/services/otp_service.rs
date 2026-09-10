//! Shared OTP authentication service — generic request/verify logic used by both
//! customer and employee auth flows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;

// ---------------------------------------------------------------------------
// Shared request/response types
// ---------------------------------------------------------------------------

/// Incoming OTP request body (email only).
///
/// **Caller**: `customer::request_otp`, `employee::request_otp`
/// **Why**: Both auth flows accept the same input shape.
#[derive(Debug, Deserialize)]
pub(crate) struct OtpRequest {
    pub email: String,
}

/// Generic OTP response with a user-facing message.
///
/// **Caller**: `customer::request_otp`, `employee::request_otp`
/// **Why**: Both flows return the same shape.
#[derive(Debug, Serialize)]
pub(crate) struct OtpResponse {
    pub message: String,
}

/// Incoming OTP verification body (email + 6-digit code).
///
/// **Caller**: `customer::verify_otp`, `employee::verify_otp`
/// **Why**: Both flows accept the same input shape.
#[derive(Debug, Deserialize)]
pub(crate) struct VerifyRequest {
    pub email: String,
    pub code: String,
}

// ---------------------------------------------------------------------------
// OTP backend trait
// ---------------------------------------------------------------------------
/// Wrong guesses a login code tolerates before it stops being valid.
///
/// Five is enough to survive fat fingers and a code read off the wrong mail, and it
/// cuts the six-digit search from a million to five per issued code. The counter lives
/// on the code, so hitting the limit costs the real user one "neuen Code anfordern"
/// rather than a lockout of their address.
pub(crate) const MAX_OTP_ATTEMPTS: i32 = 5;


/// Abstracts the repo-specific OTP operations so the generic handler can work
/// for both customers and employees.
///
/// **Caller**: `handle_request_otp`, `handle_verify_otp`
/// **Why**: Customer and employee OTP flows differ only in which DB tables they
///          hit and whether existence is checked before sending. This trait
///          captures those differences so the shared logic stays DRY.
pub(crate) trait OtpBackend: Send + Sync {
    /// Whether to silently skip sending when the user is unknown (employee flow)
    /// vs always sending (customer flow, which upserts on verify).
    fn check_existence_before_send(&self) -> bool;

    /// Check whether the user exists. Only called when `check_existence_before_send` is true.
    fn user_exists(
        &self,
        pool: &PgPool,
        email: &str,
    ) -> impl std::future::Future<Output = Result<bool, sqlx::Error>> + Send;

    /// Count OTPs sent to this email in the last 10 minutes.
    fn count_recent_otps(
        &self,
        pool: &PgPool,
        email: &str,
    ) -> impl std::future::Future<Output = Result<i64, sqlx::Error>> + Send;

    /// Persist a new OTP. `code_hash` is an Argon2 hash, never the plaintext.
    fn insert_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<(), sqlx::Error>> + Send;

    /// Every code still live for this address: id and Argon2 hash, newest first.
    fn find_live_otps(
        &self,
        pool: &PgPool,
        email: &str,
        now: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<Vec<(Uuid, String)>, sqlx::Error>> + Send;

    /// Count one failed guess against every code currently live for this address.
    fn record_failed_attempt(
        &self,
        pool: &PgPool,
        email: &str,
    ) -> impl std::future::Future<Output = Result<u64, sqlx::Error>> + Send;

    /// Mark an OTP row as used.
    fn mark_otp_used(
        &self,
        pool: &PgPool,
        otp_id: Uuid,
    ) -> impl std::future::Future<Output = Result<(), sqlx::Error>> + Send;

    /// The email subject line for the OTP email.
    fn otp_email_subject(&self) -> &str;

    /// The success message returned after requesting an OTP.
    fn request_success_message(&self) -> &str;

    /// Label used in tracing logs (e.g. "Customer", "Employee").
    fn user_label(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Generic handlers
// ---------------------------------------------------------------------------

/// Shared OTP request logic: validate email, rate-limit, generate code, send email.
///
/// **Caller**: `customer::request_otp`, `employee::request_otp`
/// **Why**: Eliminates duplicated OTP generation / email sending logic.
///
/// # Parameters
/// - `backend` — trait impl that routes to the correct DB tables
/// - `pool` — database connection pool
/// - `email_config` — SMTP settings
/// - `raw_email` — raw email from the request body (will be trimmed + lowercased)
///
/// # Returns
/// `OtpResponse` with a user-facing message.
///
/// # Errors
/// `ApiError::Validation` for bad email, `ApiError::BadRequest` for rate limit,
/// `ApiError::Internal` if SMTP fails.
pub(crate) async fn handle_request_otp(
    backend: &impl OtpBackend,
    pool: &PgPool,
    email_config: &aust_core::config::EmailConfig,
    raw_email: &str,
) -> Result<OtpResponse, ApiError> {
    let email = raw_email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::Validation("Ungültige E-Mail-Adresse".into()));
    }

    // Existence check (employee flow skips sending if unknown)
    let should_send = if backend.check_existence_before_send() {
        backend.user_exists(pool, &email).await?
    } else {
        true
    };

    // Rate limit: max 3 OTPs per email in last 10 minutes
    let recent_count = backend.count_recent_otps(pool, &email).await?;
    if recent_count >= 3 {
        return Err(ApiError::BadRequest(
            "Zu viele Anfragen. Bitte warten Sie einige Minuten.".into(),
        ));
    }

    if should_send {
        let code: String = {
            use rand::Rng;
            let mut rng = rand::rng();
            format!("{:06}", rng.random_range(0..1_000_000u32))
        };

        let expires_at = Utc::now() + chrono::Duration::minutes(10);
        // Only the hash is stored; the plaintext leaves in the mail below and nowhere else.
        let code_hash = hash_otp(&code)?;
        backend.insert_otp(pool, &email, &code_hash, expires_at).await?;

        let subject = backend.otp_email_subject();
        let body_text = format!(
            "Guten Tag,\n\nIhr Zugangscode lautet: {code}\n\nDieser Code ist 10 Minuten gültig.\n\nMit freundlichen Grüßen,\nAust Umzüge"
        );

        send_otp_email(email_config, &email, subject, &body_text)
            .await
            .map_err(|e| {
                tracing::error!(label = backend.user_label(), "Failed to send OTP email: {e}");
                ApiError::Internal("E-Mail konnte nicht gesendet werden".into())
            })?;

        tracing::info!(label = backend.user_label(), "OTP sent");
    }

    Ok(OtpResponse {
        message: backend.request_success_message().to_string(),
    })
}

/// Shared OTP verification logic: validate code format, find valid OTP, mark used,
/// generate session token.
///
/// **Caller**: `customer::verify_otp`, `employee::verify_otp`
/// **Why**: Eliminates duplicated OTP verification logic. The caller is responsible
///          for user-specific lookup (upsert customer / fetch employee) and session
///          creation, since those differ between flows.
///
/// # Parameters
/// - `backend` — trait impl that routes to the correct DB tables
/// - `pool` — database connection pool
/// - `raw_email` — raw email from request body
/// - `raw_code` — raw 6-digit code from request body
///
/// # Returns
/// A session token string. The OTP has been marked as used.
///
/// # Errors
/// `ApiError::Validation` for bad code length, `ApiError::Unauthorized` for
/// invalid/expired code.
pub(crate) async fn handle_verify_otp(
    backend: &impl OtpBackend,
    pool: &PgPool,
    raw_email: &str,
    raw_code: &str,
) -> Result<String, ApiError> {
    let email = raw_email.trim().to_lowercase();
    let code = raw_code.trim().to_string();

    if code.len() != 6 {
        return Err(ApiError::Validation("Code muss 6 Stellen haben".into()));
    }

    let now = Utc::now();
    let candidates = backend.find_live_otps(pool, &email, now).await?;
    let matched = candidates
        .into_iter()
        .find(|(_, hash)| verify_otp_hash(&code, hash));

    let otp_id = match matched {
        Some((id, _)) => id,
        None => {
            // Count the miss against every code still live for this address. Once a code
            // reaches MAX_OTP_ATTEMPTS it stops being returned as a candidate, so the
            // six-digit space is no longer grindable inside the ten-minute window.
            backend.record_failed_attempt(pool, &email).await?;
            return Err(ApiError::Unauthorized(
                "Ungültiger oder abgelaufener Code".into(),
            ));
        }
    };

    backend.mark_otp_used(pool, otp_id).await?;

    let token = generate_session_token();
    Ok(token)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Hash a login code for storage.
///
/// **Caller**: `handle_request_otp`, before the code is persisted.
/// **Why**: The code was stored in plaintext, so a read-only replica, a nightly backup
/// or a dump handed out working login codes for every address in the table. Argon2 with
/// a random salt, the same treatment the admin password reset already gave its code.
pub(crate) fn hash_otp(code: &str) -> Result<String, ApiError> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;

    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(code.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(format!("Code konnte nicht gespeichert werden: {e}")))
}

/// Does this submitted code match a stored hash?
///
/// A malformed stored value simply fails to match rather than erroring, so one bad row
/// cannot lock an address out of logging in.
fn verify_otp_hash(code: &str, stored: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;

    PasswordHash::new(stored)
        .map(|parsed| Argon2::default().verify_password(code.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// Generate a secure 64-character hex session token.
///
/// **Caller**: `handle_verify_otp`
/// **Why**: Cryptographically random token for session persistence.
pub(crate) fn generate_session_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Send an OTP email via SMTP.
///
/// **Caller**: `handle_request_otp`
/// **Why**: Shared SMTP send logic for both customer and employee OTP emails.
pub(crate) async fn send_otp_email(
    email_config: &aust_core::config::EmailConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_plain_email, send_email};

    let message = build_plain_email(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.smtp_tls,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())
}
