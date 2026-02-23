use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    Admin,
    Operator,
}

impl Default for UserRole {
    fn default() -> Self {
        Self::Operator
    }
}

impl UserRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "admin" => Self::Admin,
            _ => Self::Operator,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub role: UserRole,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateUser {
    #[validate(email(message = "Ungültige E-Mail-Adresse"))]
    pub email: String,
    #[validate(length(min = 8, message = "Passwort muss mindestens 8 Zeichen haben"))]
    pub password: String,
    #[validate(length(min = 1, message = "Name darf nicht leer sein"))]
    pub name: String,
    pub role: Option<UserRole>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthToken {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    pub sub: Uuid,
    pub email: String,
    pub role: UserRole,
    pub exp: usize,
    pub iat: usize,
}
