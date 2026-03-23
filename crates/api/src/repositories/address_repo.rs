//! Address repository — centralised queries for the `addresses` table.

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// SQLx projection row for the `addresses` table used within offer generation.
#[derive(Debug, FromRow)]
pub(crate) struct AddressRow {
    #[allow(dead_code)]
    pub id: Uuid,
    pub street: String,
    pub city: String,
    pub postal_code: Option<String>,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
}

/// Fetch a single address by primary key (offer-generation projection).
///
/// **Caller**: `build_offer_with_overrides`, `orchestrator::try_auto_generate_offer`
/// **Why**: Offer generation needs street/city/floor/elevator for XLSX fields and pricing.
pub(crate) async fn fetch_by_id(
    pool: &PgPool,
    address_id: Uuid,
) -> Result<Option<AddressRow>, ApiError> {
    let row = sqlx::query_as(
        "SELECT id, street, city, postal_code, floor, elevator FROM addresses WHERE id = $1",
    )
    .bind(address_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch an optional address by ID — returns `None` when the ID itself is `None`.
///
/// **Caller**: `build_offer_with_overrides` for origin/destination/stop addresses
/// **Why**: Convenience wrapper that avoids repeated `if let Some(id)` boilerplate.
pub(crate) async fn fetch_optional(
    pool: &PgPool,
    address_id: Option<Uuid>,
) -> Result<Option<AddressRow>, ApiError> {
    match address_id {
        Some(id) => fetch_by_id(pool, id).await,
        None => Ok(None),
    }
}

/// Minimal address projection used for distance calculation in the orchestrator.
#[derive(Debug, FromRow)]
pub(crate) struct AddressStrRow {
    pub street: String,
    pub city: String,
    pub postal_code: Option<String>,
}

/// Fetch street/city/postal_code for an address (distance calculation).
///
/// **Caller**: `orchestrator::try_auto_generate_offer`
/// **Why**: ORS route calculation only needs the address string, not floor/elevator.
pub(crate) async fn fetch_street_city(
    pool: &PgPool,
    address_id: Uuid,
) -> Result<Option<AddressStrRow>, sqlx::Error> {
    sqlx::query_as("SELECT street, city, postal_code FROM addresses WHERE id = $1")
        .bind(address_id)
        .fetch_optional(pool)
        .await
}

/// Full address row including latitude, longitude, and country.
#[derive(Debug, FromRow)]
pub(crate) struct AddressFullRow {
    pub id: Uuid,
    pub street: String,
    pub city: String,
    pub postal_code: Option<String>,
    #[sqlx(default)]
    pub country: String,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
    #[sqlx(default)]
    pub latitude: Option<f64>,
    #[sqlx(default)]
    pub longitude: Option<f64>,
}

/// Fetch a full address row by ID including lat/lng/country.
///
/// **Caller**: `inquiry_builder::fetch_address`
/// **Why**: Inquiry detail needs the full address including geo-coordinates.
pub(crate) async fn fetch_full(
    pool: &PgPool,
    address_id: Uuid,
) -> Result<Option<AddressFullRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, street, city, postal_code, country, floor, elevator, latitude, longitude
        FROM addresses WHERE id = $1
        "#,
    )
    .bind(address_id)
    .fetch_optional(pool)
    .await
}


/// Insert a new address and return its ID.
///
/// **Caller**: `create_inquiry`, `handle_submission`, `handle_complete_inquiry`
/// **Why**: Multiple entry points create addresses; centralises the INSERT.
pub(crate) async fn create(
    pool: &PgPool,
    street: &str,
    city: &str,
    postal_code: Option<&str>,
    floor: Option<&str>,
    elevator: Option<bool>,
) -> Result<Uuid, sqlx::Error> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(street)
    .bind(city)
    .bind(postal_code)
    .bind(floor)
    .bind(elevator)
    .fetch_one(pool)
    .await?;
    Ok(id)
}
