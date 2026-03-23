//! Auth repository — centralised queries for admin user authentication,
//! registration, password management, and password reset OTP flow.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

/// User row projection for authentication (login, refresh, password change).
#[derive(Debug, FromRow)]
pub(crate) struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub role: String,
}

/// Password reset OTP row.
#[derive(Debug, FromRow)]
pub(crate) struct ResetRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub otp_hash: String,
    pub expires_at: DateTime<Utc>,
}

/// Fetch a user by email (exact match).
///
/// **Caller**: `login` handler
/// **Why**: Verifies credentials during login.
pub(crate) async fn fetch_user_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as("SELECT id, email, password_hash, role FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await
}

/// Fetch a user by email (case-insensitive match).
///
/// **Caller**: `reset_password_request`, `reset_password_verify`
/// **Why**: Password reset uses case-insensitive email lookup.
pub(crate) async fn fetch_user_by_email_lower(
    pool: &PgPool,
    email_lower: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, email, password_hash, role FROM users WHERE LOWER(email) = $1",
    )
    .bind(email_lower)
    .fetch_optional(pool)
    .await
}

/// Check whether a user with the given ID exists.
///
/// **Caller**: `refresh_token` handler
/// **Why**: Validates that the user referenced in a refresh token still exists.
pub(crate) async fn user_exists(pool: &PgPool, user_id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Check whether a user with the given email already exists.
///
/// **Caller**: `register` handler
/// **Why**: Prevents duplicate email registration.
pub(crate) async fn email_exists(pool: &PgPool, email: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Insert a new user record.
///
/// **Caller**: `register` handler
/// **Why**: Creates a new admin user with hashed password.
pub(crate) async fn insert_user(
    pool: &PgPool,
    id: Uuid,
    email: &str,
    password_hash: &str,
    name: &str,
    role: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO users (id, email, password_hash, name, role, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        "#,
    )
    .bind(id)
    .bind(email)
    .bind(password_hash)
    .bind(name)
    .bind(role)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a user by ID.
///
/// **Caller**: `change_password` handler
/// **Why**: Retrieves the current password hash for verification before changing.
pub(crate) async fn fetch_user_by_id(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, email, password_hash, role FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Update a user's password hash.
///
/// **Caller**: `change_password` handler
/// **Why**: Stores the new Argon2 hash after verifying the current password.
pub(crate) async fn update_password(
    pool: &PgPool,
    user_id: Uuid,
    new_hash: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = $2 WHERE id = $3")
        .bind(new_hash)
        .bind(now)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Invalidate all unused password reset tokens for a user.
///
/// **Caller**: `reset_password_request` handler
/// **Why**: Ensures only the latest OTP is valid.
pub(crate) async fn invalidate_resets(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE admin_password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a new password reset OTP token.
///
/// **Caller**: `reset_password_request` handler
/// **Why**: Stores the hashed OTP for later verification.
pub(crate) async fn insert_reset_token(
    pool: &PgPool,
    user_id: Uuid,
    otp_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO admin_password_resets (user_id, otp_hash, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(otp_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the latest unused, unexpired password reset token for a user.
///
/// **Caller**: `reset_password_verify` handler
/// **Why**: Retrieves the OTP hash for verification.
pub(crate) async fn fetch_valid_reset(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<ResetRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, user_id, otp_hash, expires_at
        FROM admin_password_resets
        WHERE user_id = $1 AND used_at IS NULL AND expires_at > now()
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Mark a password reset token as used (within a transaction).
///
/// **Caller**: `reset_password_verify` handler
/// **Why**: Single-use OTP enforcement.
pub(crate) async fn mark_reset_used(
    tx: &mut Transaction<'_, Postgres>,
    reset_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE admin_password_resets SET used_at = $1 WHERE id = $2")
        .bind(now)
        .bind(reset_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Update a user's password hash (within a transaction).
///
/// **Caller**: `reset_password_verify` handler
/// **Why**: Password update is part of the same transaction as marking the OTP used.
pub(crate) async fn update_password_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    new_hash: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = $2 WHERE id = $3")
        .bind(new_hash)
        .bind(now)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
