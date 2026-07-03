//! Customer address book repository — the reusable per-customer address catalogue.
//!
//! Rows are self-contained copies (not FKs into `addresses`); see the
//! `customer_addresses` migration for the rationale. Entries are harvested from
//! inquiry addresses on creation, added manually in the admin customer view, and
//! (future) extracted from correspondence.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// A single address-book entry for a customer.
#[derive(Debug, FromRow)]
pub(crate) struct CustomerAddressRow {
    pub id: Uuid,
    pub street: String,
    pub house_number: Option<String>,
    pub postal_code: Option<String>,
    pub city: String,
    pub country: String,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
    pub parking_ban: bool,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub label: Option<String>,
    pub source: String,
    pub last_used_at: DateTime<Utc>,
}

/// List a customer's known addresses, most-recently-used first.
///
/// **Caller**: `admin_customers::get_customer`, `list_customer_addresses`.
/// **Why**: Renders the customer's address book and feeds the inquiry-create picker.
pub(crate) async fn list_for_customer(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Vec<CustomerAddressRow>, ApiError> {
    let rows = sqlx::query_as(
        r#"
        SELECT id, street, house_number, postal_code, city, country,
               floor, elevator, parking_ban, latitude, longitude,
               label, source, last_used_at
        FROM customer_addresses
        WHERE customer_id = $1
        ORDER BY last_used_at DESC, created_at DESC
        "#,
    )
    .bind(customer_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Insert an address-book entry, or refresh an existing matching one.
///
/// Deduplication follows the `uniq_customer_address` index (case-insensitive
/// street/city, coalesced house_number/postal_code). On a match we bump
/// `last_used_at` and backfill any logistics/geo fields that were previously
/// unknown — so harvesting the same address again enriches rather than duplicates.
///
/// **Caller**: `create_inquiry` harvest, `update_customer` billing harvest,
///             `add_customer_address` (manual).
/// **Why**: Single dedup-aware entry point for all book writes.
// repository fn — args mirror DB columns
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    customer_id: Uuid,
    street: &str,
    house_number: Option<&str>,
    postal_code: Option<&str>,
    city: &str,
    country: Option<&str>,
    floor: Option<&str>,
    elevator: Option<bool>,
    parking_ban: bool,
    latitude: Option<f64>,
    longitude: Option<f64>,
    label: Option<&str>,
    source: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let (id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO customer_addresses (
            id, customer_id, street, house_number, postal_code, city, country,
            floor, elevator, parking_ban, latitude, longitude, label, source,
            last_used_at, created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, 'Deutschland'),
                $8, $9, $10, $11, $12, $13, $14, $15, $15)
        ON CONFLICT (customer_id, lower(street), coalesce(house_number, ''),
                     coalesce(postal_code, ''), lower(city))
        DO UPDATE SET
            floor        = COALESCE(customer_addresses.floor, EXCLUDED.floor),
            elevator     = COALESCE(customer_addresses.elevator, EXCLUDED.elevator),
            parking_ban  = customer_addresses.parking_ban OR EXCLUDED.parking_ban,
            latitude     = COALESCE(customer_addresses.latitude, EXCLUDED.latitude),
            longitude    = COALESCE(customer_addresses.longitude, EXCLUDED.longitude),
            label        = COALESCE(customer_addresses.label, EXCLUDED.label),
            last_used_at = EXCLUDED.last_used_at
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(customer_id)
    .bind(street)
    .bind(house_number)
    .bind(postal_code)
    .bind(city)
    .bind(country)
    .bind(floor)
    .bind(elevator)
    .bind(parking_ban)
    .bind(latitude)
    .bind(longitude)
    .bind(label)
    .bind(source)
    .bind(now)
    .fetch_one(executor)
    .await?;
    Ok(id)
}

/// Delete a single address-book entry belonging to a customer.
///
/// **Caller**: `delete_customer_address`.
/// **Why**: Admin removes a stale/wrong address from a customer's book.
/// Scoped by `customer_id` so one customer's id can't delete another's entry.
///
/// # Returns
/// Number of rows deleted (0 if not found / not owned by this customer).
pub(crate) async fn delete(
    pool: &PgPool,
    customer_id: Uuid,
    id: Uuid,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM customer_addresses WHERE id = $1 AND customer_id = $2")
        .bind(id)
        .bind(customer_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
