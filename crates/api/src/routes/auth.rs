use axum::{extract::State, routing::post, Extension, Json, Router};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
use serde::Serialize;
use validator::Validate;

use aust_core::models::{AuthToken, CreateUser, LoginRequest, TokenClaims, UserRole};

use crate::repositories::auth_repo;
use crate::{ApiError, AppState};

/// Register the public authentication routes (login and token refresh).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly.
/// **Why**: Login and refresh must be publicly accessible (no existing JWT required).
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", post(login))
        .route("/refresh", post(refresh_token))
        .route("/reset-password/request", post(reset_password_request))
        .route("/reset-password/verify", post(reset_password_verify))
}

/// Register the protected authentication routes (require an existing valid JWT).
///
/// **Caller**: `crates/api/src/routes/mod.rs` route tree assembly, nested under the admin
/// JWT middleware.
/// **Why**: Register and change-password must be gated so only authenticated admins can
/// create new accounts or change passwords.
pub fn protected_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/register", post(register))
        .route("/change-password", post(change_password))
}

/// Mint a new access token and refresh token pair for a user.
///
/// **Caller**: `login` and `refresh_token` handlers.
/// **Why**: Centralises JWT creation so both login and refresh use identical signing
/// logic. Access token expiry is configurable via `Config.auth.jwt_expiry_hours`;
/// refresh token is always fixed at 7 days.
///
/// # Parameters
/// - `user_id` — UUID placed in the `sub` claim
/// - `email` — stored in the token claims for display purposes
/// - `role` — `UserRole` enum stored in the claims for middleware authorisation
/// - `jwt_secret` — HMAC-SHA256 signing secret from config
/// - `expiry_hours` — access token lifetime in hours
///
/// # Returns
/// `AuthToken` with `access_token`, `refresh_token`, `token_type = "Bearer"`, and
/// `expires_in` (seconds).
///
/// # Errors
/// - `500` if JWT encoding fails (should only happen with an invalid secret format)
fn create_tokens(
    user_id: Uuid,
    email: &str,
    role: UserRole,
    jwt_secret: &str,
    expiry_hours: u64,
) -> Result<AuthToken, ApiError> {
    let now = Utc::now().timestamp() as usize;

    let access_claims = TokenClaims {
        sub: user_id,
        email: email.to_string(),
        role,
        iat: now,
        exp: now + (expiry_hours as usize * 3600),
    };

    let access_token = encode(
        &Header::default(),
        &access_claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(format!("Token-Erstellung fehlgeschlagen: {e}")))?;

    // Refresh token: 7 days
    let refresh_claims = TokenClaims {
        sub: user_id,
        email: email.to_string(),
        role,
        iat: now,
        exp: now + (7 * 24 * 3600),
    };

    let refresh_token = encode(
        &Header::default(),
        &refresh_claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(format!("Refresh-Token-Erstellung fehlgeschlagen: {e}")))?;

    Ok(AuthToken {
        access_token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in: expiry_hours * 3600,
    })
}

/// `POST /api/v1/auth/login` — Authenticate an admin user and return JWT tokens.
///
/// **Caller**: Axum router / admin dashboard login page.
/// **Why**: Verifies email + Argon2 password hash, then mints a short-lived access token
/// and a 7-day refresh token.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, JWT config)
/// - `request` — `LoginRequest` JSON body with `email` and `password`
///
/// # Returns
/// `200 OK` with `AuthToken` JSON (access_token, refresh_token, token_type, expires_in).
///
/// # Errors
/// - `400` if email or password fields are empty
/// - `401` if credentials are invalid (same message for email-not-found and wrong password,
///   to prevent user enumeration)
async fn login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthToken>, ApiError> {
    if request.email.is_empty() || request.password.is_empty() {
        return Err(ApiError::Validation(
            "E-Mail und Passwort sind erforderlich".into(),
        ));
    }

    let user = auth_repo::fetch_user_by_email(&state.db, &request.email)
        .await?
        .ok_or_else(|| {
            ApiError::Unauthorized("Ungültige E-Mail oder Passwort".into())
        })?;

    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|_| ApiError::Internal("Passwort-Hash ungültig".into()))?;

    Argon2::default()
        .verify_password(request.password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized("Ungültige E-Mail oder Passwort".into()))?;

    let role = UserRole::from_str(&user.role);

    let token = create_tokens(
        user.id,
        &user.email,
        role,
        &state.config.auth.jwt_secret,
        state.config.auth.jwt_expiry_hours,
    )?;

    Ok(Json(token))
}

#[derive(Debug, Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

/// `POST /api/v1/auth/refresh` — Exchange a valid refresh token for a new token pair.
///
/// **Caller**: Axum router / admin dashboard token refresh logic.
/// **Why**: Validates the refresh token signature and expiry, verifies the user still
/// exists in the DB, and issues fresh access + refresh tokens. This allows the dashboard
/// to stay authenticated without re-entering credentials.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, JWT config)
/// - `request` — JSON body with `refresh_token` string
///
/// # Returns
/// `200 OK` with new `AuthToken` JSON.
///
/// # Errors
/// - `401` if the refresh token is invalid, expired, or the user no longer exists
async fn refresh_token(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RefreshRequest>,
) -> Result<Json<AuthToken>, ApiError> {
    let secret = &state.config.auth.jwt_secret;

    let token_data = decode::<TokenClaims>(
        &request.refresh_token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| ApiError::Unauthorized("Ungültiges oder abgelaufenes Refresh-Token".into()))?;

    let claims = token_data.claims;

    // Verify user still exists
    let exists = auth_repo::user_exists(&state.db, claims.sub).await?;
    if !exists {
        return Err(ApiError::Unauthorized("Benutzer nicht gefunden".into()));
    }

    let token = create_tokens(
        claims.sub,
        &claims.email,
        claims.role,
        secret,
        state.config.auth.jwt_expiry_hours,
    )?;

    Ok(Json(token))
}

// --- Register ---

/// Hash a plaintext password with Argon2id using a random salt.
///
/// **Caller**: `register` and `change_password` handlers.
/// **Why**: Centralises Argon2 hashing so both account creation and password change use
/// identical parameters (Argon2 default configuration).
///
/// # Parameters
/// - `password` — plaintext password string
///
/// # Returns
/// PHC string (e.g. `$argon2id$v=19$m=19456,t=2,p=1$...`) suitable for storage in
/// `users.password_hash`.
///
/// # Errors
/// - `500` if Argon2 hashing fails (should not occur in normal operation)
fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| ApiError::Internal(format!("Passwort-Hashing fehlgeschlagen: {e}")))?;
    Ok(hash.to_string())
}

#[derive(Debug, Serialize)]
struct RegisterResponse {
    id: Uuid,
    email: String,
    name: String,
    role: UserRole,
}

/// `POST /api/v1/auth/register` — Register a new admin user (protected, requires existing JWT).
///
/// **Caller**: Axum protected router / admin dashboard "Neuen Benutzer anlegen" form.
/// **Why**: Creates a new user with a hashed password. The `CreateUser` struct is validated
/// (email format, minimum password length) before insertion. Returns a conflict error if
/// the email is already taken.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `request` — `CreateUser` JSON body (email, password, name, optional role)
///
/// # Returns
/// `200 OK` with `RegisterResponse` (id, email, name, role).
///
/// # Errors
/// - `400` if validation fails (bad email format, short password, duplicate email)
/// - `500` on DB or hashing failures
async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateUser>,
) -> Result<Json<RegisterResponse>, ApiError> {
    request
        .validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    // Check if email already taken
    let exists = auth_repo::email_exists(&state.db, &request.email).await?;
    if exists {
        return Err(ApiError::Validation(
            "E-Mail-Adresse bereits vergeben".into(),
        ));
    }

    let password_hash = hash_password(&request.password)?;
    let role = request.role.unwrap_or_default();
    let id = Uuid::now_v7();
    let now = Utc::now();

    auth_repo::insert_user(&state.db, id, &request.email, &password_hash, &request.name, role.as_str(), now).await?;

    Ok(Json(RegisterResponse {
        id,
        email: request.email,
        name: request.name,
        role,
    }))
}

// --- Change Password ---

#[derive(Debug, Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

/// `POST /api/v1/auth/change-password` — Change the authenticated user's password.
///
/// **Caller**: Axum protected router / admin dashboard account settings page.
/// **Why**: Requires the user to supply their current password before accepting the new one,
/// preventing account takeover if a session token is stolen. Minimum new password length
/// is 8 characters.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `claims` — JWT claims of the currently authenticated user (provides `sub` user ID)
/// - `request` — JSON body with `current_password` and `new_password`
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `400` if new password is shorter than 8 characters or current password is wrong
/// - `404` if the user is not found (should not occur with a valid token)
async fn change_password(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(request): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.new_password.len() < 8 {
        return Err(ApiError::Validation(
            "Neues Passwort muss mindestens 8 Zeichen haben".into(),
        ));
    }

    let user = auth_repo::fetch_user_by_id(&state.db, claims.sub)
        .await?
        .ok_or_else(|| ApiError::NotFound("Benutzer nicht gefunden".into()))?;

    // Verify current password
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|_| ApiError::Internal("Passwort-Hash ungueltig".into()))?;

    Argon2::default()
        .verify_password(request.current_password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Validation("Aktuelles Passwort ist falsch".into()))?;

    // Hash and store new password
    let new_hash = hash_password(&request.new_password)?;

    auth_repo::update_password(&state.db, claims.sub, &new_hash, Utc::now()).await?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Password Reset (OTP) ---

#[derive(Debug, Deserialize)]
struct ResetRequestBody {
    email: String,
}

/// `POST /api/v1/auth/reset-password/request` — Send a 6-digit OTP to the user's email.
///
/// **Caller**: Axum public router / admin login page "Passwort vergessen" flow.
/// **Why**: Initiates password recovery by generating a short-lived OTP, storing its
/// Argon2 hash in `admin_password_resets`, and emailing the plaintext code to the user.
/// Always returns 200 regardless of whether the email exists to prevent user enumeration.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config)
/// - `body` — JSON with `email`
///
/// # Returns
/// `200 OK` with `{"ok": true}` in all cases.
async fn reset_password_request(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ResetRequestBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let email = body.email.trim().to_lowercase();

    // Look up user — if not found, return 200 silently (no enumeration)
    let user = auth_repo::fetch_user_by_email_lower(&state.db, &email).await?;

    if let Some(user) = user {
        // Generate 6-digit OTP
        let otp: u32 = rand::thread_rng().gen_range(100_000..=999_999);
        let otp_str = format!("{otp:06}");

        // Hash it for storage
        let otp_hash = hash_password(&otp_str)?;
        let expires_at = Utc::now() + Duration::minutes(15);

        // Invalidate any existing unused tokens for this user
        auth_repo::invalidate_resets(&state.db, user.id).await?;

        // Store new token
        auth_repo::insert_reset_token(&state.db, user.id, &otp_hash, expires_at).await?;

        // Send email (best-effort — don't fail the request if SMTP is down)
        let body_text = format!(
            "Ihr Passwort-Reset-Code lautet:\n\n  {otp_str}\n\nDer Code ist 15 Minuten gültig.\n\nFalls Sie kein Passwort-Reset angefordert haben, können Sie diese E-Mail ignorieren."
        );
        let _ = crate::services::email::send_email(
            &state.config.email.smtp_host,
            state.config.email.smtp_port,
            &state.config.email.username,
            &state.config.email.password,
            crate::services::email::build_plain_email(
                &state.config.email.from_address,
                &state.config.email.from_name,
                &user.email,
                "Passwort-Reset Code – AUST Admin",
                &body_text,
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?,
        )
        .await;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct ResetVerifyBody {
    email: String,
    otp: String,
    new_password: String,
}

/// `POST /api/v1/auth/reset-password/verify` — Validate OTP and set a new password.
///
/// **Caller**: Axum public router / admin login page OTP entry step.
/// **Why**: Verifies the 6-digit OTP against the stored Argon2 hash, checks expiry and
/// single-use, then updates the user's password hash and marks the token used.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `body` — JSON with `email`, `otp` (6 digits), `new_password`
///
/// # Returns
/// `200 OK` with `{"ok": true}` on success.
///
/// # Errors
/// - `400` if new password is shorter than 8 characters
/// - `400` if OTP is invalid or expired (generic message, no enumeration)
async fn reset_password_verify(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ResetVerifyBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.new_password.len() < 8 {
        return Err(ApiError::Validation(
            "Neues Passwort muss mindestens 8 Zeichen haben".into(),
        ));
    }

    let email = body.email.trim().to_lowercase();

    let user = auth_repo::fetch_user_by_email_lower(&state.db, &email)
        .await?
        .ok_or_else(|| ApiError::Validation("Ungültiger oder abgelaufener Code".into()))?;

    // Fetch the latest unused, unexpired token for this user
    let reset = auth_repo::fetch_valid_reset(&state.db, user.id)
        .await?
        .ok_or_else(|| ApiError::Validation("Ungültiger oder abgelaufener Code".into()))?;

    // Verify OTP
    let parsed_hash = PasswordHash::new(&reset.otp_hash)
        .map_err(|_| ApiError::Internal("OTP-Hash ungültig".into()))?;

    Argon2::default()
        .verify_password(body.otp.trim().as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Validation("Ungültiger oder abgelaufener Code".into()))?;

    // Mark token used and update password in a transaction
    let new_hash = hash_password(&body.new_password)?;
    let now = Utc::now();

    let mut tx = state.db.begin().await?;

    auth_repo::mark_reset_used(&mut tx, reset.id, now).await?;
    auth_repo::update_password_tx(&mut tx, user.id, &new_hash, now).await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({ "ok": true })))
}
