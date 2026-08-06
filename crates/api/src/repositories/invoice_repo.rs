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
    /// Base netto amount captured at creation (offer price or manual price).
    /// NULL on pre-migration rows — callers fall back to the active offer.
    pub base_netto_cents: Option<i64>,
    /// TRUE when Alex has taken over the line items by hand — the invoice is then
    /// rendered from `line_items_json` and never recomputed from the offer.
    pub is_manual: bool,
    /// Hand-edited line items (array of `ManualLineItem`), present only for
    /// manual invoices. NULL for offer-derived invoices.
    pub line_items_json: Option<serde_json::Value>,
}

/// Flat projection for Rechnungsausgangsbuch — one row per invoice with
/// customer name, service date, and everything needed to compute the invoice's
/// own amounts.
#[derive(Debug, FromRow)]
pub(crate) struct RechnungsausgangRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub invoice_number: String,
    pub invoice_type: String,
    pub partial_percent: Option<i32>,
    pub status: String,
    pub is_manual: bool,
    pub extra_services: serde_json::Value,
    pub line_items_json: Option<serde_json::Value>,
    pub base_netto_cents: Option<i64>,
    pub pdf_s3_key: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub payment_method: Option<String>,
    pub notes: Option<String>,
    pub due_date: Option<chrono::NaiveDate>,
    /// Netto amount from the active offer in cents — fallback for pre-migration
    /// rows whose `base_netto_cents` is NULL.
    pub offer_netto_cents: Option<i64>,
    pub customer_name: Option<String>,
    pub scheduled_date: Option<chrono::NaiveDate>,
}

/// Minimal offer projection for invoice amount calculation.
#[derive(Debug, FromRow)]
pub(crate) struct ActiveOfferRow {
    pub price_cents: i64,
    pub offer_number: Option<String>,
    /// KVA line items stored as JSONB; NULL when no offer exists or for pre-migration offers.
    pub line_items_json: Option<serde_json::Value>,
    /// Number of workers (J50 in the offer formula) — needed to recompute labor line totals
    /// on the invoice (`hours × rate × persons`).
    pub persons: Option<i32>,
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
                deposit_percent, deposit_invoice_id,
                base_netto_cents, is_manual, line_items_json
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
        "SELECT price_cents, offer_number, line_items_json, persons FROM offers WHERE inquiry_id = $1
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
// repository fn — args mirror DB columns
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_partial_first(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    group_id: Uuid,
    percent: i32,
    base_netto_cents: i64,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            partial_group_id, partial_percent, deposit_percent, status, extra_services, pdf_s3_key, created_at, base_netto_cents)
         VALUES ($1,$2,$3,'partial_first',$4,$5,$5::smallint,'ready','[]',$6,$7,$8)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(group_id)
    .bind(percent)
    .bind(pdf_s3_key)
    .bind(created_at)
    .bind(base_netto_cents)
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
// repository fn — args mirror DB columns
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_partial_final(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    inquiry_id: Uuid,
    invoice_number: &str,
    group_id: Uuid,
    percent: i32,
    first_id: Uuid,
    base_netto_cents: i64,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            partial_group_id, partial_percent, deposit_invoice_id, status, extra_services, pdf_s3_key, created_at, base_netto_cents)
         VALUES ($1,$2,$3,'partial_final',$4,$5,$6,'draft','[]',$7,$8,$9)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(group_id)
    .bind(percent)
    .bind(first_id)
    .bind(pdf_s3_key)
    .bind(created_at)
    .bind(base_netto_cents)
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
    base_netto_cents: i64,
    pdf_s3_key: &str,
    created_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type,
            status, extra_services, pdf_s3_key, created_at, base_netto_cents)
         VALUES ($1,$2,$3,'full','ready','[]',$4,$5,$6)",
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(invoice_number)
    .bind(pdf_s3_key)
    .bind(created_at)
    .bind(base_netto_cents)
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
                deposit_percent, deposit_invoice_id,
                base_netto_cents, is_manual, line_items_json
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
                deposit_percent, deposit_invoice_id,
                base_netto_cents, is_manual, line_items_json
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

/// The inquiry and customer behind an invoice.
///
/// **Caller**: `services::billing_reminders::mark_invoice_paid`
/// **Why**: The register's "Bezahlt" button only knows the invoice id, but the
/// review request hangs off the inquiry and the toast needs the customer's name.
/// Returns `None` when `inv_id` is not a core invoice (e.g. it's a storage one).
pub(crate) async fn fetch_inquiry_and_customer(
    pool: &PgPool,
    inv_id: Uuid,
) -> Result<Option<(Uuid, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT inv.inquiry_id, c.name AS customer_name
         FROM invoices inv
         JOIN inquiries i ON i.id = inv.inquiry_id
         LEFT JOIN customers c ON c.id = i.customer_id
         WHERE inv.id = $1",
    )
    .bind(inv_id)
    .fetch_optional(pool)
    .await
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

/// Overwrite an invoice's number.
///
/// **Caller**: `invoices::update_invoice_number`
/// **Why**: Recovery path when the system counter fell out of sync with manually-sent
/// invoices. The `invoices_invoice_number_key` UNIQUE constraint guards collisions;
/// the caller maps that violation to a friendly message.
pub(crate) async fn update_invoice_number(
    pool: &PgPool,
    inv_id: Uuid,
    invoice_number: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET invoice_number = $1 WHERE id = $2")
        .bind(invoice_number)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Set (or clear) an invoice's payment method.
///
/// **Caller**: `admin::update_payment_method` (Rechnungsausgangsbuch editor)
/// **Why**: `payment_method` is captured after the fact (Alex notes how the customer
/// actually paid), so it has no set path at invoice creation time.
pub(crate) async fn update_payment_method(
    pool: &PgPool,
    inv_id: Uuid,
    payment_method: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET payment_method = $1 WHERE id = $2")
        .bind(payment_method)
        .bind(inv_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Move `invoice_number_seq` forward so the next generated number is `> seq`.
///
/// **Caller**: `invoices::update_invoice_number`
/// **Why**: After an overwrite to a higher number, the auto-counter must catch up so
/// the next generated invoice doesn't collide. `GREATEST` guarantees the sequence
/// only ever advances — never rewinds (which would risk reusing a number).
pub(crate) async fn advance_invoice_sequence(
    pool: &PgPool,
    seq: i64,
) -> Result<(), sqlx::Error> {
    // setval(..., is_called = true) ⇒ next nextval() returns the set value + 1.
    sqlx::query(
        "SELECT setval('invoice_number_seq', GREATEST((SELECT last_value FROM invoice_number_seq), $1), true)",
    )
    .bind(seq)
    .execute(pool)
    .await?;
    Ok(())
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

/// Persist hand-edited line items and the manual flag on an invoice.
///
/// **Caller**: `invoices::update_invoice` (manual-line-items branch).
/// **Why**: Makes `line_items_json` the stored source of truth so the invoice is
/// rendered from these lines and the offer-derived rebuild is bypassed. Passing
/// `is_manual = false` with `line_items = NULL` reverts to offer-derived mode.
pub(crate) async fn update_line_items(
    pool: &PgPool,
    inv_id: Uuid,
    line_items: Option<&serde_json::Value>,
    is_manual: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE invoices SET line_items_json = $1, is_manual = $2 WHERE id = $3")
        .bind(line_items)
        .bind(is_manual)
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

/// Fetch the origin (service) address ID for an inquiry.
///
/// **Caller**: `invoices::load_invoice_context` — determines the Auftragsort.
/// **Why**: The Auftragsort on the invoice (A27) is where the service is performed
/// (origin address), which may differ from the billing address (Rechnungsadresse).
pub(crate) async fn fetch_origin_address_id(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT origin_address_id FROM inquiries WHERE id = $1")
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

/// List every invoice ever issued, with customer name and the fields needed to
/// compute its own amounts, for the Rechnungsausgangsbuch.
///
/// **Caller**: `admin::rechnungsausgangsbuch`
/// **Why**: A Rechnungsausgangsbuch is a legal register — once a number has been
/// issued the row must appear, whatever happened to the surrounding inquiry.
/// This query used to INNER JOIN `inquiries` filtered to
/// `status IN ('completed','invoiced','paid')` and INNER JOIN `customers`, so an
/// invoice whose inquiry sat in any other status (or whose customer row was gone)
/// silently vanished from the register — 43 of 80 issued numbers were missing in
/// production (feedback report a61982f1). Both joins are now LEFT joins and the
/// status filter is gone: `invoices` alone decides which rows exist.
pub(crate) async fn list_for_rechnungsausgangsbuch(
    pool: &PgPool,
) -> Result<Vec<RechnungsausgangRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT
            inv.id,
            inv.inquiry_id,
            inv.invoice_number,
            inv.invoice_type,
            inv.partial_percent,
            inv.status,
            inv.is_manual,
            inv.extra_services,
            inv.line_items_json,
            inv.base_netto_cents,
            inv.pdf_s3_key,
            inv.sent_at,
            inv.paid_at,
            inv.created_at,
            inv.payment_method,
            inv.notes,
            inv.due_date,
            off.price_cents AS offer_netto_cents,
            c.name AS customer_name,
            i.scheduled_date
         FROM invoices inv
         LEFT JOIN inquiries i ON i.id = inv.inquiry_id
         LEFT JOIN customers c ON c.id = i.customer_id
         LEFT JOIN offers off ON off.inquiry_id = inv.inquiry_id
             AND off.id = (SELECT o2.id FROM offers o2
                           WHERE o2.inquiry_id = inv.inquiry_id
                           ORDER BY o2.created_at DESC LIMIT 1)
         ORDER BY COALESCE(inv.sent_at, inv.created_at) ASC",
    )
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    /// Insert a bare invoice row directly — the register must not care how it got there.
    async fn insert_invoice(
        pool: &PgPool,
        inquiry_id: Uuid,
        number: &str,
        invoice_type: &str,
        status: &str,
        sent: bool,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type, status,
                extra_services, base_netto_cents, created_at, sent_at)
             VALUES ($1,$2,$3,$4,$5,'[]',100000, NOW(), CASE WHEN $6 THEN NOW() ELSE NULL END)",
        )
        .bind(id)
        .bind(inquiry_id)
        .bind(number)
        .bind(invoice_type)
        .bind(status)
        .bind(sent)
        .execute(pool)
        .await
        .expect("insert test invoice");
        id
    }

    async fn seed_inquiry(pool: &PgPool, status: &str) -> Uuid {
        let customer_id = test_helpers::insert_test_customer(pool).await;
        let origin = test_helpers::insert_test_address(
            pool, "Musterstr. 1", "Hildesheim", "31134", None, None,
        )
        .await;
        let dest = test_helpers::insert_test_address(
            pool, "Zielstr. 5", "Hannover", "30159", None, None,
        )
        .await;
        test_helpers::insert_test_inquiry_full(
            pool, customer_id, origin, dest, status, "termin", None,
        )
        .await
    }

    /// The register is a legal ledger: an issued number must appear regardless of what
    /// the surrounding inquiry is doing. The old query INNER JOINed `inquiries` filtered
    /// to `completed|invoiced|paid`, which hid 43 of 80 issued numbers in production
    /// (feedback report a61982f1).
    #[sqlx::test(migrations = "../../migrations")]
    async fn lists_invoices_whatever_the_inquiry_status_is(pool: PgPool) {
        // Two of these statuses were excluded by the old query.
        let scheduled = seed_inquiry(&pool, "scheduled").await;
        let cancelled = seed_inquiry(&pool, "cancelled").await;
        let invoiced = seed_inquiry(&pool, "invoiced").await;

        insert_invoice(&pool, scheduled, "2026-9001", "full", "sent", true).await;
        insert_invoice(&pool, cancelled, "2026-9002", "full", "sent", true).await;
        insert_invoice(&pool, invoiced, "2026-9003", "full", "sent", true).await;

        let rows = list_for_rechnungsausgangsbuch(&pool).await.unwrap();
        let numbers: Vec<&str> = rows.iter().map(|r| r.invoice_number.as_str()).collect();

        assert!(numbers.contains(&"2026-9001"), "scheduled inquiry dropped: {numbers:?}");
        assert!(numbers.contains(&"2026-9002"), "cancelled inquiry dropped: {numbers:?}");
        assert!(numbers.contains(&"2026-9003"), "invoiced inquiry dropped: {numbers:?}");
    }

    /// Guards the projection itself: the query is a runtime-typed `query_as`, so a
    /// renamed or retyped column only fails at request time. This pins every field the
    /// register handler reads.
    #[sqlx::test(migrations = "../../migrations")]
    async fn projects_every_column_the_register_renders(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool, "invoiced").await;
        test_helpers::insert_test_offer(&pool, inquiry_id, "accepted").await;
        insert_invoice(&pool, inquiry_id, "2026-9010", "partial_first", "sent", true).await;

        let rows = list_for_rechnungsausgangsbuch(&pool).await.unwrap();
        let row = rows
            .iter()
            .find(|r| r.invoice_number == "2026-9010")
            .expect("invoice missing from register");

        assert_eq!(row.inquiry_id, inquiry_id);
        assert_eq!(row.invoice_type, "partial_first");
        assert_eq!(row.status, "sent");
        assert!(!row.is_manual);
        assert_eq!(row.base_netto_cents, Some(100_000));
        assert_eq!(row.offer_netto_cents, Some(50_000)); // from the helper's offer
        assert!(row.customer_name.is_some());
        assert!(row.sent_at.is_some());
        assert!(row.extra_services.is_array());
    }
}
