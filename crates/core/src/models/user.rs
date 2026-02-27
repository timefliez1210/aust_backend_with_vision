use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// Access level for admin API users.
///
/// Role-based access is not yet enforced by middleware (see TODO in CLAUDE.md),
/// but the field is persisted and exposed so that future middleware can
/// distinguish admins from operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    /// Full access to all admin operations including user management.
    Admin,
    /// Read/write access to quotes and offers; no user management.
    Operator,
}

impl Default for UserRole {
    fn default() -> Self {
        Self::Operator
    }
}

impl UserRole {
    /// Returns the lowercase string stored in the `users.role` database column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
        }
    }

    /// Parses a role from its database string representation.
    /// Unknown strings default to `Operator` rather than failing.
    pub fn from_str(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            _ => Self::Operator,
        }
    }
}

/// An admin user who can access the REST API.
///
/// Distinct from a `Customer`: admins log in with a username/password and
/// receive a JWT. Customers authenticate via OTP magic link or are unauthenticated
/// (form submissions arrive by email).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// Login email address; must be unique across all admin users.
    pub email: String,
    /// Display name shown in the admin UI.
    pub name: String,
    pub role: UserRole,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new admin user.
///
/// **Caller**: Admin `POST /api/v1/users` (or an initial seed migration).
/// **Why**: Separates creation input (includes raw password for hashing) from
/// the stored `User` (which never exposes the password hash).
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateUser {
    #[validate(email(message = "Ungültige E-Mail-Adresse"))]
    pub email: String,
    /// Plain-text password; hashed with Argon2 before storage (TODO: not yet implemented).
    #[validate(length(min = 8, message = "Passwort muss mindestens 8 Zeichen haben"))]
    pub password: String,
    #[validate(length(min = 1, message = "Name darf nicht leer sein"))]
    pub name: String,
    /// Defaults to `Operator` when `None`.
    pub role: Option<UserRole>,
}

/// Credentials submitted to `POST /api/v1/auth/login`.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    pub password: String,
}

/// JWT access + refresh token pair returned after a successful login.
///
/// **Caller**: The `auth/login` route handler returns this; the admin client
/// stores `access_token` in memory and `refresh_token` in local storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthToken {
    /// Short-lived JWT for authenticating API requests (Bearer token).
    pub access_token: String,
    /// Long-lived token used to obtain a new `access_token` without re-login.
    pub refresh_token: String,
    /// Always `"Bearer"`.
    pub token_type: String,
    /// Access token lifetime in seconds.
    pub expires_in: u64,
}

/// JWT payload (claims) embedded inside the signed access token.
///
/// **Caller**: Auth middleware decodes and validates this from the
/// `Authorization: Bearer <token>` header on every protected API request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    /// Subject — the user's UUID.
    pub sub: Uuid,
    pub email: String,
    pub role: UserRole,
    /// Expiry timestamp (Unix seconds), validated by the JWT library.
    pub exp: usize,
    /// Issued-at timestamp (Unix seconds).
    pub iat: usize,
}
