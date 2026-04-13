//! Customer repository — centralised queries for the `customers` table.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// SQLx projection row for the `customers` table.
///
/// Used by offer generation (greeting, address block), inquiry builder (snapshot),
/// and admin endpoints (detail, list).
#[derive(Debug, FromRow)]
pub(crate) struct CustomerRow {
    #[allow(dead_code)]
    pub id: Uuid,
    pub email: Option<String>,
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
    #[sqlx(default)]
    pub customer_type: Option<String>,
    #[sqlx(default)]
    pub company_name: Option<String>,
    #[sqlx(default)]
    pub billing_address_id: Option<Uuid>,
}

impl CustomerRow {
    /// Formal greeting line using stored salutation + last name.
    /// Falls back to the `detect_salutation_and_greeting` heuristic for legacy
    /// customers who pre-date the structured name fields.
    ///
    /// For business customers without a known Ansprechpartner, returns
    /// "Sehr geehrte Damen und Herren,".
    pub fn formal_greeting(&self) -> String {
        // Business without personal salutation → generic
        if self.customer_type.as_deref() == Some("business")
            && self.last_name.is_none()
            && self.salutation.is_none()
        {
            return "Sehr geehrte Damen und Herren,".to_string();
        }
        match (self.salutation.as_deref(), self.last_name.as_deref()) {
            (Some("Herr"), Some(ln)) => format!("Sehr geehrter Herr {ln},"),
            (Some("Frau"), Some(ln)) => format!("Sehr geehrte Frau {ln},"),
            (Some("D"), Some(ln)) => format!("Sehr geehrte Person {ln},"),
            _ => {
                let name = self.name.as_deref().or(self.email.as_deref()).unwrap_or("Kunde");
                crate::services::offer_builder::detect_salutation_and_greeting(name).1
            }
        }
    }

    /// Address-block salutation for XLSX cell A8 ("Herrn", "Frau", "Divers", or "").
    /// For business customers, returns the company name instead of a personal salutation.
    pub fn address_salutation(&self) -> String {
        // Business → company name goes in the salutation slot
        if self.customer_type.as_deref() == Some("business") {
            if let Some(ref cn) = self.company_name {
                if !cn.is_empty() {
                    return cn.clone();
                }
            }
        }
        match self.salutation.as_deref() {
            Some("Herr") => "Herrn".to_string(),
            Some("Frau") => "Frau".to_string(),
            Some("D") => "Divers".to_string(),
            _ => {
                let name = self.name.as_deref().or(self.email.as_deref()).unwrap_or("Kunde");
                crate::services::offer_builder::detect_salutation_and_greeting(name).0
            }
        }
    }

    /// Attention line for XLSX cell A9 — "z.Hd. Herrn Müller" for business
    /// customers with a known Ansprechpartner. Empty string for private.
    pub fn attention_line(&self) -> String {
        if self.customer_type.as_deref() != Some("business") {
            return String::new();
        }
        // Business with known Ansprechpartner
        let name = match (self.salutation.as_deref(), self.first_name.as_deref(), self.last_name.as_deref()) {
            (Some("Herr"), Some(f), Some(l)) => format!("z.Hd. Herrn {f} {l}"),
            (Some("Frau"), Some(f), Some(l)) => format!("z.Hd. Frau {f} {l}"),
            (Some("D"), Some(f), Some(l)) => format!("z.Hd. {f} {l}"),
            (Some("Herr"), None, Some(l)) => format!("z.Hd. Herrn {l}"),
            (Some("Frau"), None, Some(l)) => format!("z.Hd. Frau {l}"),
            (_, Some(f), Some(l)) => format!("z.Hd. {f} {l}"),
            _ => String::new(),
        };
        name
    }

    /// Full display name: first + last, or the legacy `name` field, or email.
    pub fn display_name(&self) -> String {
        match (self.first_name.as_deref(), self.last_name.as_deref()) {
            (Some(f), Some(l)) => format!("{f} {l}"),
            (Some(f), None) => f.to_string(),
            (None, Some(l)) => l.to_string(),
            _ => self.name.clone().unwrap_or_else(|| self.email.clone().unwrap_or_else(|| "Kunde".to_string())),
        }
    }
}

/// Fetch a customer by primary key.
///
/// **Caller**: `build_offer_with_overrides`, `inquiry_builder::build_inquiry_response`
/// **Why**: Multiple modules need the full customer row for greeting, snapshot, etc.
pub(crate) async fn fetch_by_id(pool: &PgPool, customer_id: Uuid) -> Result<CustomerRow, ApiError> {
    sqlx::query_as(
        "SELECT id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, billing_address_id FROM customers WHERE id = $1",
    )
    .bind(customer_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Customer not found".into()))
}

/// Fetch a customer by email address.
///
/// **Caller**: `orchestrator::find_or_create_offer_thread`
/// **Why**: Thread creation requires the customer_id which is looked up by email.
pub(crate) async fn fetch_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<CustomerRow>, ApiError> {
    let row = sqlx::query_as(
        "SELECT id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, billing_address_id FROM customers WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Upsert a customer by email — insert if new, merge non-null fields if existing.
///
/// **Caller**: `handle_submission`, `handle_complete_inquiry`, `video_inquiry`
/// **Why**: Multiple entry points create customers; upsert avoids duplicates while
///          filling in missing fields from later submissions.
///
/// # Returns
/// The customer's UUID (either newly created or existing).
pub(crate) async fn upsert(
    pool: &PgPool,
    email: &str,
    name: Option<&str>,
    salutation: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    phone: Option<&str>,
    customer_type: Option<&str>,
    company_name: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let (id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
        ON CONFLICT (email) DO UPDATE SET
            name         = COALESCE(EXCLUDED.name,         customers.name),
            salutation   = COALESCE(EXCLUDED.salutation,   customers.salutation),
            first_name   = COALESCE(EXCLUDED.first_name,   customers.first_name),
            last_name    = COALESCE(EXCLUDED.last_name,     customers.last_name),
            phone        = COALESCE(EXCLUDED.phone,        customers.phone),
            customer_type = COALESCE(EXCLUDED.customer_type, customers.customer_type),
            company_name  = COALESCE(EXCLUDED.company_name,  customers.company_name),
            updated_at   = $10
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(salutation)
    .bind(first_name)
    .bind(last_name)
    .bind(phone)
    .bind(customer_type)
    .bind(company_name)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Fetch a customer by inquiry ID (joins through inquiries table).
///
/// **Caller**: `invoices::build_invoice_data`
/// **Why**: Invoice generation needs customer data but only has the inquiry ID.
pub(crate) async fn fetch_by_inquiry_id(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<CustomerRow, ApiError> {
    sqlx::query_as(
        "SELECT c.id, c.email, c.name, c.salutation, c.first_name, c.last_name, c.phone, c.customer_type, c.company_name, c.billing_address_id
         FROM customers c
         JOIN inquiries i ON i.customer_id = c.id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Customer not found for inquiry".into()))
}

/// Check whether a customer with the given ID exists.
///
/// **Caller**: `create_inquiry`
/// **Why**: Validates customer_id before creating an inquiry.
pub(crate) async fn exists(pool: &PgPool, customer_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM customers WHERE id = $1)")
        .bind(customer_id)
        .fetch_one(pool)
        .await
}

/// Create a recipient customer from form fields.
///
/// **Caller**: All four submission handlers (photo, video, mobile, AR) and manual_inquiry.
/// **Why**: When a customer books a service for someone else, the recipient
///          is a separate person. We create a minimal customer record so that
///          `recipient_id` can be set on the inquiry.
///
/// If the recipient has no email, we generate a placeholder to satisfy the
/// UNIQUE constraint on customers.email. The placeholder includes the payer's
/// email as a disambiguator so that recipients without email don't collide.
pub(crate) async fn create_recipient(
    pool: &PgPool,
    salutation: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    phone: Option<&str>,
    email: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Uuid, ApiError> {
    // Recipient must have at least a last name to be meaningful
    let last = last_name.filter(|s| !s.trim().is_empty());
    if last.is_none() {
        // No recipient data — return early with Ok(None) equivalent
        // Callers should check whether recipient_last_name was provided before calling
        return Err(ApiError::Validation("Empfaenger-Nachname ist erforderlich".into()));
    }

    // Use provided email, or generate a placeholder
    let recipient_email = match email.filter(|s| !s.trim().is_empty()) {
        Some(e) => e.to_string(),
        None => format!("recipient-{}@aufraeumhelden.com", uuid::Uuid::now_v7()),
    };

    let id = uuid::Uuid::now_v7();
    let name = format!("{} {}",
        first_name.as_deref().unwrap_or(""),
        last.as_deref().unwrap_or(""
    )).trim().to_string();

    sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, customer_type, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 'private', $8, $8)
        ON CONFLICT (email) DO UPDATE SET
            name = COALESCE(NULLIF(EXCLUDED.name, ''), customers.name),
            salutation = COALESCE(NULLIF(EXCLUDED.salutation, ''), customers.salutation),
            first_name = COALESCE(NULLIF(EXCLUDED.first_name, ''), customers.first_name),
            last_name = COALESCE(NULLIF(EXCLUDED.last_name, ''), customers.last_name),
            phone = COALESCE(NULLIF(EXCLUDED.phone, ''), customers.phone),
            updated_at = EXCLUDED.updated_at
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(&recipient_email)
    .bind(&name)
    .bind(salutation)
    .bind(first_name)
    .bind(last)
    .bind(phone)
    .bind(now)
    .fetch_one(pool)
    .await
    .map(|(id,): (Uuid,)| id)
    .map_err(|e| ApiError::Internal(format!("Empfaenger konnte nicht erstellt werden: {e}")))
}
