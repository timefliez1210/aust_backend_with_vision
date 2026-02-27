use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// A moving-company customer, identified uniquely by email address.
///
/// Customers are created (or upserted) by the orchestrator the first time a
/// moving inquiry arrives for a given email. All quotes and offers reference
/// a `Customer` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    /// UUID v7 primary key (time-ordered for efficient B-tree indexing).
    pub id: Uuid,
    /// Unique email address; used as the primary business identifier.
    pub email: String,
    /// Full display name; may be absent for unidentified direct emails.
    pub name: Option<String>,
    /// Phone number; used for follow-up calls and offer delivery confirmations.
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new customer record.
///
/// **Caller**: `orchestrator.rs` calls the customer repository with this struct
/// when processing a new `MovingInquiry`.
/// **Why**: Separating creation input from the full `Customer` model keeps
/// validation logic close to the input boundary and prevents callers from
/// accidentally supplying server-generated fields like `id` or timestamps.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateCustomer {
    /// Must be a syntactically valid email address.
    #[validate(email(message = "Ungültige E-Mail-Adresse"))]
    pub email: String,
    pub name: Option<String>,
    /// When present, must be at least 5 characters (allows short codes like `"0176x"`).
    #[validate(length(min = 5, message = "Telefonnummer muss mindestens 5 Zeichen haben"))]
    pub phone: Option<String>,
}

/// Partial update applied to an existing customer record.
///
/// **Caller**: Admin API `PATCH /api/v1/customers/{id}` and the orchestrator
/// when new contact info arrives in a follow-up email.
/// **Why**: Using `Option` fields means callers only send the fields they want
/// to change; `None` fields are left unchanged in the database.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCustomer {
    pub name: Option<String>,
    pub phone: Option<String>,
}
