use axum::{extract::State, routing::post, Extension, Json, Router};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::Deserialize;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
use serde::Serialize;
use validator::Validate;

use aust_core::models::{AuthToken, CreateUser, LoginRequest, TokenClaims, UserRole};

use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/login", post(login))
        .route("/refresh", post(refresh_token))
}

/// Routes that require authentication (nested under admin middleware)
pub fn protected_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/register", post(register))
        .route("/change-password", post(change_password))
}

#[derive(Debug, FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    role: String,
}

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

async fn login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthToken>, ApiError> {
    if request.email.is_empty() || request.password.is_empty() {
        return Err(ApiError::Validation(
            "E-Mail und Passwort sind erforderlich".into(),
        ));
    }

    let user: Option<UserRow> = sqlx::query_as(
        "SELECT id, email, password_hash, role FROM users WHERE email = $1",
    )
    .bind(&request.email)
    .fetch_optional(&state.db)
    .await?;

    let user = user.ok_or_else(|| {
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
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE id = $1")
            .bind(claims.sub)
            .fetch_optional(&state.db)
            .await?;

    if exists.is_none() {
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

async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateUser>,
) -> Result<Json<RegisterResponse>, ApiError> {
    request
        .validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    // Check if email already taken
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE email = $1")
            .bind(&request.email)
            .fetch_optional(&state.db)
            .await?;

    if exists.is_some() {
        return Err(ApiError::Validation(
            "E-Mail-Adresse bereits vergeben".into(),
        ));
    }

    let password_hash = hash_password(&request.password)?;
    let role = request.role.unwrap_or_default();
    let id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        r#"
        INSERT INTO users (id, email, password_hash, name, role, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        "#,
    )
    .bind(id)
    .bind(&request.email)
    .bind(&password_hash)
    .bind(&request.name)
    .bind(role.as_str())
    .bind(now)
    .execute(&state.db)
    .await?;

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

    let user: Option<UserRow> = sqlx::query_as(
        "SELECT id, email, password_hash, role FROM users WHERE id = $1",
    )
    .bind(claims.sub)
    .fetch_optional(&state.db)
    .await?;

    let user = user.ok_or_else(|| ApiError::NotFound("Benutzer nicht gefunden".into()))?;

    // Verify current password
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|_| ApiError::Internal("Passwort-Hash ungueltig".into()))?;

    Argon2::default()
        .verify_password(request.current_password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Validation("Aktuelles Passwort ist falsch".into()))?;

    // Hash and store new password
    let new_hash = hash_password(&request.new_password)?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = $2 WHERE id = $3")
        .bind(&new_hash)
        .bind(Utc::now())
        .bind(claims.sub)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "ok": true })))
}
