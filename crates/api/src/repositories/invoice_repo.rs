//! Invoice repository — centralised queries for `invoices` and related tables.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ── Row types ────────────────────────────────────────────────────────────────

/// Full projection of an invoice row.
#[derive(Debug, FromRow)]
pub(crate) struct InvoiceRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub invoice_number: String,
    pub invoice_type: String,
    pub partial_group_id: Option<Uuid>,
    pub partial_percent: Option<i32>,
    pub status: String,
    pub extra_services: serde_json::Value,
    pub pdf_s3_key: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Minimal offer projection for invoice amount calculation.
#[derive(Debug, FromRow)]
pub(crate) struct ActiveOfferRow {
    pub price_cents: i64,
    pub offer_number: Option<String>,
}

/// Address projection for invoice display.
#[derive(Debug, FromRow)]
pub(crate) struct InvoiceAddressRow {
    pub street: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
}

// ── Queries ──────────────────────────────────────────────────────────────────

/// List all invoices for an inquiry, ordered by creation date.
///
/// **Caller**: `invoices::list_invoices`
/// **Why**: Returns every invoice (full or partial pair) for the given inquiry.
pub(crate) async fn list_by_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<InvoiceRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, inquiry_id, invoice_number, invoice_type, partial_group_id,
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at
         FROM invoices WHERE inquiry_id = $1 ORDER BY created_at",
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await
}

/// Fetch the most recent offer for an inquiry (price + number).
///
/// **Caller**: `invoices::list_invoices`, `invoices::load_invoice_context`
/// **Why**: Invoice amounts are derived from the offer price.
pub(crate) async fn fetch_active_offer(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<ActiveOfferRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT price_cents, offer_number FROM offers WHERE inquiry_id = $1
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Fetch inquiry status by ID.
///
/// **Caller**: `invoices::create_invoice`, `invoices::send_invoice`
/// **Why**: Validates inquiry is in a sendable/creatable state.
pub(crate) async fn fetch_inquiry_status(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM inquiries WHERE id = $1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(s,)| s))
}

/// Count existing invoices for an inquiry.
///
/// **Caller**: `invoices::create_invoice`
/// **Why**: Guards against creating duplicate invoices.
pub(crate) async fn count_by_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM invoices WHERE inquiry_id = $1")
            .bind(inquiry_id)
            .fetch_one(pool)
            .await?;
    Ok(count)
}

/// Allocate N sequential invoice numbers in a single round-trip.
///
/// **Caller**: `invoices::create_invoice`
/// **Why**: Avoids sequence gaps on partial failures by fetching all needed numbers at once.
pub(crate) async fn next_invoice_numbers(
    pool: &PgPool,
    count: usize,
) -> Result<Vec<i64>, sqlx::Error> {
    match count {
        1 => {
            let (v,): (i64,) =
                sqlx::query_as("SELECT nextval('invoice_number_seq')")
                    .fetch_one(pool)
                    .await?;
            Ok(vec![v])
        }
        2 => {
            let (v1, v2): (i64, i64) =
                sqlx::query_as("SELECT nextval('invoice_number_seq'), nextval('invoice_number_seq')")
                    .fetch_one(pool)
                    .await?;
            Ok(vec![v1, v2])
        }
        _ => {
            // Fallback for arbitrary counts
            let mut vals = Vec::with_capacity(count);
            for _ in 0..count {
                let (v,): (i64,) =
                    sqlx::query_as("SELECT nextval('invoice_number_seq')")
                        .fetch_one(pool)
                        .await?;
                vals.push(v);
            }
            Ok(vals)
        }
    }
}

/// Insert a partial_first invoice row.
///
/// **Caller**: `invoices::create_invoice` (partial flow)
/// **Why**: Creates the Anzahlung invoice with status `ready`.
pub(crate) async fn insert_partial_first(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    group_id: Uuid,
    percent: i32,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            partial_group_id, partial_percent, status, extra_services, pdf_s3_key, created_at)
         VALUES ($1,$2,$3,'partial_first',$4,$5,'ready','[]',$6,$7)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(group_id)
    .bind(percent)
    .bind(pdf_s3_key)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Insert a partial_final invoice row.
///
/// **Caller**: `invoices::create_invoice` (partial flow)
/// **Why**: Creates the Restbetrag invoice with status `draft`.
pub(crate) async fn insert_partial_final(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    group_id: Uuid,
    percent: i32,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            partial_group_id, partial_percent, status, extra_services, pdf_s3_key, created_at)
         VALUES ($1,$2,$3,'partial_final',$4,$5,'draft','[]',$6,$7)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(group_id)
    .bind(percent)
    .bind(pdf_s3_key)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Insert a full invoice row.
///
/// **Caller**: `invoices::create_invoice` (full flow)
/// **Why**: Creates a single full invoice with status `ready`.
pub(crate) async fn insert_full(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            status, extra_services, pdf_s3_key, created_at)
         VALUES ($1,$2,$3,'full','ready','[]',$4,$5)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(pdf_s3_key)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a single invoice by ID.
///
/// **Caller**: `invoices::fetch_invoice_row`
/// **Why**: Returns the full invoice row after creation or update.
pub(crate) async fn fetch_by_id(
    pool: &PgPool,
    inv_id: Uuid,
) -> Result<Option<InvoiceRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, inquiry_id, invoice_number, invoice_type, partial_group_id,
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at
         FROM invoices WHERE id = $1",
    )
    .bind(inv_id)
    .fetch_optional(pool)
    .await
}

/// Fetch a single invoice by ID + inquiry_id (ownership check).
///
/// **Caller**: `invoices::get_invoice`, `invoices::update_invoice`, `invoices::send_invoice`
/// **Why**: Validates that the invoice belongs to the given inquiry.
pub(crate) async fn fetch_by_id_and_inquiry(
    pool: &PgPool,
    inv_id: Uuid,
    inquiry_id: Uuid,
) -> Result<Option<InvoiceRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, inquiry_id, invoice_number, invoice_type, partial_group_id,
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at
         FROM invoices WHERE id = $1 AND inquiry_id = $2",
    )
    .bind(inv_id)
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Fetch PDF key + invoice number for PDF download.
///
/// **Caller**: `invoices::get_invoice_pdf`
/// **Why**: Minimal projection for the download endpoint.
pub(crate) async fn fetch_pdf_key(
    pool: &PgPool,
    inv_id: Uuid,
    inquiry_id: Uuid,
) -> Result<Option<(Option<String>, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT pdf_s3_key, invoice_number FROM invoices WHERE id = $1 AND inquiry_id = $2",
    )
    .bind(inv_id)
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Mark an invoice as paid.
///
/// **Caller**: `invoices::update_invoice`
/// **Why**: Sets paid_at timestamp and status.
pub(crate) async fn mark_paid(
    pool: &PgPool,
    inv_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET status = 'paid', paid_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Count unpaid invoices for an inquiry.
///
/// **Caller**: `invoices::update_invoice`
/// **Why**: Determines if inquiry should auto-transition to `paid`.
pub(crate) async fn count_unpaid(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM invoices WHERE inquiry_id = $1 AND status != 'paid'")
            .bind(inquiry_id)
            .fetch_one(pool)
            .await?;
    Ok(count)
}

/// Transition inquiry to paid if not already.
///
/// **Caller**: `invoices::update_invoice`
/// **Why**: Auto-transitions inquiry status when all invoices are paid.
pub(crate) async fn transition_inquiry_to_paid(
    pool: &PgPool,
    inquiry_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inquiries SET status = 'paid', updated_at = $1 WHERE id = $2 AND status != 'paid'",
    )
    .bind(now)
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update extra_services JSON on an invoice.
///
/// **Caller**: `invoices::update_invoice`
/// **Why**: Persists the updated extra services list before PDF regeneration.
pub(crate) async fn update_extra_services(
    pool: &PgPool,
    inv_id: Uuid,
    extra_services: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET extra_services = $1 WHERE id = $2")
        .bind(extra_services)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update the PDF S3 key on an invoice.
///
/// **Caller**: `invoices::update_invoice`
/// **Why**: Stores the new S3 key after PDF regeneration.
pub(crate) async fn update_pdf_key(
    pool: &PgPool,
    inv_id: Uuid,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET pdf_s3_key = $1 WHERE id = $2")
        .bind(key)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Fetch partial_percent from a sibling partial_first invoice.
///
/// **Caller**: `invoices::compute_invoice_amounts`
/// **Why**: Fallback lookup for partial_final base netto calculation.
pub(crate) async fn fetch_sibling_percent(
    pool: &PgPool,
    group_id: Uuid,
) -> Result<Option<i32>, sqlx::Error> {
    let row: Option<(Option<i32>,)> = sqlx::query_as(
        "SELECT partial_percent FROM invoices
         WHERE partial_group_id = $1 AND invoice_type = 'partial_first'",
    )
    .bind(group_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(p,)| p))
}

/// Fetch customer email and name for invoice email dispatch.
///
/// **Caller**: `invoices::send_invoice`
/// **Why**: Loads the recipient details for the invoice email.
pub(crate) async fn fetch_customer_for_invoice(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(String, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT c.email, c.name FROM customers c
         JOIN inquiries i ON i.customer_id = c.id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Mark invoice as sent.
///
/// **Caller**: `invoices::send_invoice`
/// **Why**: Updates status and sent_at after email dispatch.
pub(crate) async fn mark_sent(
    pool: &PgPool,
    inv_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET status = 'sent', sent_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Auto-transition inquiry to invoiced if in an earlier stage.
///
/// **Caller**: `invoices::send_invoice`
/// **Why**: Advances the inquiry lifecycle after invoice dispatch.
pub(crate) async fn transition_inquiry_to_invoiced(
    pool: &PgPool,
    inquiry_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inquiries SET status = 'invoiced', updated_at = $1 WHERE id = $2
         AND status IN ('accepted','scheduled','completed')",
    )
    .bind(now)
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the preferred_date from an inquiry for invoice date display.
///
/// **Caller**: `invoices::load_invoice_context`
/// **Why**: Service date on the invoice comes from the inquiry's preferred_date.
pub(crate) async fn fetch_moving_date(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<chrono::NaiveDate>, sqlx::Error> {
    let row: Option<(Option<DateTime<Utc>>,)> =
        sqlx::query_as("SELECT preferred_date FROM inquiries WHERE id = $1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(dt,)| dt).map(|dt| dt.date_naive()))
}

/// Fetch origin address for invoice display.
///
/// **Caller**: `invoices::load_invoice_context`
/// **Why**: Invoice PDF includes the origin address.
pub(crate) async fn fetch_origin_address(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<InvoiceAddressRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.street, a.city, a.postal_code
         FROM addresses a
         JOIN inquiries i ON i.origin_address_id = a.id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Fetch the latest offer netto price for an inquiry.
///
/// **Caller**: `invoices::get_offer_netto`
/// **Why**: Display amounts on invoice responses are derived from the offer price.
pub(crate) async fn fetch_offer_netto(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT price_cents FROM offers WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(c,)| c).unwrap_or(0))
}
