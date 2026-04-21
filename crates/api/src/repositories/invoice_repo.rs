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
    /// Anzahlung percent stored on `partial_first`; NULL otherwise.
    pub deposit_percent: Option<i16>,
    /// FK to the sibling `partial_first` invoice, stored on `partial_final`; NULL otherwise.
    pub deposit_invoice_id: Option<Uuid>,
}

/// Minimal offer projection for invoice amount calculation.
#[derive(Debug, FromRow)]
pub(crate) struct ActiveOfferRow {
    pub price_cents: i64,
    pub offer_number: Option<String>,
    /// KVA line items stored as JSONB; NULL when no offer exists or for pre-migration offers.
    pub line_items_json: Option<serde_json::Value>,
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
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at,
                deposit_percent, deposit_invoice_id
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
        "SELECT price_cents, offer_number, line_items_json FROM offers WHERE inquiry_id = $1
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
/// **Why**: Creates the Anzahlung invoice with status `ready`. Also persists
/// `deposit_percent` so the final invoice can reference it without a sibling lookup.
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
            partial_group_id, partial_percent, deposit_percent, status, extra_services, pdf_s3_key, created_at)
         VALUES ($1,$2,$3,'partial_first',$4,$5,$5::smallint,'ready','[]',$6,$7)",
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
/// **Why**: Creates the Schlussrechnung with status `draft`. Stores `deposit_invoice_id`
/// (FK to the sibling `partial_first`) so the deduction line can reference the exact
/// Anzahlung invoice number without another DB round-trip.
pub(crate) async fn insert_partial_final(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    group_id: Uuid,
    percent: i32,
    first_id: Uuid,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            partial_group_id, partial_percent, deposit_invoice_id, status, extra_services, pdf_s3_key, created_at)
         VALUES ($1,$2,$3,'partial_final',$4,$5,$6,'draft','[]',$7,$8)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(group_id)
    .bind(percent)
    .bind(first_id)
    .bind(pdf_s3_key)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Fetch the invoice number of the partial_first sibling for a partial_final invoice.
///
/// **Caller**: `invoices::build_final_line_items` (PDF regeneration on PATCH)
/// **Why**: The Schlussrechnung needs to print "Abzüglich Anzahlung gemäß Rechnung Nr. {n}".
pub(crate) async fn fetch_deposit_invoice_number(
    pool: &PgPool,
    deposit_invoice_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT invoice_number FROM invoices WHERE id = $1")
            .bind(deposit_invoice_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(n,)| n))
}

/// Fetch the partial_first invoice number via group_id (fallback when deposit_invoice_id is NULL).
///
/// **Caller**: `invoices::build_final_line_items`
/// **Why**: Pre-migration rows don't have `deposit_invoice_id`; look up via `partial_group_id`.
pub(crate) async fn fetch_deposit_number_by_group(
    pool: &PgPool,
    group_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT invoice_number FROM invoices
         WHERE partial_group_id = $1 AND invoice_type = 'partial_first'",
    )
    .bind(group_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(n,)| n))
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
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at,
                deposit_percent, deposit_invoice_id
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
                partial_percent, status, extra_services, pdf_s3_key, sent_at, paid_at, created_at,
                deposit_percent, deposit_invoice_id
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

/// Fetch customer email and name for invoice email dispatch.
///
/// **Caller**: `invoices::send_invoice`
/// **Why**: Loads the recipient details for the invoice email.
pub(crate) async fn fetch_customer_for_invoice(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(Option<String>, Option<String>)>, sqlx::Error> {
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

/// Fetch the scheduled_date from an inquiry for invoice date display.
///
/// **Caller**: `invoices::load_invoice_context`
/// **Why**: Service date on the invoice comes from the inquiry's scheduled_date.
pub(crate) async fn fetch_moving_date(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<chrono::NaiveDate>, sqlx::Error> {
    let row: Option<(Option<chrono::NaiveDate>,)> =
        sqlx::query_as("SELECT scheduled_date FROM inquiries WHERE id = $1")
            .bind(inquiry_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(dt,)| dt))
}

/// Resolve the billing address ID for an inquiry.
///
/// **Caller**: `invoices::build_invoice_data` — determines which address goes on the invoice header.
/// **Why**: Priority order: explicit `billing_address_id` > destination (post-move) > origin.
///
/// # Returns
/// The UUID of the resolved address, or `None` if the inquiry has no addresses at all.
pub(crate) async fn resolve_billing_address_id(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(billing_address_id,
            CASE WHEN status IN ('completed','invoiced','paid') AND destination_address_id IS NOT NULL
                 THEN destination_address_id
                 ELSE origin_address_id
            END)
         FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
    .map(|opt: Option<Option<Uuid>>| opt.flatten())
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
