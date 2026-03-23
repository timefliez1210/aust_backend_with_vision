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

    /// Persist a new OTP code.
    fn insert_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        expires_at: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<(), sqlx::Error>> + Send;

    /// Find a valid (unused, non-expired) OTP. Returns the row ID.
    fn find_valid_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<Option<Uuid>, sqlx::Error>> + Send;

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
        backend.insert_otp(pool, &email, &code, expires_at).await?;

        let subject = backend.otp_email_subject();
        let body_text = format!(
            "Guten Tag,\n\nIhr Zugangscode lautet: {code}\n\nDieser Code ist 10 Minuten gültig.\n\nMit freundlichen Grüßen,\nAust Umzüge"
        );

        send_otp_email(email_config, &email, subject, &body_text)
            .await
            .map_err(|e| {
                tracing::error!("Failed to send OTP email to {email}: {e}");
                ApiError::Internal("E-Mail konnte nicht gesendet werden".into())
            })?;

        tracing::info!(email = %email, label = backend.user_label(), "OTP sent");
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
    let otp_id = backend
        .find_valid_otp(pool, &email, &code, now)
        .await?
        .ok_or_else(|| ApiError::Unauthorized("Ungültiger oder abgelaufener Code".into()))?;

    backend.mark_otp_used(pool, otp_id).await?;

    let token = generate_session_token();
    Ok(token)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())
}
