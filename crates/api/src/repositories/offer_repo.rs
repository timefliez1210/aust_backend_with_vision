//! Offer repository — centralised queries for the `offers` table.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// Check whether any offer exists for an inquiry (any status).
///
/// **Caller**: `orchestrator::try_auto_generate_offer`
/// **Why**: Skip auto-generation if an offer already exists.
pub(crate) async fn any_exists_for_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM offers WHERE inquiry_id = $1 LIMIT 1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

/// Fetch the ID of the latest active (non-rejected, non-cancelled) offer for an inquiry.
///
/// **Caller**: `generate_inquiry_offer` (for UPDATE-in-place), `get_inquiry_pdf`
/// **Why**: Active offer lookup is used by multiple endpoints; centralises the status filter.
pub(crate) async fn fetch_active_id_for_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM offers WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled') ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Fetch the offer number and customer last name for building a human-readable filename.
///
/// **Caller**: `get_inquiry_pdf`, `send_draft_email`
/// **Why**: The download filename should be `{seq}-{year} {last_name}` (e.g. `131-2026 Krause`)
///          rather than a raw UUID. This provides the two pieces needed to build that string.
///
/// # Returns
/// `Some((offer_number, last_name))` when the offer and its customer exist, `None` otherwise.
/// `last_name` may be an empty string if the customer has no last_name on record.
pub(crate) async fn fetch_offer_filename_parts(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<(String, String)>, ApiError> {
    let row: Option<(String, String)> = sqlx::query_as(
        r#"
        SELECT COALESCE(o.offer_number, ''), COALESCE(c.last_name, '')
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(offer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Format an offer filename from offer_number and customer last name.
///
/// **Caller**: any route that serves or attaches an offer PDF
/// **Why**: Converts the internal `{year}-{seq:04}` offer_number (e.g. "2026-0131") into
///          the human-readable `{seq}-{year} {last_name}` format (e.g. "131-2026 Krause").
///
/// Falls back to `{offer_number} {last_name}` when offer_number cannot be parsed.
pub(crate) fn build_offer_filename(offer_number: &str, last_name: &str, ext: &str) -> String {
    let parts: Vec<&str> = offer_number.splitn(2, '-').collect();
    if parts.len() == 2 {
        let year = parts[0];
        let seq: u64 = parts[1].trim_start_matches('0').parse().unwrap_or(0);
        let name = if last_name.is_empty() { "Angebot" } else { last_name };
        format!("{seq}-{year} {name}.{ext}")
    } else {
        let name = if last_name.is_empty() { "Angebot" } else { last_name };
        format!("{offer_number} {name}.{ext}")
    }
}

/// Fetch the active offer's ID and PDF storage key for PDF download.
///
/// **Caller**: `get_inquiry_pdf`
/// **Why**: Downloads the latest active offer's PDF from S3.
pub(crate) async fn fetch_active_pdf_key(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(Uuid, Option<String>)>, ApiError> {
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, pdf_storage_key FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch the inquiry_id for a given offer.
///
/// **Caller**: `orchestrator::handle_offer_denial`, `run_offer_event_handler` (edit mode)
/// **Why**: The Telegram callback carries only the offer_id; we need the inquiry_id for status updates.
pub(crate) async fn fetch_inquiry_id(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT inquiry_id FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Fetch the offer_number for an existing offer (used during UPDATE-in-place).
///
/// **Caller**: `build_offer_with_overrides`
/// **Why**: When regenerating an offer, the offer number must be preserved.
pub(crate) async fn fetch_offer_number(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT offer_number FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(n,)| n))
}

/// Fetch the stored fahrt_override_cents for an existing offer.
///
/// **Caller**: `build_offer_with_overrides`
/// **Why**: Admin-set Fahrkostenpauschale overrides must be carried forward on regeneration.
pub(crate) async fn fetch_fahrt_override(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<i32>, ApiError> {
    let row: Option<(Option<i32>,)> =
        sqlx::query_as("SELECT fahrt_override_cents FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(c,)| c))
}

/// Fetch price_cents for an offer (used for LLM context in edit flow).
///
/// **Caller**: `orchestrator::fetch_current_offer_summary`
/// **Why**: The LLM prompt needs the current offer price.
pub(crate) async fn fetch_price(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT price_cents FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(p,)| p))
}

/// Get next offer number from the sequence.
///
/// **Caller**: `build_offer_with_overrides`
/// **Why**: New offers need a sequential offer number.
pub(crate) async fn next_offer_number(
    pool: &PgPool,
    today: chrono::NaiveDate,
) -> Result<String, sqlx::Error> {
    let (seq_val,): (i64,) = sqlx::query_as("SELECT nextval('offer_number_seq')")
        .fetch_one(pool)
        .await?;
    Ok(format!("{}-{:04}", today.format("%Y"), seq_val))
}

/// Mark an offer as rejected.
///
/// **Caller**: `handle_offer_denial`
/// **Why**: Telegram ❌ button rejects the offer.
pub(crate) async fn reject(pool: &PgPool, offer_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(offer_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Fetch customer email and inquiry_id for an offer (used in approval flow).
///
/// **Caller**: `handle_offer_approval`
/// **Why**: Approval needs the customer email for the draft and the inquiry_id for the thread.
pub(crate) async fn fetch_approval_context(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<(String, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT c.email, o.inquiry_id
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(offer_id)
    .fetch_optional(pool)
    .await
}

/// Volume estimation row for offer generation — lightweight projection.
#[derive(Debug, FromRow)]
pub(crate) struct VolumeEstimationRow {
    pub result_data: Option<serde_json::Value>,
    pub source_data: Option<serde_json::Value>,
    pub total_volume_m3: Option<f64>,
    pub method: String,
}

/// Fetch the latest volume estimation for an inquiry (for offer generation).
///
/// **Caller**: `offers::build_offer_with_overrides`
/// **Why**: Offer generation needs the detected items from the latest estimation.
pub(crate) async fn fetch_latest_estimation(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<VolumeEstimationRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT result_data, source_data, total_volume_m3, method
        FROM volume_estimations
        WHERE inquiry_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Full offer row returned by insert/update RETURNING.
#[derive(Debug, FromRow)]
pub(crate) struct OfferFullRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub price_cents: i64,
    pub currency: String,
    pub valid_until: Option<NaiveDate>,
    pub pdf_storage_key: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub offer_number: Option<String>,
    pub persons: Option<i32>,
    pub hours_estimated: Option<f64>,
    pub rate_per_hour_cents: Option<i64>,
    pub line_items_json: Option<serde_json::Value>,
    pub fahrt_override_cents: Option<i32>,
}

/// Update an existing offer and return the full row.
///
/// **Caller**: `offers::build_offer_with_overrides` (regenerate path)
/// **Why**: Regenerating an offer updates the price, PDF key, and pricing parameters.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_returning(
    pool: &PgPool,
    offer_id: Uuid,
    price_cents: i64,
    pdf_storage_key: Option<&str>,
    status: &str,
    persons: i32,
    hours_estimated: f64,
    rate_per_hour_cents: i64,
    line_items_json: &Option<serde_json::Value>,
    fahrt_override_cents: Option<i32>,
) -> Result<OfferFullRow, sqlx::Error> {
    sqlx::query_as(
        r#"
        UPDATE offers
        SET price_cents = $1, pdf_storage_key = $2, status = $3,
            persons = $4, hours_estimated = $5, rate_per_hour_cents = $6,
            line_items_json = $7,
            fahrt_override_cents = $8
        WHERE id = $9
        RETURNING id, inquiry_id, price_cents, currency, valid_until, pdf_storage_key, status,
                  created_at, sent_at, offer_number, persons, hours_estimated,
                  rate_per_hour_cents, line_items_json, fahrt_override_cents
        "#,
    )
    .bind(price_cents)
    .bind(pdf_storage_key)
    .bind(status)
    .bind(persons)
    .bind(hours_estimated)
    .bind(rate_per_hour_cents)
    .bind(line_items_json)
    .bind(fahrt_override_cents)
    .bind(offer_id)
    .fetch_one(pool)
    .await
}

/// Insert a new offer and return the full row.
///
/// **Caller**: `offers::build_offer_with_overrides` (new offer path)
/// **Why**: Creates the offer record with pricing, PDF key, and all line item data.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_returning(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    price_cents: i64,
    currency: &str,
    valid_until: Option<NaiveDate>,
    pdf_storage_key: Option<&str>,
    status: &str,
    now: DateTime<Utc>,
    offer_number: &str,
    persons: i32,
    hours_estimated: f64,
    rate_per_hour_cents: i64,
    line_items_json: &Option<serde_json::Value>,
    fahrt_override_cents: Option<i32>,
) -> Result<OfferFullRow, sqlx::Error> {
    sqlx::query_as(
        r#"
        INSERT INTO offers (id, inquiry_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at,
                            offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json,
                            fahrt_override_cents)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        RETURNING id, inquiry_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at,
                  offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json, fahrt_override_cents
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(price_cents)
    .bind(currency)
    .bind(valid_until)
    .bind(pdf_storage_key)
    .bind(status)
    .bind(now)
    .bind(offer_number)
    .bind(persons)
    .bind(hours_estimated)
    .bind(rate_per_hour_cents)
    .bind(line_items_json)
    .bind(fahrt_override_cents)
    .fetch_one(pool)
    .await
}

/// Fetch an inquiry row for offer generation (lightweight projection).
///
/// **Caller**: `offers::build_offer_with_overrides`
/// **Why**: Offer generation needs inquiry status, volume, distance, addresses, and services.
pub(crate) async fn fetch_inquiry_for_offer(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<crate::types::InquiryRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, stop_address_id,
               status, estimated_volume_m3, distance_km, preferred_date, notes, services,
               source, offer_sent_at, accepted_at, created_at, updated_at
        FROM inquiries WHERE id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Offer row projection for the inquiry builder.
#[derive(Debug, FromRow)]
pub(crate) struct OfferBuilderRow {
    pub id: Uuid,
    #[sqlx(default)]
    pub offer_number: Option<String>,
    pub price_cents: i64,
    pub status: String,
    pub persons: Option<i32>,
    pub hours_estimated: Option<f64>,
    pub rate_per_hour_cents: Option<i64>,
    pub line_items_json: Option<serde_json::Value>,
    pub pdf_storage_key: Option<String>,
    #[sqlx(default)]
    pub valid_until: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

/// Fetch the latest active offer for an inquiry (inquiry builder projection).
///
/// **Caller**: `inquiry_builder::build_inquiry_response`
/// **Why**: Inquiry detail includes the latest non-rejected/non-cancelled offer.
pub(crate) async fn fetch_active_for_builder(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<OfferBuilderRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, offer_number, price_cents, status, persons, hours_estimated,
               rate_per_hour_cents, line_items_json, pdf_storage_key, valid_until,
               created_at
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}
