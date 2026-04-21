use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::{AddressSnapshot, TokenClaims};
use crate::repositories::{address_repo, admin_repo};
use crate::{ApiError, AppState};

use super::admin::require_admin;

// --- Customers ---

#[derive(Debug, Deserialize)]
pub(super) struct ListCustomersQuery {
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerListItem {
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    customer_type: Option<String>,
    company_name: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerListResponse {
    customers: Vec<CustomerListItem>,
    total: i64,
}

/// `GET /api/v1/admin/customers` — List customers with optional full-text search.
///
/// **Caller**: Axum router / admin dashboard customers list page.
/// **Why**: Paginated, ILIKE-searchable customer listing for the admin panel.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `search` (matched against name and email), `limit`, `offset`
///
/// # Returns
/// `200 OK` with `CustomerListResponse` containing `customers` array and `total` count.
pub(super) async fn list_customers(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListCustomersQuery>,
) -> Result<Json<CustomerListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    let repo_customers = admin_repo::list_customers(&state.db, &search, limit, offset).await?;
    let customers: Vec<CustomerListItem> = repo_customers
        .into_iter()
        .map(|c| CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone,
            customer_type: c.customer_type, company_name: c.company_name, created_at: c.created_at,
        })
        .collect();

    let total = admin_repo::count_customers(&state.db, &search).await?;

    Ok(Json(CustomerListResponse { customers, total }))
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerDetailResponse {
    id: Uuid,
    email: Option<String>,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    customer_type: Option<String>,
    company_name: Option<String>,
    billing_address_id: Option<Uuid>,
    billing_address: Option<AddressSnapshot>,
    created_at: DateTime<Utc>,
    quotes: Vec<CustomerQuote>,
    offers: Vec<CustomerOffer>,
    termine: Vec<CustomerTermin>,
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerQuote {
    id: Uuid,
    status: String,
    service_type: Option<String>,
    estimated_volume_m3: Option<f64>,
    scheduled_date: Option<NaiveDate>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerOffer {
    id: Uuid,
    inquiry_id: Uuid,
    price_cents: i64,
    status: String,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(super) struct CustomerTermin {
    id: Uuid,
    title: String,
    category: String,
    scheduled_date: Option<NaiveDate>,
    status: String,
}

/// `GET /api/v1/admin/customers/{id}` — Retrieve a customer with their quotes and offers.
///
/// **Caller**: Axum router / admin dashboard customer detail page.
/// **Why**: Returns customer contact info plus all associated inquiries and offers,
/// ordered newest-first, for the admin CRM view.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
///
/// # Returns
/// `200 OK` with `CustomerDetailResponse`.
///
/// # Errors
/// - `404` if customer not found
pub(super) async fn get_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<CustomerDetailResponse>, ApiError> {
    let repo_customer = admin_repo::fetch_customer(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))?;

    let repo_quotes = admin_repo::fetch_customer_quotes(&state.db, id).await?;
    let quotes: Vec<CustomerQuote> = repo_quotes
        .into_iter()
        .map(|q| CustomerQuote {
            id: q.id, status: q.status, service_type: q.service_type,
            estimated_volume_m3: q.estimated_volume_m3,
            scheduled_date: q.scheduled_date, created_at: q.created_at,
        })
        .collect();

    let repo_offers = admin_repo::fetch_customer_offers(&state.db, id).await?;
    let offers: Vec<CustomerOffer> = repo_offers
        .into_iter()
        .map(|o| CustomerOffer {
            id: o.id, inquiry_id: o.inquiry_id, price_cents: o.price_cents,
            status: o.status, created_at: o.created_at, sent_at: o.sent_at,
        })
        .collect();

    let repo_termine = admin_repo::fetch_customer_termine(&state.db, id).await?;
    let termine: Vec<CustomerTermin> = repo_termine
        .into_iter()
        .map(|t| CustomerTermin {
            id: t.id, title: t.title, category: t.category,
            scheduled_date: t.scheduled_date, status: t.status,
        })
        .collect();

    let billing_address = if let Some(addr_id) = repo_customer.billing_address_id {
        address_repo::fetch_full(&state.db, addr_id).await?.map(|a| AddressSnapshot {
            id: a.id,
            street: a.street,
            house_number: a.house_number,
            city: a.city,
            postal_code: a.postal_code.unwrap_or_default(),
            country: if a.country.is_empty() { "Deutschland".to_string() } else { a.country },
            floor: a.floor,
            elevator: a.elevator,
            needs_parking_ban: Some(a.parking_ban),
            parking_ban: a.parking_ban,
            latitude: a.latitude,
            longitude: a.longitude,
        })
    } else {
        None
    };

    Ok(Json(CustomerDetailResponse {
        id: repo_customer.id,
        email: repo_customer.email,
        name: repo_customer.name,
        salutation: repo_customer.salutation,
        first_name: repo_customer.first_name,
        last_name: repo_customer.last_name,
        phone: repo_customer.phone,
        customer_type: repo_customer.customer_type,
        company_name: repo_customer.company_name,
        billing_address_id: repo_customer.billing_address_id,
        billing_address,
        created_at: repo_customer.created_at,
        quotes,
        offers,
        termine,
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateCustomerRequest {
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    customer_type: Option<String>,
    company_name: Option<String>,
    /// Inline billing address — if provided, creates an address record and sets billing_address_id.
    /// If both billing_address and billing_address_id are provided, billing_address_id takes priority.
    billing_address: Option<super::inquiries::AddressInput>,
    billing_address_id: Option<Uuid>,
    /// Set to true to clear the customer's billing address override.
    clear_billing_address: Option<bool>,
}

/// `PATCH /api/v1/admin/customers/{id}` — Partially update a customer's contact fields.
///
/// **Caller**: Axum router / admin dashboard customer edit form.
/// **Why**: Allows correcting a customer's name, phone, or email without touching other
/// fields (COALESCE-based partial update).
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
/// - `request` — optional `name`, `phone`, `email` fields
///
/// # Returns
/// `200 OK` with updated `CustomerListItem`.
///
/// # Errors
/// - `404` if customer not found
pub(super) async fn update_customer(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateCustomerRequest>,
) -> Result<Json<CustomerListItem>, ApiError> {
    use crate::repositories::address_repo;

    // Create billing address if provided inline
    let resolved_billing_address_id = if let Some(ref addr) = request.billing_address {
        let street = addr.street.as_deref().unwrap_or("").trim().to_string();
        let city = addr.city.as_deref().unwrap_or("").trim().to_string();
        if !street.is_empty() || !city.is_empty() {
            let aid = address_repo::create(
                &state.db,
                &street,
                &city,
                addr.postal_code.as_deref(),
                addr.floor.as_deref(),
                addr.elevator,
                addr.house_number.as_deref(),
                addr.parking_ban,
            )
            .await?;
            Some(aid)
        } else {
            request.billing_address_id
        }
    } else {
        request.billing_address_id
    };

    // If clear_billing_address is true, set to None (explicitly clear the override)
    // Using Option<Option<Uuid>>: None = don't touch, Some(None) = clear, Some(Some(id)) = set
    let billing_address_id: Option<Option<Uuid>> = if request.clear_billing_address.unwrap_or(false) {
        Some(None) // clear
    } else {
        resolved_billing_address_id.map(Some) // set or don't touch
    };

    // For email updates: Option<&str> where None = don't touch, Some("value") = set value,
    // Some("") = clear to NULL. Trim the value; empty string after trim clears email.
    let email_update = request.email.as_deref().map(|s| s.trim());

    let repo_row = admin_repo::update_customer(
        &state.db, id,
        request.name.as_deref(), request.salutation.as_deref(),
        request.first_name.as_deref(), request.last_name.as_deref(),
        request.phone.as_deref(), email_update,
        request.customer_type.as_deref(), request.company_name.as_deref(),
        billing_address_id,
    )
    .await?;

    tracing::info!(admin = %claims.sub, admin_email = %claims.email, customer_id = %id, "Admin updated customer");

    repo_row
        .map(|c| Json(CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone,
            customer_type: c.customer_type, company_name: c.company_name, created_at: c.created_at,
        }))
        .ok_or_else(|| ApiError::NotFound(format!("Kunde {id} nicht gefunden")))
}

// --- Create Customer ---

#[derive(Debug, Deserialize)]
pub(super) struct CreateCustomerRequest {
    email: Option<String>,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
    customer_type: Option<String>,
    company_name: Option<String>,
}

/// `POST /api/v1/admin/customers` — Create a new customer record.
///
/// **Caller**: Axum router / admin dashboard "Neuer Kunde" form.
/// **Why**: Allows manually creating a customer before creating an inquiry for walk-in or
/// phone inquiries that bypass the email pipeline. Email is optional — older customers
/// who don't have email can still be registered.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `request` — JSON body with optional `email`, optional `name` and `phone`
///
/// # Returns
/// `201 Created` with the new `CustomerListItem` JSON.
///
/// # Errors
/// - `400` if a customer with the same email already exists
/// - `500` on other DB failures
pub(super) async fn create_customer(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(request): Json<CreateCustomerRequest>,
) -> Result<(axum::http::StatusCode, Json<CustomerListItem>), ApiError> {
    let id = Uuid::now_v7();
    let now = Utc::now();

    // Trim the email and treat empty strings as None (no email)
    let email = request.email.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let repo_row = admin_repo::create_customer(
        &state.db, id, email,
        request.name.as_deref(), request.salutation.as_deref(),
        request.first_name.as_deref(), request.last_name.as_deref(),
        request.phone.as_deref(), request.customer_type.as_deref(),
        request.company_name.as_deref(), now,
    )
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("customers_email_key") {
                return ApiError::Validation("E-Mail-Adresse existiert bereits".into());
            }
        }
        ApiError::Database(e)
    })?;

    repo_row
        .map(|c| (axum::http::StatusCode::CREATED, Json(CustomerListItem {
            id: c.id, email: c.email, name: c.name, salutation: c.salutation,
            first_name: c.first_name, last_name: c.last_name, phone: c.phone,
            customer_type: c.customer_type, company_name: c.company_name, created_at: c.created_at,
        })))
        .ok_or_else(|| ApiError::Internal("Kunde konnte nicht erstellt werden".into()))
}

/// `POST /api/v1/admin/customers/{id}/delete` — Hard-delete a customer and all their data.
///
/// **Caller**: Axum router / admin dashboard customer delete action.
/// **Why**: Cascades via FK to inquiries, offers, volume_estimations, email_threads, and
/// email_messages. Use for GDPR erasure requests. S3 objects (PDFs, images) are orphaned
/// and must be cleaned separately.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — customer UUID path parameter
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if customer not found
pub(super) async fn delete_customer(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&claims)?;
    // Cascades: inquiries, offers, volume_estimations, email_threads, email_messages
    let rows = admin_repo::delete_customer(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(format!("Kunde {id} nicht gefunden")));
    }
    tracing::info!(admin = %claims.sub, admin_email = %claims.email, customer_id = %id, "Admin deleted customer");
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Addresses ---

#[derive(Debug, Deserialize)]
pub(super) struct UpdateAddressRequest {
    street: Option<String>,
    house_number: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
    parking_ban: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(super) struct AddressResponse {
    id: Uuid,
    street: String,
    house_number: Option<String>,
    city: String,
    postal_code: Option<String>,
    floor: Option<String>,
    elevator: Option<bool>,
    parking_ban: bool,
}

/// `PATCH /api/v1/admin/addresses/{id}` — Partially update an address record.
///
/// **Caller**: Axum router / admin dashboard address edit form on the inquiry detail page.
/// **Why**: Allows correcting street, city, postal code, floor, or elevator without
/// creating a new address record. After editing, the admin typically uses the regenerate
/// endpoint to refresh the distance and regenerate the offer.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — address UUID path parameter
/// - `request` — partial update fields; omitted fields are unchanged
///
/// # Returns
/// `200 OK` with updated `AddressResponse`.
///
/// # Errors
/// - `404` if address not found
pub(super) async fn update_address(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateAddressRequest>,
) -> Result<Json<AddressResponse>, ApiError> {
    let repo_row = admin_repo::update_address(
        &state.db, id,
        request.street.as_deref(), request.house_number.as_deref(), request.city.as_deref(),
        request.postal_code.as_deref(), request.floor.as_deref(),
        request.elevator, request.parking_ban,
    )
    .await?;

    repo_row
        .map(|a| Json(AddressResponse {
            id: a.id, street: a.street, house_number: a.house_number, city: a.city,
            postal_code: a.postal_code, floor: a.floor, elevator: a.elevator, parking_ban: a.parking_ban,
        }))
        .ok_or_else(|| ApiError::NotFound(format!("Adresse {id} nicht gefunden")))
}
