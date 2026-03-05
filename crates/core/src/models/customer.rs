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
    /// Full display name (Vorname + Nachname); kept for display and backwards compat.
    pub name: Option<String>,
    /// Explicit salutation chosen by the customer: "Herr", "Frau", or "D" (divers).
    /// When present, always used verbatim — never guessed from the name.
    pub salutation: Option<String>,
    /// Given name (Vorname).
    pub first_name: Option<String>,
    /// Family name (Nachname); used in formal greetings ("Sehr geehrter Herr Müller").
    pub last_name: Option<String>,
    /// Phone number; used for follow-up calls and offer delivery confirmations.
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Customer {
    /// Build the formal greeting line from stored fields, e.g.
    /// `"Sehr geehrter Herr Müller,"` or `"Sehr geehrte Frau Schmidt,"`.
    /// Falls back to `"Sehr geehrte Damen und Herren,"` when salutation is unknown.
    pub fn formal_greeting(&self) -> String {
        match (self.salutation.as_deref(), self.last_name.as_deref()) {
            (Some("Herr"), Some(ln)) => format!("Sehr geehrter Herr {ln},"),
            (Some("Frau"), Some(ln)) => format!("Sehr geehrte Frau {ln},"),
            (Some("D"),    Some(ln)) => format!("Sehr geehrte Person {ln},"),
            _ => "Sehr geehrte Damen und Herren,".to_string(),
        }
    }

    /// Address-block salutation for the XLSX offer (cell A8).
    /// Returns `"Herrn"`, `"Frau"`, `"Divers"`, or `""`.
    pub fn address_salutation(&self) -> &str {
        match self.salutation.as_deref() {
            Some("Herr") => "Herrn",
            Some("Frau") => "Frau",
            Some("D")    => "Divers",
            _            => "",
        }
    }
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
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
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
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
}
