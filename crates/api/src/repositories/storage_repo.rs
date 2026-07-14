//! Storage-rental ("Lagerung") repository — contracts + their monthly invoices.
//!
//! Deliberately isolated from `invoice_repo`/`inquiry_repo`: storage invoices are
//! their own entity and only share the invoice-number sequence (allocated via
//! `invoice_repo::next_invoice_numbers`). `sqm` is a NUMERIC column, cast to/from
//! `float8` at the SQL boundary so we don't pull in a decimal dependency.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::ApiError;

/// A storage-rental contract row.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)] // complete row projection; some fields are read only by future callers
pub(crate) struct StorageContractRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub billing_address_id: Option<Uuid>,
    pub contract_start: NaiveDate,
    pub contract_end: Option<NaiveDate>,
    pub sqm: f64,
    pub monthly_netto_cents: i64,
    pub billing_day: i16,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Contract joined with its customer, for dashboard listing.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)] // customer_email surfaced for the dashboard picker/contact view
pub(crate) struct StorageContractListRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub contract_start: NaiveDate,
    pub contract_end: Option<NaiveDate>,
    pub sqm: f64,
    pub monthly_netto_cents: i64,
    pub billing_day: i16,
    pub status: String,
    pub note: Option<String>,
}

/// A monthly storage invoice row.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)] // complete row projection; approve/send reads a subset
pub(crate) struct StorageInvoiceRow {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub invoice_number: String,
    pub period_year: i32,
    pub period_month: i32,
    pub netto_cents: i64,
    pub pdf_s3_key: Option<String>,
    pub status: String,
    pub payment_method: Option<String>,
    pub created_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
}

/// Invoice joined with contract + customer, for dashboard listing.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct StorageInvoiceListRow {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub invoice_number: String,
    pub period_year: i32,
    pub period_month: i32,
    pub netto_cents: i64,
    pub status: String,
    pub payment_method: Option<String>,
    pub pdf_s3_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub customer_name: Option<String>,
    pub sqm: f64,
}

const CONTRACT_COLS: &str = "id, customer_id, billing_address_id, contract_start, contract_end, \
     sqm::float8 AS sqm, monthly_netto_cents, billing_day, status, note, created_at, updated_at";

const INVOICE_COLS: &str = "id, contract_id, invoice_number, period_year, period_month, \
     netto_cents, pdf_s3_key, status, payment_method, created_at, approved_at, sent_at";

// ── Contracts ───────────────────────────────────────────────────────────────

/// Insert a storage contract. `billing_day` is derived by the caller (clamped ≤28).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_contract(
    pool: &PgPool,
    customer_id: Uuid,
    billing_address_id: Option<Uuid>,
    contract_start: NaiveDate,
    contract_end: Option<NaiveDate>,
    sqm: f64,
    monthly_netto_cents: i64,
    billing_day: i16,
    note: Option<&str>,
) -> Result<StorageContractRow, ApiError> {
    let sql = format!(
        "INSERT INTO storage_contracts
            (customer_id, billing_address_id, contract_start, contract_end, sqm,
             monthly_netto_cents, billing_day, note)
         VALUES ($1,$2,$3,$4,$5::numeric,$6,$7,$8)
         RETURNING {CONTRACT_COLS}"
    );
    let row = sqlx::query_as(&sql)
        .bind(customer_id)
        .bind(billing_address_id)
        .bind(contract_start)
        .bind(contract_end)
        .bind(sqm)
        .bind(monthly_netto_cents)
        .bind(billing_day)
        .bind(note)
        .fetch_one(pool)
        .await?;
    Ok(row)
}

/// Update the editable fields of a contract. All non-status fields are overwritten
/// from the request; `billing_day` is recomputed by the caller from `contract_start`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_contract(
    pool: &PgPool,
    id: Uuid,
    billing_address_id: Option<Uuid>,
    contract_start: NaiveDate,
    contract_end: Option<NaiveDate>,
    sqm: f64,
    monthly_netto_cents: i64,
    billing_day: i16,
    status: &str,
    note: Option<&str>,
) -> Result<StorageContractRow, ApiError> {
    let sql = format!(
        "UPDATE storage_contracts SET
            billing_address_id = $2, contract_start = $3, contract_end = $4,
            sqm = $5::numeric, monthly_netto_cents = $6, billing_day = $7,
            status = $8, note = $9
         WHERE id = $1
         RETURNING {CONTRACT_COLS}"
    );
    let row: Option<StorageContractRow> = sqlx::query_as(&sql)
        .bind(id)
        .bind(billing_address_id)
        .bind(contract_start)
        .bind(contract_end)
        .bind(sqm)
        .bind(monthly_netto_cents)
        .bind(billing_day)
        .bind(status)
        .bind(note)
        .fetch_optional(pool)
        .await?;
    row.ok_or_else(|| ApiError::NotFound(format!("Storage contract {id} not found")))
}

/// Delete a contract. Fails if invoices reference it (FK RESTRICT) — the route
/// maps that to a friendly 409-style error.
pub(crate) async fn delete_contract(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM storage_contracts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

pub(crate) async fn fetch_contract(pool: &PgPool, id: Uuid) -> Result<StorageContractRow, ApiError> {
    let sql = format!("SELECT {CONTRACT_COLS} FROM storage_contracts WHERE id = $1");
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Storage contract {id} not found")))
}

pub(crate) async fn list_contracts(pool: &PgPool) -> Result<Vec<StorageContractListRow>, ApiError> {
    let rows = sqlx::query_as(
        "SELECT sc.id, sc.customer_id, c.name AS customer_name, c.email AS customer_email,
                sc.contract_start, sc.contract_end, sc.sqm::float8 AS sqm,
                sc.monthly_netto_cents, sc.billing_day, sc.status, sc.note
         FROM storage_contracts sc
         JOIN customers c ON c.id = sc.customer_id
         ORDER BY sc.status = 'active' DESC, sc.contract_start DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// All `active` contracts — the billing tick iterates these.
pub(crate) async fn list_active_contracts(pool: &PgPool) -> Result<Vec<StorageContractRow>, sqlx::Error> {
    let sql = format!("SELECT {CONTRACT_COLS} FROM storage_contracts WHERE status = 'active'");
    sqlx::query_as(&sql).fetch_all(pool).await
}

// ── Invoices ────────────────────────────────────────────────────────────────

/// Has this contract already been billed for `(year, month)`?
///
/// **Caller**: `services::storage_billing_service::generate_invoice`, before it
/// draws an invoice number.
/// **Why**: the billing tick runs hourly and a contract stays "due" for the rest of
/// the month after its billing day. Letting it reach `insert_invoice` (which is
/// idempotent, but only *after* a number has been drawn) burned one value of the
/// shared `invoice_number_seq` on every tick — hundreds of phantom gaps per contract
/// per month in the Rechnungsausgangsbuch's sequential numbering.
pub(crate) async fn period_billed(
    pool: &PgPool,
    contract_id: Uuid,
    period_year: i32,
    period_month: i32,
) -> Result<bool, sqlx::Error> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT 1 FROM storage_invoices
         WHERE contract_id = $1 AND period_year = $2 AND period_month = $3",
    )
    .bind(contract_id)
    .bind(period_year)
    .bind(period_month)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Insert a storage invoice for a billing period. Returns the new row's id, or
/// `None` when a row for `(contract_id, year, month)` already exists (the UNIQUE
/// constraint makes the billing tick idempotent).
pub(crate) async fn insert_invoice(
    pool: &PgPool,
    contract_id: Uuid,
    invoice_number: &str,
    period_year: i32,
    period_month: i32,
    netto_cents: i64,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO storage_invoices
            (contract_id, invoice_number, period_year, period_month, netto_cents)
         VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (contract_id, period_year, period_month) DO NOTHING
         RETURNING id",
    )
    .bind(contract_id)
    .bind(invoice_number)
    .bind(period_year)
    .bind(period_month)
    .bind(netto_cents)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

pub(crate) async fn set_invoice_pdf_key(pool: &PgPool, id: Uuid, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE storage_invoices SET pdf_s3_key = $2 WHERE id = $1")
        .bind(id)
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn fetch_invoice(pool: &PgPool, id: Uuid) -> Result<StorageInvoiceRow, ApiError> {
    let sql = format!("SELECT {INVOICE_COLS} FROM storage_invoices WHERE id = $1");
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Storage invoice {id} not found")))
}

/// List invoices for the dashboard, optionally filtered by status.
pub(crate) async fn list_invoices(
    pool: &PgPool,
    status: Option<&str>,
) -> Result<Vec<StorageInvoiceListRow>, ApiError> {
    let base = "SELECT si.id, si.contract_id, si.invoice_number, si.period_year, si.period_month,
                si.netto_cents, si.status, si.payment_method, si.pdf_s3_key, si.created_at,
                c.name AS customer_name, sc.sqm::float8 AS sqm
         FROM storage_invoices si
         JOIN storage_contracts sc ON sc.id = si.contract_id
         JOIN customers c ON c.id = sc.customer_id";
    let rows = match status {
        Some(s) => {
            sqlx::query_as(&format!("{base} WHERE si.status = $1 ORDER BY si.created_at DESC"))
                .bind(s)
                .fetch_all(pool)
                .await?
        }
        None => {
            sqlx::query_as(&format!("{base} ORDER BY si.created_at DESC"))
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

/// Mark an invoice approved + sent (both timestamps set in one write).
pub(crate) async fn mark_invoice_approved_sent(
    pool: &PgPool,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE storage_invoices SET status = 'sent', approved_at = $2, sent_at = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Reject (cancel) a pending invoice.
pub(crate) async fn mark_invoice_rejected(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE storage_invoices SET status = 'cancelled' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Row for the shared Rechnungsausgangsbuch (invoice register). Storage invoices
/// share the invoice-number sequence and must appear in the legal register.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct StorageRegisterRow {
    pub id: Uuid,
    pub invoice_number: String,
    pub netto_cents: i64,
    pub status: String,
    pub payment_method: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub customer_name: Option<String>,
    pub period_year: i32,
    pub period_month: i32,
}

/// All non-cancelled storage invoices for the Rechnungsausgangsbuch.
pub(crate) async fn list_for_register(pool: &PgPool) -> Result<Vec<StorageRegisterRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT si.id, si.invoice_number, si.netto_cents, si.status, si.payment_method,
                si.sent_at, si.paid_at, si.created_at, c.name AS customer_name,
                si.period_year, si.period_month
         FROM storage_invoices si
         JOIN storage_contracts sc ON sc.id = si.contract_id
         JOIN customers c ON c.id = sc.customer_id
         WHERE si.status <> 'cancelled'",
    )
    .fetch_all(pool)
    .await
}

/// Mark a storage invoice paid. Returns rows affected — 0 means `id` is not a
/// storage invoice, which is how the register's shared paid-endpoint decides
/// whether to fall through to the core `invoices` table.
pub(crate) async fn mark_invoice_paid(
    pool: &PgPool,
    id: Uuid,
    paid_at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE storage_invoices SET status = 'paid', paid_at = $2 \
         WHERE id = $1 AND status <> 'cancelled'",
    )
    .bind(id)
    .bind(paid_at)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

pub(crate) async fn set_invoice_payment_method(
    pool: &PgPool,
    id: Uuid,
    method: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("UPDATE storage_invoices SET payment_method = $2 WHERE id = $1")
        .bind(id)
        .bind(method)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
