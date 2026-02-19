use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateCustomer {
    #[validate(email(message = "Ungültige E-Mail-Adresse"))]
    pub email: String,
    pub name: Option<String>,
    #[validate(length(min = 5, message = "Telefonnummer muss mindestens 5 Zeichen haben"))]
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCustomer {
    pub name: Option<String>,
    pub phone: Option<String>,
}
