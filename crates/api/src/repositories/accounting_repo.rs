//! Accounting repository — queries behind the Gewinn tab.
//!
//! Tables: `expense_categories`, `expenses`, `recurring_expenses`, `labor_months`,
//! `accounting_audit_log` (migration `20261002120000_profit_accounting.sql`), plus
//! read-only bulk reads over the crew-hours tables for the labor-cost projection.
//!
//! This is Alex's personal controlling, not tax bookkeeping. It is kept GoBD-ready:
//! every write goes through [`log`] and a correction defaults to a Storno row —
//! including the bulk ones: generated Dauerauftrag drafts (actor `dauerauftrag`) and
//! the drafts a template edit/delete re-syncs or removes get one entry per row.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::ApiError;

// ── Row types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, FromRow)]
pub(crate) struct CategoryRow {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub default_vat_rate: i16,
    pub skr03_account: Option<String>,
    pub sort_order: i32,
    pub active: bool,
    /// Loaded onto the Stundensatz-Kalkulation. FALSE for costs billed or covered
    /// separately (fuel via the Fahrkostenpauschale).
    pub in_hourly_rate: bool,
    /// KVA/invoice position names through which customers pay for this cost (e.g.
    /// Kraftstoff → Fahrkostenpauschale). Empty = Eigene Kosten, loaded onto the rate.
    pub recharge_positions: Vec<String>,
}

/// An expense joined with the display names the list needs.
#[derive(Debug, Clone, Serialize, FromRow)]
pub(crate) struct ExpenseRow {
    pub id: Uuid,
    pub category_id: Uuid,
    pub category_name: String,
    pub category_kind: String,
    pub status: String,
    pub receipt_date: NaiveDate,
    pub paid_on: Option<NaiveDate>,
    pub period_month: NaiveDate,
    pub supplier: Option<String>,
    pub receipt_number: Option<String>,
    pub description: Option<String>,
    pub netto_cents: i64,
    pub vat_rate: i16,
    pub vat_cents: i64,
    pub brutto_cents: i64,
    pub vehicle_id: Option<Uuid>,
    pub vehicle_label: Option<String>,
    pub inquiry_id: Option<Uuid>,
    pub inquiry_label: Option<String>,
    pub employee_id: Option<Uuid>,
    pub employee_name: Option<String>,
    pub recurring_id: Option<Uuid>,
    pub receipt_s3_key: Option<String>,
    pub receipt_filename: Option<String>,
    pub receipt_mime: Option<String>,
    pub receipt_sha256: Option<String>,
    pub storno_of: Option<Uuid>,
    /// Id of the Storno row reversing this booking, if any.
    pub storno_id: Option<Uuid>,
    pub locked_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Every column an expense write sets. Amounts are already split netto/USt/brutto.
#[derive(Debug, Clone)]
pub(crate) struct ExpenseInput {
    pub category_id: Uuid,
    pub status: String,
    pub receipt_date: NaiveDate,
    pub paid_on: Option<NaiveDate>,
    pub period_month: NaiveDate,
    pub supplier: Option<String>,
    pub receipt_number: Option<String>,
    pub description: Option<String>,
    pub netto_cents: i64,
    pub vat_rate: i16,
    pub vat_cents: i64,
    pub brutto_cents: i64,
    pub vehicle_id: Option<Uuid>,
    pub inquiry_id: Option<Uuid>,
    pub employee_id: Option<Uuid>,
}

#[derive(Debug, Default)]
pub(crate) struct ExpenseFilter {
    pub from_month: Option<NaiveDate>,
    pub to_month: Option<NaiveDate>,
    pub category_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub inquiry_id: Option<Uuid>,
    pub employee_id: Option<Uuid>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub(crate) struct RecurringRow {
    pub id: Uuid,
    pub category_id: Uuid,
    pub category_name: String,
    pub category_kind: String,
    pub label: String,
    pub supplier: Option<String>,
    pub netto_cents: i64,
    pub vat_rate: i16,
    pub vat_cents: i64,
    pub brutto_cents: i64,
    pub interval_months: i16,
    pub day_of_month: i16,
    pub start_month: NaiveDate,
    pub end_month: Option<NaiveDate>,
    pub vehicle_id: Option<Uuid>,
    pub vehicle_label: Option<String>,
    pub active: bool,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct RecurringInput {
    pub category_id: Uuid,
    pub label: String,
    pub supplier: Option<String>,
    pub netto_cents: i64,
    pub vat_rate: i16,
    pub vat_cents: i64,
    pub brutto_cents: i64,
    pub interval_months: i16,
    pub day_of_month: i16,
    pub start_month: NaiveDate,
    pub end_month: Option<NaiveDate>,
    pub vehicle_id: Option<Uuid>,
    pub active: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub(crate) struct LaborMonthRow {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub month: NaiveDate,
    pub paid_hours: f64,
    pub worked_hours: f64,
    pub rate_cents: i32,
    pub cost_cents: i64,
    pub unconfirmed_days: i32,
    pub breakdown: serde_json::Value,
    pub transferred_at: DateTime<Utc>,
    pub transferred_by: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LaborMonthInput {
    pub employee_id: Uuid,
    pub paid_hours: f64,
    pub worked_hours: f64,
    pub rate_cents: i32,
    pub cost_cents: i64,
    pub unconfirmed_days: i32,
    pub breakdown: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub(crate) struct AuditRow {
    pub id: Uuid,
    pub entity: String,
    pub entity_id: Option<Uuid>,
    pub action: String,
    pub actor: Option<String>,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// Booked cost summed per (period month, category kind) — the overview's cost lines.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct CostByKindRow {
    pub period_month: NaiveDate,
    pub kind: String,
    pub status: String,
    pub netto_cents: i64,
}

/// Booked wages per (month, employee). `employee_id` NULL = not linked to anyone
/// (e.g. one SV transfer to the Krankenkasse for the whole crew).
#[derive(Debug, Clone, FromRow)]
pub(crate) struct WagesRow {
    pub period_month: NaiveDate,
    pub employee_id: Option<Uuid>,
    pub netto_cents: i64,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct CostByVehicleRow {
    pub vehicle_id: Uuid,
    pub vehicle_label: String,
    pub kennzeichen: String,
    pub netto_cents: i64,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct CostByInquiryRow {
    pub inquiry_id: Uuid,
    pub netto_cents: i64,
}

/// One crew assignment-day, for every employee, from all three crew tables.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct CrewDayRow {
    pub employee_id: Uuid,
    /// `inquiry` | `calendar_item` | `appointment`
    pub kind: String,
    pub source_id: Uuid,
    /// The move this day belongs to — the inquiry itself, or an appointment's parent.
    pub inquiry_id: Option<Uuid>,
    pub day: NaiveDate,
    pub actual_hours: Option<f64>,
    pub planned_hours: Option<f64>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct BulkAdjustmentRow {
    pub employee_id: Uuid,
    pub entry_type: String,
    pub inquiry_id: Option<Uuid>,
    pub calendar_item_id: Option<Uuid>,
    pub job_date: NaiveDate,
    pub deactivated: bool,
    pub paid_clock_in: Option<NaiveTime>,
    pub paid_clock_out: Option<NaiveTime>,
    pub paid_break_minutes: Option<i32>,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct EmployeeNameRow {
    pub id: Uuid,
    pub name: String,
    pub active: bool,
}

/// A job (inquiry) whose Leistungszeitraum touches a month, for the per-job margin.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct JobRow {
    pub id: Uuid,
    pub status: String,
    pub customer_name: Option<String>,
    pub origin_city: Option<String>,
    pub destination_city: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub offer_netto_cents: Option<i64>,
}

// ── Audit log ───────────────────────────────────────────────────────────────

/// Append one entry to the accounting audit log.
///
/// **Why**: GoBD-readiness. The log can't be reconstructed after the fact, so every
/// write path calls this — inside the same transaction as the write it describes.
pub(crate) async fn log(
    tx: &mut Transaction<'_, Postgres>,
    entity: &str,
    entity_id: Option<Uuid>,
    action: &str,
    actor: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO accounting_audit_log (entity, entity_id, action, actor, before, after)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(entity)
    .bind(entity_id)
    .bind(action)
    .bind(actor)
    .bind(before)
    .bind(after)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn list_audit(
    pool: &PgPool,
    entity_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AuditRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, entity, entity_id, action, actor, before, after, created_at
         FROM accounting_audit_log
         WHERE ($1::uuid IS NULL OR entity_id = $1)
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(entity_id)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

// ── Categories ──────────────────────────────────────────────────────────────

pub(crate) async fn list_categories(pool: &PgPool) -> Result<Vec<CategoryRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, name, kind, default_vat_rate, skr03_account, sort_order, active, in_hourly_rate, recharge_positions
         FROM expense_categories ORDER BY sort_order, name",
    )
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn fetch_category(pool: &PgPool, id: Uuid) -> Result<Option<CategoryRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, name, kind, default_vat_rate, skr03_account, sort_order, active, in_hourly_rate, recharge_positions
         FROM expense_categories WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

pub(crate) async fn insert_category(
    pool: &PgPool,
    name: &str,
    kind: &str,
    default_vat_rate: i16,
    actor: &str,
) -> Result<CategoryRow, ApiError> {
    let mut tx = pool.begin().await?;
    let row: CategoryRow = sqlx::query_as(
        "INSERT INTO expense_categories (name, kind, default_vat_rate, sort_order)
         VALUES ($1, $2, $3, (SELECT COALESCE(MAX(sort_order), 0) + 10 FROM expense_categories))
         RETURNING id, name, kind, default_vat_rate, skr03_account, sort_order, active, in_hourly_rate, recharge_positions",
    )
    .bind(name)
    .bind(kind)
    .bind(default_vat_rate)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::Conflict("Kategorie existiert bereits".into())
        }
        _ => e.into(),
    })?;
    log(&mut tx, "category", Some(row.id), "create", actor, None, to_json(&row)).await?;
    tx.commit().await?;
    Ok(row)
}

/// Set the positions a category is recharged through. No positions = Eigene Kosten
/// (loaded onto the hourly rate); `in_hourly_rate` follows.
pub(crate) async fn set_category_recharge(
    pool: &PgPool,
    id: Uuid,
    positions: &[String],
    actor: &str,
) -> Result<CategoryRow, ApiError> {
    let mut tx = pool.begin().await?;
    let before: CategoryRow = sqlx::query_as(
        "SELECT id, name, kind, default_vat_rate, skr03_account, sort_order, active, in_hourly_rate, recharge_positions
         FROM expense_categories WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound("Kategorie nicht gefunden".into()))?;
    let after: CategoryRow = sqlx::query_as(
        "UPDATE expense_categories
         SET recharge_positions = $2, in_hourly_rate = (cardinality($2::text[]) = 0)
         WHERE id = $1
         RETURNING id, name, kind, default_vat_rate, skr03_account, sort_order, active, in_hourly_rate, recharge_positions",
    )
    .bind(id)
    .bind(positions)
    .fetch_one(&mut *tx)
    .await?;
    log(&mut tx, "category", Some(id), "update", actor, to_json(&before), to_json(&after)).await?;
    tx.commit().await?;
    Ok(after)
}

/// An issued invoice with everything needed to split its netto into positions.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct PositionInvoiceRow {
    pub invoice_type: String,
    pub partial_percent: Option<i32>,
    pub deposit_percent: Option<i16>,
    pub is_manual: bool,
    pub line_items_json: Option<serde_json::Value>,
    pub extra_services: serde_json::Value,
    pub offer_line_items: Option<serde_json::Value>,
    pub service_date: Option<NaiveDate>,
}

/// Issued invoices (as the register counts them: sent or paid, or past draft status;
/// not void/written off) whose Leistungszeitraum starts in the range.
pub(crate) async fn invoices_for_positions(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<PositionInvoiceRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT inv.invoice_type, inv.partial_percent, inv.deposit_percent, inv.is_manual,
                inv.line_items_json, inv.extra_services,
                off.line_items_json AS offer_line_items,
                COALESCE(inv.service_start, i.scheduled_date) AS service_date
         FROM invoices inv
         LEFT JOIN inquiries i ON i.id = inv.inquiry_id
         LEFT JOIN LATERAL (
             SELECT o.line_items_json FROM offers o
             WHERE o.inquiry_id = inv.inquiry_id
               AND o.status NOT IN ('rejected', 'cancelled', 'superseded')
             ORDER BY o.created_at DESC LIMIT 1
         ) off ON TRUE
         WHERE NOT inv.is_legacy
           AND inv.status NOT IN ('void', 'written_off')
           AND (inv.sent_at IS NOT NULL OR inv.paid_at IS NOT NULL
                OR inv.status NOT IN ('draft', 'ready', 'pending_approval'))
           AND COALESCE(inv.service_start, i.scheduled_date) BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn count_legacy_invoices(pool: &PgPool, from: NaiveDate, to: NaiveDate) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM invoices WHERE is_legacy AND service_start BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await?)
}

/// Position names Alex has actually used on KVAs and manual invoices in the last
/// year, most frequent first — the picker's suggestions next to the catalogue.
pub(crate) async fn used_position_names(pool: &PgPool) -> Result<Vec<String>, ApiError> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT name FROM (
            SELECT TRIM(li->>'description') AS name
            FROM offers o, jsonb_array_elements(o.line_items_json) li
            WHERE o.line_items_json IS NOT NULL
              AND o.created_at > NOW() - INTERVAL '12 months'
              AND COALESCE((li->>'is_labor')::bool, false) = false
            UNION ALL
            SELECT TRIM(li->>'description')
            FROM invoices inv, jsonb_array_elements(inv.line_items_json) li
            WHERE inv.is_manual AND inv.line_items_json IS NOT NULL
              AND inv.created_at > NOW() - INTERVAL '12 months'
        ) t
        WHERE name <> ''
        GROUP BY name
        ORDER BY COUNT(*) DESC, name
        LIMIT 150
        "#,
    )
    .fetch_all(pool)
    .await?)
}

// ── Expenses ────────────────────────────────────────────────────────────────

const EXPENSE_SELECT: &str = r#"
    SELECT e.id, e.category_id, c.name AS category_name, c.kind AS category_kind,
           e.status, e.receipt_date, e.paid_on, e.period_month, e.supplier,
           e.receipt_number, e.description, e.netto_cents, e.vat_rate, e.vat_cents,
           e.brutto_cents, e.vehicle_id,
           CASE WHEN v.id IS NULL THEN NULL ELSE v.label || ' (' || v.kennzeichen || ')' END
               AS vehicle_label,
           e.inquiry_id,
           CASE WHEN i.id IS NULL THEN NULL
                ELSE COALESCE(cu.first_name || ' ' || cu.last_name, cu.name, '')
                     || COALESCE(' · ' || TO_CHAR(i.scheduled_date, 'DD.MM.YYYY'), '') END
               AS inquiry_label,
           e.employee_id,
           CASE WHEN em.id IS NULL THEN NULL ELSE em.first_name || ' ' || em.last_name END
               AS employee_name,
           e.recurring_id, e.receipt_s3_key, e.receipt_filename, e.receipt_mime,
           e.receipt_sha256, e.storno_of, s.id AS storno_id, e.locked_at, e.created_by,
           e.created_at, e.updated_at
    FROM expenses e
    JOIN expense_categories c ON c.id = e.category_id
    LEFT JOIN vehicles v   ON v.id = e.vehicle_id
    LEFT JOIN inquiries i  ON i.id = e.inquiry_id
    LEFT JOIN customers cu ON cu.id = i.customer_id
    LEFT JOIN employees em ON em.id = e.employee_id
    LEFT JOIN expenses s   ON s.storno_of = e.id
"#;

pub(crate) async fn list_expenses(
    pool: &PgPool,
    f: &ExpenseFilter,
) -> Result<Vec<ExpenseRow>, ApiError> {
    let sql = format!(
        "{EXPENSE_SELECT}
         WHERE ($1::date IS NULL OR e.period_month >= $1)
           AND ($2::date IS NULL OR e.period_month <= $2)
           AND ($3::uuid IS NULL OR e.category_id = $3)
           AND ($4::uuid IS NULL OR e.vehicle_id = $4)
           AND ($5::uuid IS NULL OR e.inquiry_id = $5)
           AND ($6::uuid IS NULL OR e.employee_id = $6)
           AND ($7::text IS NULL OR e.status = $7)
         ORDER BY e.receipt_date DESC, e.created_at DESC"
    );
    Ok(sqlx::query_as(&sql)
        .bind(f.from_month)
        .bind(f.to_month)
        .bind(f.category_id)
        .bind(f.vehicle_id)
        .bind(f.inquiry_id)
        .bind(f.employee_id)
        .bind(f.status.as_deref())
        .fetch_all(pool)
        .await?)
}

pub(crate) async fn fetch_expense(pool: &PgPool, id: Uuid) -> Result<Option<ExpenseRow>, ApiError> {
    let sql = format!("{EXPENSE_SELECT} WHERE e.id = $1");
    Ok(sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?)
}

async fn fetch_expense_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<ExpenseRow>, ApiError> {
    let sql = format!("{EXPENSE_SELECT} WHERE e.id = $1");
    Ok(sqlx::query_as(&sql).bind(id).fetch_optional(&mut **tx).await?)
}

fn to_json<T: Serialize>(v: &T) -> Option<serde_json::Value> {
    serde_json::to_value(v).ok()
}

async fn fetch_expenses_tx(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<ExpenseRow>, ApiError> {
    let sql = format!("{EXPENSE_SELECT} WHERE e.id = ANY($1) ORDER BY e.period_month");
    Ok(sqlx::query_as(&sql).bind(ids).fetch_all(&mut **tx).await?)
}

/// Equal apart from the `updated_at` stamp — a resync that changed nothing is not logged.
fn same_content(a: &ExpenseRow, b: &ExpenseRow) -> bool {
    let strip = |r: &ExpenseRow| {
        let mut v = to_json(r).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("updated_at");
        }
        v
    };
    strip(a) == strip(b)
}

/// Delete expense rows and log each one with its full content.
///
/// **Why**: the cascades of a Dauerauftrag edit/delete touch several drafts in one
/// go; each still needs its own `delete` entry, written before the row is gone.
async fn delete_expenses_logged(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
    actor: &str,
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    for row in fetch_expenses_tx(tx, ids).await? {
        log(tx, "expense", Some(row.id), "delete", actor, to_json(&row), None).await?;
    }
    sqlx::query("DELETE FROM expenses WHERE id = ANY($1)").bind(ids).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn insert_expense(
    pool: &PgPool,
    input: &ExpenseInput,
    actor: &str,
) -> Result<ExpenseRow, ApiError> {
    let mut tx = pool.begin().await?;
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO expenses (category_id, status, receipt_date, paid_on, period_month,
                               supplier, receipt_number, description, netto_cents, vat_rate,
                               vat_cents, brutto_cents, vehicle_id, inquiry_id, employee_id,
                               created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
         RETURNING id",
    )
    .bind(input.category_id)
    .bind(&input.status)
    .bind(input.receipt_date)
    .bind(input.paid_on)
    .bind(input.period_month)
    .bind(&input.supplier)
    .bind(&input.receipt_number)
    .bind(&input.description)
    .bind(input.netto_cents)
    .bind(input.vat_rate)
    .bind(input.vat_cents)
    .bind(input.brutto_cents)
    .bind(input.vehicle_id)
    .bind(input.inquiry_id)
    .bind(input.employee_id)
    .bind(actor)
    .fetch_one(&mut *tx)
    .await?;
    let row = fetch_expense_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Buchung verschwunden".into()))?;
    log(&mut tx, "expense", Some(id), "create", actor, None, to_json(&row)).await?;
    tx.commit().await?;
    Ok(row)
}

/// A Storno row or a booking that has been reversed is frozen: editing either would
/// break the pair summing to zero.
fn ensure_editable(row: &ExpenseRow) -> Result<(), ApiError> {
    if row.storno_of.is_some() {
        return Err(ApiError::Conflict("Eine Stornobuchung kann nicht bearbeitet werden".into()));
    }
    if row.storno_id.is_some() {
        return Err(ApiError::Conflict("Stornierte Buchungen können nicht bearbeitet werden".into()));
    }
    Ok(())
}

pub(crate) async fn update_expense(
    pool: &PgPool,
    id: Uuid,
    input: &ExpenseInput,
    actor: &str,
) -> Result<ExpenseRow, ApiError> {
    let mut tx = pool.begin().await?;
    let before = fetch_expense_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    ensure_editable(&before)?;
    sqlx::query(
        "UPDATE expenses SET category_id = $2, status = $3, receipt_date = $4, paid_on = $5,
                period_month = $6, supplier = $7, receipt_number = $8, description = $9,
                netto_cents = $10, vat_rate = $11, vat_cents = $12, brutto_cents = $13,
                vehicle_id = $14, inquiry_id = $15, employee_id = $16, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(id)
    .bind(input.category_id)
    .bind(&input.status)
    .bind(input.receipt_date)
    .bind(input.paid_on)
    .bind(input.period_month)
    .bind(&input.supplier)
    .bind(&input.receipt_number)
    .bind(&input.description)
    .bind(input.netto_cents)
    .bind(input.vat_rate)
    .bind(input.vat_cents)
    .bind(input.brutto_cents)
    .bind(input.vehicle_id)
    .bind(input.inquiry_id)
    .bind(input.employee_id)
    .execute(&mut *tx)
    .await?;
    let after = fetch_expense_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Buchung verschwunden".into()))?;
    log(&mut tx, "expense", Some(id), "update", actor, to_json(&before), to_json(&after)).await?;
    tx.commit().await?;
    Ok(after)
}

/// Confirm a recurring draft as an actual booking.
pub(crate) async fn confirm_expense(pool: &PgPool, id: Uuid, actor: &str) -> Result<ExpenseRow, ApiError> {
    let mut tx = pool.begin().await?;
    let before = fetch_expense_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    if before.status != "draft" {
        return Err(ApiError::Conflict("Buchung ist bereits gebucht".into()));
    }
    sqlx::query("UPDATE expenses SET status = 'booked', updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let after = fetch_expense_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Buchung verschwunden".into()))?;
    log(&mut tx, "expense", Some(id), "confirm", actor, to_json(&before), to_json(&after)).await?;
    tx.commit().await?;
    Ok(after)
}

/// Reverse a booking with a negated twin row. Returns the Storno row.
pub(crate) async fn storno_expense(pool: &PgPool, id: Uuid, actor: &str) -> Result<ExpenseRow, ApiError> {
    let mut tx = pool.begin().await?;
    let orig = fetch_expense_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    if orig.storno_of.is_some() {
        return Err(ApiError::Conflict("Eine Stornobuchung kann nicht storniert werden".into()));
    }
    if orig.storno_id.is_some() {
        return Err(ApiError::Conflict("Buchung ist bereits storniert".into()));
    }
    if orig.status == "draft" {
        return Err(ApiError::Conflict(
            "Ein unbestätigter Dauerauftrag wird gelöscht, nicht storniert".into(),
        ));
    }
    let description = format!(
        "Storno: {}",
        orig.description.clone().or(orig.supplier.clone()).unwrap_or_else(|| orig.category_name.clone())
    );
    let (sid,): (Uuid,) = sqlx::query_as(
        "INSERT INTO expenses (category_id, status, receipt_date, paid_on, period_month,
                               supplier, receipt_number, description, netto_cents, vat_rate,
                               vat_cents, brutto_cents, vehicle_id, inquiry_id, employee_id,
                               storno_of, created_by)
         SELECT category_id, 'booked', CURRENT_DATE, NULL, period_month,
                supplier, receipt_number, $2, -netto_cents, vat_rate,
                -vat_cents, -brutto_cents, vehicle_id, inquiry_id, employee_id,
                id, $3
         FROM expenses WHERE id = $1
         RETURNING id",
    )
    .bind(id)
    .bind(description)
    .bind(actor)
    .fetch_one(&mut *tx)
    .await?;
    let storno = fetch_expense_tx(&mut tx, sid).await?.ok_or_else(|| ApiError::Internal("Storno verschwunden".into()))?;
    log(&mut tx, "expense", Some(id), "storno", actor, to_json(&orig), to_json(&storno)).await?;
    tx.commit().await?;
    Ok(storno)
}

/// Hard-delete a booking. Deleting an original also removes its Storno row — left
/// behind, the negative twin would silently lower the costs. Returns the receipt
/// keys that are no longer referenced so the caller can drop them from S3.
pub(crate) async fn delete_expense(pool: &PgPool, id: Uuid, actor: &str) -> Result<Vec<String>, ApiError> {
    let mut tx = pool.begin().await?;
    let row = fetch_expense_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    // Deleting only the reversal would silently bring the original cost back.
    if row.storno_of.is_some() {
        return Err(ApiError::Conflict(
            "Eine Stornobuchung kann nicht einzeln gelöscht werden. Lösche die Originalbuchung, die Storno wird mitgelöscht.".into(),
        ));
    }
    let mut keys = Vec::new();
    if let Some(sid) = row.storno_id {
        if let Some(s) = fetch_expense_tx(&mut tx, sid).await? {
            log(&mut tx, "expense", Some(sid), "delete", actor, to_json(&s), None).await?;
            keys.extend(s.receipt_s3_key.clone());
        }
        sqlx::query("DELETE FROM expenses WHERE id = $1").bind(sid).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM expenses WHERE id = $1").bind(id).execute(&mut *tx).await?;
    // A Dauerauftrag month deleted by hand must not be regenerated on the next read.
    if let Some(rid) = row.recurring_id {
        sqlx::query(
            "INSERT INTO recurring_expense_skips (recurring_id, period_month) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(rid)
        .bind(row.period_month)
        .execute(&mut *tx)
        .await?;
    }
    log(&mut tx, "expense", Some(id), "delete", actor, to_json(&row), None).await?;
    keys.extend(row.receipt_s3_key.clone());
    tx.commit().await?;
    Ok(keys)
}

pub(crate) async fn set_receipt(
    pool: &PgPool,
    id: Uuid,
    key: &str,
    filename: &str,
    mime: &str,
    sha256: &str,
    actor: &str,
) -> Result<ExpenseRow, ApiError> {
    let mut tx = pool.begin().await?;
    let before = fetch_expense_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    sqlx::query(
        "UPDATE expenses SET receipt_s3_key = $2, receipt_filename = $3, receipt_mime = $4,
                receipt_sha256 = $5, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(id)
    .bind(key)
    .bind(filename)
    .bind(mime)
    .bind(sha256)
    .execute(&mut *tx)
    .await?;
    let after = fetch_expense_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Buchung verschwunden".into()))?;
    log(&mut tx, "expense", Some(id), "update", actor, to_json(&before), to_json(&after)).await?;
    tx.commit().await?;
    Ok(after)
}

// ── Recurring templates ─────────────────────────────────────────────────────

const RECURRING_SELECT: &str = r#"
    SELECT r.id, r.category_id, c.name AS category_name, c.kind AS category_kind,
           r.label, r.supplier,
           r.netto_cents, r.vat_rate, r.vat_cents, r.brutto_cents, r.interval_months,
           r.day_of_month, r.start_month, r.end_month, r.vehicle_id,
           CASE WHEN v.id IS NULL THEN NULL ELSE v.label || ' (' || v.kennzeichen || ')' END
               AS vehicle_label,
           r.active, r.notes, r.created_at, r.updated_at
    FROM recurring_expenses r
    JOIN expense_categories c ON c.id = r.category_id
    LEFT JOIN vehicles v ON v.id = r.vehicle_id
"#;

pub(crate) async fn list_recurring(pool: &PgPool) -> Result<Vec<RecurringRow>, ApiError> {
    let sql = format!("{RECURRING_SELECT} ORDER BY r.active DESC, c.sort_order, r.label");
    Ok(sqlx::query_as(&sql).fetch_all(pool).await?)
}

async fn fetch_recurring_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<RecurringRow>, ApiError> {
    let sql = format!("{RECURRING_SELECT} WHERE r.id = $1");
    Ok(sqlx::query_as(&sql).bind(id).fetch_optional(&mut **tx).await?)
}

pub(crate) async fn insert_recurring(
    pool: &PgPool,
    input: &RecurringInput,
    actor: &str,
) -> Result<RecurringRow, ApiError> {
    let mut tx = pool.begin().await?;
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO recurring_expenses (category_id, label, supplier, netto_cents, vat_rate,
                vat_cents, brutto_cents, interval_months, day_of_month, start_month, end_month,
                vehicle_id, active, notes)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
         RETURNING id",
    )
    .bind(input.category_id)
    .bind(&input.label)
    .bind(&input.supplier)
    .bind(input.netto_cents)
    .bind(input.vat_rate)
    .bind(input.vat_cents)
    .bind(input.brutto_cents)
    .bind(input.interval_months)
    .bind(input.day_of_month)
    .bind(input.start_month)
    .bind(input.end_month)
    .bind(input.vehicle_id)
    .bind(input.active)
    .bind(&input.notes)
    .fetch_one(&mut *tx)
    .await?;
    let row = fetch_recurring_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Dauerauftrag verschwunden".into()))?;
    log(&mut tx, "recurring_expense", Some(id), "create", actor, None, to_json(&row)).await?;
    tx.commit().await?;
    Ok(row)
}

pub(crate) async fn update_recurring(
    pool: &PgPool,
    id: Uuid,
    input: &RecurringInput,
    actor: &str,
) -> Result<RecurringRow, ApiError> {
    let mut tx = pool.begin().await?;
    let before = fetch_recurring_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Dauerauftrag nicht gefunden".into()))?;
    sqlx::query(
        "UPDATE recurring_expenses SET category_id = $2, label = $3, supplier = $4,
                netto_cents = $5, vat_rate = $6, vat_cents = $7, brutto_cents = $8,
                interval_months = $9, day_of_month = $10, start_month = $11, end_month = $12,
                vehicle_id = $13, active = $14, notes = $15, updated_at = NOW()
         WHERE id = $1",
    )
    .bind(id)
    .bind(input.category_id)
    .bind(&input.label)
    .bind(&input.supplier)
    .bind(input.netto_cents)
    .bind(input.vat_rate)
    .bind(input.vat_cents)
    .bind(input.brutto_cents)
    .bind(input.interval_months)
    .bind(input.day_of_month)
    .bind(input.start_month)
    .bind(input.end_month)
    .bind(input.vehicle_id)
    .bind(input.active)
    .bind(&input.notes)
    .execute(&mut *tx)
    .await?;
    // Drafts for months the template no longer charges (start moved later, end moved
    // earlier, interval changed) go; booked entries are history and stay. Months that
    // became due are filled by the next `generate_recurring_drafts`.
    let doomed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT e.id FROM expenses e JOIN recurring_expenses r ON r.id = e.recurring_id
         WHERE r.id = $1 AND e.status = 'draft'
           AND (e.period_month < r.start_month
                OR (r.end_month IS NOT NULL AND e.period_month > r.end_month)
                OR ((EXTRACT(YEAR FROM e.period_month) * 12 + EXTRACT(MONTH FROM e.period_month))
                    - (EXTRACT(YEAR FROM r.start_month) * 12 + EXTRACT(MONTH FROM r.start_month)))::int
                   % r.interval_months <> 0)",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    delete_expenses_logged(&mut tx, &doomed, actor).await?;
    // Unconfirmed drafts follow the template; booked entries are history and stay.
    let draft_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM expenses WHERE recurring_id = $1 AND status = 'draft'")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    let drafts_before = fetch_expenses_tx(&mut tx, &draft_ids).await?;
    sqlx::query(
        "UPDATE expenses e SET category_id = r.category_id, supplier = r.supplier,
                description = r.label, netto_cents = r.netto_cents, vat_rate = r.vat_rate,
                vat_cents = r.vat_cents, brutto_cents = r.brutto_cents, vehicle_id = r.vehicle_id,
                -- same date rule as generate_recurring_drafts
                receipt_date = (e.period_month + (LEAST(r.day_of_month, 28) - 1) * INTERVAL '1 day')::date,
                updated_at = NOW()
         FROM recurring_expenses r
         WHERE r.id = $1 AND e.recurring_id = r.id AND e.status = 'draft'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    for (old, new) in drafts_before.iter().zip(fetch_expenses_tx(&mut tx, &draft_ids).await?) {
        if !same_content(old, &new) {
            log(&mut tx, "expense", Some(new.id), "update", actor, to_json(old), to_json(&new)).await?;
        }
    }
    let after = fetch_recurring_tx(&mut tx, id).await?.ok_or_else(|| ApiError::Internal("Dauerauftrag verschwunden".into()))?;
    log(&mut tx, "recurring_expense", Some(id), "update", actor, to_json(&before), to_json(&after)).await?;
    tx.commit().await?;
    Ok(after)
}

/// Delete a template together with its unconfirmed drafts. Booked entries keep their
/// amounts and lose only the link (`ON DELETE SET NULL`).
pub(crate) async fn delete_recurring(pool: &PgPool, id: Uuid, actor: &str) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    let before = fetch_recurring_tx(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Dauerauftrag nicht gefunden".into()))?;
    let drafts: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM expenses WHERE recurring_id = $1 AND status = 'draft'")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    delete_expenses_logged(&mut tx, &drafts, actor).await?;
    sqlx::query("DELETE FROM recurring_expenses WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    log(&mut tx, "recurring_expense", Some(id), "delete", actor, to_json(&before), None).await?;
    tx.commit().await?;
    Ok(())
}

/// Create the draft entry of every active template for every due month up to
/// `up_to_month`. Idempotent: the partial unique index on
/// `(recurring_id, period_month)` makes a second run a no-op. A month whose entry
/// Alex deleted by hand is listed in `recurring_expense_skips` and stays deleted;
/// every other missing month is (back)filled — including after the template's start
/// month was moved earlier.
///
/// **Why on read, not a cron**: the Gewinn tab is the only reader, and generating on
/// open keeps the result deterministic and testable.
pub(crate) async fn generate_recurring_drafts(
    pool: &PgPool,
    up_to_month: NaiveDate,
) -> Result<u64, ApiError> {
    let mut tx = pool.begin().await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO expenses (category_id, status, receipt_date, period_month, supplier,
                              description, netto_cents, vat_rate, vat_cents, brutto_cents,
                              vehicle_id, recurring_id, created_by)
        SELECT r.category_id, 'draft',
               (m.month + (LEAST(r.day_of_month, 28) - 1) * INTERVAL '1 day')::date,
               m.month::date, r.supplier, r.label, r.netto_cents, r.vat_rate, r.vat_cents,
               r.brutto_cents, r.vehicle_id, r.id, 'dauerauftrag'
        FROM recurring_expenses r
        CROSS JOIN LATERAL generate_series(
            r.start_month,
            LEAST(COALESCE(r.end_month, $1::date), $1::date),
            (r.interval_months || ' months')::interval
        ) AS m(month)
        WHERE r.active
          AND NOT EXISTS (
              SELECT 1 FROM recurring_expense_skips k
              WHERE k.recurring_id = r.id AND k.period_month = m.month::date)
        ON CONFLICT (recurring_id, period_month) WHERE recurring_id IS NOT NULL DO NOTHING
        RETURNING id
        "#,
    )
    .bind(up_to_month)
    .fetch_all(&mut *tx)
    .await?;
    for row in fetch_expenses_tx(&mut tx, &ids).await? {
        log(&mut tx, "expense", Some(row.id), "create", "dauerauftrag", None, to_json(&row)).await?;
    }
    tx.commit().await?;
    Ok(ids.len() as u64)
}

// ── Aggregations ────────────────────────────────────────────────────────────

/// Unconfirmed Dauerauftrag drafts, for the "zu bestätigen" badge.
pub(crate) async fn count_drafts(pool: &PgPool) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM expenses WHERE status = 'draft'")
        .fetch_one(pool)
        .await?)
}

/// Cost per (month, category kind, status), Storno rows netting out naturally.
pub(crate) async fn cost_by_kind(
    pool: &PgPool,
    from_month: NaiveDate,
    to_month: NaiveDate,
) -> Result<Vec<CostByKindRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT e.period_month, c.kind, e.status, SUM(e.netto_cents)::bigint AS netto_cents
         FROM expenses e JOIN expense_categories c ON c.id = e.category_id
         WHERE e.period_month BETWEEN $1 AND $2
         GROUP BY e.period_month, c.kind, e.status",
    )
    .bind(from_month)
    .bind(to_month)
    .fetch_all(pool)
    .await?)
}

/// Booked wages per (month, employee) — drafts are not money that was paid.
pub(crate) async fn wages_by_month(
    pool: &PgPool,
    from_month: NaiveDate,
    to_month: NaiveDate,
) -> Result<Vec<WagesRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT e.period_month, e.employee_id, SUM(e.netto_cents)::bigint AS netto_cents
         FROM expenses e JOIN expense_categories c ON c.id = e.category_id
         WHERE c.kind = 'wages' AND e.status = 'booked'
           AND e.period_month BETWEEN $1 AND $2
         GROUP BY e.period_month, e.employee_id",
    )
    .bind(from_month)
    .bind(to_month)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn cost_by_vehicle(
    pool: &PgPool,
    from_month: NaiveDate,
    to_month: NaiveDate,
) -> Result<Vec<CostByVehicleRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT v.id AS vehicle_id, v.label AS vehicle_label, v.kennzeichen,
                SUM(e.netto_cents)::bigint AS netto_cents
         FROM expenses e JOIN vehicles v ON v.id = e.vehicle_id
         WHERE e.period_month BETWEEN $1 AND $2
         GROUP BY v.id, v.label, v.kennzeichen",
    )
    .bind(from_month)
    .bind(to_month)
    .fetch_all(pool)
    .await?)
}

/// Costs booked directly against jobs (Halteverbot, Fremdleistung, Material, …).
pub(crate) async fn cost_by_inquiry(
    pool: &PgPool,
    inquiry_ids: &[Uuid],
) -> Result<Vec<CostByInquiryRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT e.inquiry_id AS inquiry_id, SUM(e.netto_cents)::bigint AS netto_cents
         FROM expenses e
         WHERE e.inquiry_id = ANY($1)
         GROUP BY e.inquiry_id",
    )
    .bind(inquiry_ids)
    .fetch_all(pool)
    .await?)
}

// ── Labor: hours snapshots + live crew hours ────────────────────────────────

pub(crate) async fn list_labor_months(
    pool: &PgPool,
    from_month: NaiveDate,
    to_month: NaiveDate,
) -> Result<Vec<LaborMonthRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, employee_id, month, paid_hours::float8 AS paid_hours,
                worked_hours::float8 AS worked_hours, rate_cents, cost_cents,
                unconfirmed_days, breakdown, transferred_at, transferred_by
         FROM labor_months WHERE month BETWEEN $1 AND $2
         ORDER BY month, employee_id",
    )
    .bind(from_month)
    .bind(to_month)
    .fetch_all(pool)
    .await?)
}

/// Replace a month's hours snapshot with `rows`. Employees that had a snapshot but
/// are missing from `rows` (no hours any more) are removed. Every change is logged.
pub(crate) async fn replace_labor_month(
    pool: &PgPool,
    month: NaiveDate,
    rows: &[LaborMonthInput],
    actor: &str,
) -> Result<Vec<LaborMonthRow>, ApiError> {
    let mut tx = pool.begin().await?;
    let mut written = Vec::with_capacity(rows.len());
    let existing: Vec<LaborMonthRow> = sqlx::query_as(
        "SELECT id, employee_id, month, paid_hours::float8 AS paid_hours,
                worked_hours::float8 AS worked_hours, rate_cents, cost_cents,
                unconfirmed_days, breakdown, transferred_at, transferred_by
         FROM labor_months WHERE month = $1 FOR UPDATE",
    )
    .bind(month)
    .fetch_all(&mut *tx)
    .await?;

    for old in existing.iter().filter(|o| !rows.iter().any(|r| r.employee_id == o.employee_id)) {
        sqlx::query("DELETE FROM labor_months WHERE id = $1").bind(old.id).execute(&mut *tx).await?;
        log(&mut tx, "labor_month", Some(old.id), "delete", actor, to_json(old), None).await?;
    }

    for r in rows {
        let new: LaborMonthRow = sqlx::query_as(
            "INSERT INTO labor_months (employee_id, month, paid_hours, worked_hours, rate_cents,
                                       cost_cents, unconfirmed_days, breakdown, transferred_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (employee_id, month) DO UPDATE SET
                paid_hours = EXCLUDED.paid_hours, worked_hours = EXCLUDED.worked_hours,
                rate_cents = EXCLUDED.rate_cents, cost_cents = EXCLUDED.cost_cents,
                unconfirmed_days = EXCLUDED.unconfirmed_days, breakdown = EXCLUDED.breakdown,
                transferred_at = NOW(), transferred_by = EXCLUDED.transferred_by
             RETURNING id, employee_id, month, paid_hours::float8 AS paid_hours,
                       worked_hours::float8 AS worked_hours, rate_cents, cost_cents,
                       unconfirmed_days, breakdown, transferred_at, transferred_by",
        )
        .bind(r.employee_id)
        .bind(month)
        .bind(r.paid_hours)
        .bind(r.worked_hours)
        .bind(r.rate_cents)
        .bind(r.cost_cents)
        .bind(r.unconfirmed_days)
        .bind(&r.breakdown)
        .bind(actor)
        .fetch_one(&mut *tx)
        .await?;
        let before = existing.iter().find(|o| o.employee_id == r.employee_id);
        log(
            &mut tx,
            "labor_month",
            Some(new.id),
            "transfer",
            actor,
            before.and_then(to_json),
            to_json(&new),
        )
        .await?;
        written.push(new);
    }
    tx.commit().await?;
    Ok(written)
}

/// Every crew assignment-day in a range, for all employees, from the three crew
/// tables. Filters mirror `employee_repo::fetch_admin_hours`,
/// `fetch_admin_calendar_item_hours` and `fetch_admin_appointment_hours` so the
/// Gewinn tab and the hours tab count the same days — change them together.
pub(crate) async fn crew_days(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<CrewDayRow>, ApiError> {
    Ok(sqlx::query_as(
        r#"
        SELECT ie.employee_id, 'inquiry' AS kind, ie.inquiry_id AS source_id,
               ie.inquiry_id AS inquiry_id, ie.job_date AS day,
               COALESCE(ie.actual_hours::float8,
                    CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                         THEN (aust_shift_hours(ie.clock_in, ie.clock_out)
                               - COALESCE(ie.break_minutes, 0) / 60.0)::float8
                         ELSE NULL END) AS actual_hours,
               ie.planned_hours::float8 AS planned_hours,
               (ie.clock_in IS NOT NULL AND ie.clock_out IS NOT NULL) AS confirmed
        FROM inquiry_employees ie
        JOIN inquiries i ON i.id = ie.inquiry_id
        WHERE ie.job_date BETWEEN $1 AND $2
          AND i.status NOT IN ('cancelled', 'rejected', 'expired')
        UNION ALL
        SELECT cie.employee_id, 'calendar_item', cie.calendar_item_id, NULL::uuid, cie.job_date,
               COALESCE(cie.actual_hours::float8,
                    CASE WHEN cie.clock_out IS NOT NULL AND cie.clock_in IS NOT NULL
                         THEN (aust_shift_hours(cie.clock_in, cie.clock_out)
                               - COALESCE(cie.break_minutes, 0) / 60.0)::float8
                         ELSE NULL END),
               cie.planned_hours::float8,
               (cie.clock_in IS NOT NULL AND cie.clock_out IS NOT NULL)
        FROM calendar_item_employees cie
        JOIN calendar_items ci ON ci.id = cie.calendar_item_id
        WHERE cie.job_date BETWEEN $1 AND $2
          AND ci.status NOT IN ('cancelled')
        UNION ALL
        SELECT iae.employee_id, 'appointment', iae.appointment_id, a.inquiry_id, a.scheduled_date,
               COALESCE(iae.actual_hours::float8,
                    CASE WHEN iae.clock_out IS NOT NULL AND iae.clock_in IS NOT NULL
                         THEN (aust_shift_hours(iae.clock_in, iae.clock_out)
                               - COALESCE(iae.break_minutes, 0) / 60.0)::float8
                         ELSE NULL END),
               iae.planned_hours::float8,
               (iae.clock_in IS NOT NULL AND iae.clock_out IS NOT NULL)
        FROM inquiry_appointment_employees iae
        JOIN inquiry_appointments a ON a.id = iae.appointment_id
        WHERE a.scheduled_date BETWEEN $1 AND $2
          AND a.status <> 'cancelled'
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}

/// Payroll adjustments (Bezahlt-Zeiten, deactivated days) for all employees in a range.
pub(crate) async fn adjustments(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<BulkAdjustmentRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT employee_id, entry_type, inquiry_id, calendar_item_id, job_date, deactivated,
                paid_clock_in, paid_clock_out, paid_break_minutes
         FROM hours_adjustments
         WHERE job_date BETWEEN $1 AND $2",
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn employee_names(pool: &PgPool) -> Result<Vec<EmployeeNameRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT id, first_name || ' ' || last_name AS name, active
         FROM employees ORDER BY last_name, first_name",
    )
    .fetch_all(pool)
    .await?)
}

// ── Jobs ────────────────────────────────────────────────────────────────────

const JOB_SELECT: &str = r#"
    SELECT i.id, i.status,
           COALESCE(cu.first_name || ' ' || cu.last_name, cu.name) AS customer_name,
           oa.city AS origin_city,
           da.city AS destination_city, i.scheduled_date, i.end_date,
           o.price_cents AS offer_netto_cents
    FROM inquiries i
    LEFT JOIN customers cu ON cu.id = i.customer_id
    LEFT JOIN addresses oa ON oa.id = i.origin_address_id
    LEFT JOIN addresses da ON da.id = i.destination_address_id
    LEFT JOIN LATERAL (
        SELECT price_cents FROM offers
        WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled', 'superseded')
        ORDER BY created_at DESC LIMIT 1
    ) o ON TRUE
"#;

/// Jobs that start in the given month and are (or were) actually happening.
pub(crate) async fn jobs_in_month(
    pool: &PgPool,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<JobRow>, ApiError> {
    let sql = format!(
        "{JOB_SELECT}
         WHERE i.scheduled_date BETWEEN $1 AND $2
           AND i.status IN ('accepted', 'scheduled', 'completed', 'invoiced', 'paid')
         ORDER BY i.scheduled_date, cu.name"
    );
    Ok(sqlx::query_as(&sql).bind(from).bind(to).fetch_all(pool).await?)
}

pub(crate) async fn fetch_job(pool: &PgPool, id: Uuid) -> Result<Option<JobRow>, ApiError> {
    let sql = format!("{JOB_SELECT} WHERE i.id = $1");
    Ok(sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?)
}

/// Unscheduled inquiries have no `scheduled_date`; they still get a KVA preview.
pub(crate) async fn fetch_offer_for_preview(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<(Option<i64>, Option<i32>, Option<f64>)>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT o.price_cents, o.persons, o.hours_estimated
         FROM inquiries i
         LEFT JOIN LATERAL (
             SELECT price_cents, persons, hours_estimated FROM offers
             WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled', 'superseded')
             ORDER BY created_at DESC LIMIT 1
         ) o ON TRUE
         WHERE i.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Crew days of specific inquiries, whatever their date.
pub(crate) async fn crew_days_for_inquiries(
    pool: &PgPool,
    inquiry_ids: &[Uuid],
) -> Result<Vec<CrewDayRow>, ApiError> {
    Ok(sqlx::query_as(
        r#"
        SELECT ie.employee_id, 'inquiry' AS kind, ie.inquiry_id AS source_id,
               ie.inquiry_id AS inquiry_id, ie.job_date AS day,
               COALESCE(ie.actual_hours::float8,
                    CASE WHEN ie.clock_out IS NOT NULL AND ie.clock_in IS NOT NULL
                         THEN (aust_shift_hours(ie.clock_in, ie.clock_out)
                               - COALESCE(ie.break_minutes, 0) / 60.0)::float8
                         ELSE NULL END) AS actual_hours,
               ie.planned_hours::float8 AS planned_hours,
               (ie.clock_in IS NOT NULL AND ie.clock_out IS NOT NULL) AS confirmed
        FROM inquiry_employees ie
        WHERE ie.inquiry_id = ANY($1)
        UNION ALL
        SELECT iae.employee_id, 'appointment', iae.appointment_id, a.inquiry_id, a.scheduled_date,
               COALESCE(iae.actual_hours::float8,
                    CASE WHEN iae.clock_out IS NOT NULL AND iae.clock_in IS NOT NULL
                         THEN (aust_shift_hours(iae.clock_in, iae.clock_out)
                               - COALESCE(iae.break_minutes, 0) / 60.0)::float8
                         ELSE NULL END),
               iae.planned_hours::float8,
               (iae.clock_in IS NOT NULL AND iae.clock_out IS NOT NULL)
        FROM inquiry_appointment_employees iae
        JOIN inquiry_appointments a ON a.id = iae.appointment_id
        WHERE a.inquiry_id = ANY($1) AND a.status <> 'cancelled'
        "#,
    )
    .bind(inquiry_ids)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn adjustments_for_inquiries(
    pool: &PgPool,
    inquiry_ids: &[Uuid],
) -> Result<Vec<BulkAdjustmentRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT employee_id, entry_type, inquiry_id, calendar_item_id, job_date, deactivated,
                paid_clock_in, paid_clock_out, paid_break_minutes
         FROM hours_adjustments
         WHERE inquiry_id = ANY($1)",
    )
    .bind(inquiry_ids)
    .fetch_all(pool)
    .await?)
}

/// Cost per (month, category) incl. unconfirmed drafts — the Stundensatz input.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct CostByCategoryRow {
    pub period_month: NaiveDate,
    pub category_name: String,
    pub kind: String,
    pub in_hourly_rate: bool,
    pub netto_cents: i64,
}

pub(crate) async fn cost_by_category(
    pool: &PgPool,
    from_month: NaiveDate,
    to_month: NaiveDate,
) -> Result<Vec<CostByCategoryRow>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT e.period_month, c.name AS category_name, c.kind,
                c.in_hourly_rate, SUM(e.netto_cents)::bigint AS netto_cents
         FROM expenses e JOIN expense_categories c ON c.id = e.category_id
         WHERE e.period_month BETWEEN $1 AND $2
         GROUP BY e.period_month, c.id, c.name, c.kind, c.in_hourly_rate",
    )
    .bind(from_month)
    .bind(to_month)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn count_active_employees(pool: &PgPool) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM employees WHERE active")
        .fetch_one(pool)
        .await?)
}

// ── Settings ────────────────────────────────────────────────────────────────

const KEY_HOURLY_CALC: &str = "hourly_rate_calc";

/// Assumptions behind the Stundensatz-Kalkulation, all editable in the Gewinn tab.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct HourlyCalcSettings {
    /// Profit Alex wants on top of break-even, per month (netto cents).
    pub target_profit_cents: i64,
    /// How many complete months back the calculation looks (1–24).
    pub window_months: i32,
    /// Expected sold crew hours per month; `None` = the measured average.
    pub planned_hours_per_month: Option<f64>,
    /// Crew size for the full-capacity comparison; `None` = active employees.
    pub capacity_crew: Option<i32>,
    pub capacity_hours_per_day: f64,
    pub capacity_days_per_month: f64,
}

impl Default for HourlyCalcSettings {
    fn default() -> Self {
        Self {
            target_profit_cents: 0,
            window_months: 12,
            planned_hours_per_month: None,
            capacity_crew: None,
            capacity_hours_per_day: 8.0,
            capacity_days_per_month: 21.0,
        }
    }
}

pub(crate) async fn get_hourly_calc(pool: &PgPool) -> Result<HourlyCalcSettings, ApiError> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(KEY_HOURLY_CALC)
            .fetch_optional(pool)
            .await?;
    Ok(row
        .and_then(|(v,)| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

pub(crate) async fn set_hourly_calc(
    pool: &PgPool,
    value: &HourlyCalcSettings,
    actor: &str,
) -> Result<(), ApiError> {
    let before = get_hourly_calc(pool).await?;
    if &before == value {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, NOW())
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(KEY_HOURLY_CALC)
    .bind(serde_json::to_value(value).unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    log(&mut tx, "setting", None, "update", actor, to_json(&before), to_json(value)).await?;
    tx.commit().await?;
    Ok(())
}

const KEY_DEFAULT_RATE: &str = "labor_default_rate_cents";

/// €18.50 all-in (Mindestlohn + Arbeitgeberanteil), Alex's figure 2026-10.
pub(crate) const DEFAULT_RATE_CENTS: i64 = 1850;

pub(crate) async fn get_default_rate(pool: &PgPool) -> Result<i64, ApiError> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(KEY_DEFAULT_RATE)
            .fetch_optional(pool)
            .await?;
    Ok(row
        .and_then(|(v,)| v.as_i64())
        .filter(|c| *c > 0)
        .unwrap_or(DEFAULT_RATE_CENTS))
}

pub(crate) async fn set_default_rate(pool: &PgPool, cents: i64, actor: &str) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    let before: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(KEY_DEFAULT_RATE)
            .fetch_optional(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, NOW())
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(KEY_DEFAULT_RATE)
    .bind(serde_json::json!(cents))
    .execute(&mut *tx)
    .await?;
    log(
        &mut tx,
        "setting",
        None,
        "update",
        actor,
        before.map(|(v,)| serde_json::json!({ KEY_DEFAULT_RATE: v })),
        Some(serde_json::json!({ KEY_DEFAULT_RATE: cents })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
