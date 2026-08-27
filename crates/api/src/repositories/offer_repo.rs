//! Offer repository — centralised queries for the `offers` table.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgExecutor, PgPool};
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
        "SELECT id FROM offers WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled', 'superseded') ORDER BY created_at DESC LIMIT 1",
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
/// **Caller**: `get_inquiry_pdf`, `admin_emails::fetch_offer_pdf_filename`
/// **Why**: Downloads the latest active offer's PDF from S3. Excludes `'superseded'`
/// explicitly (not just via `ORDER BY created_at DESC LIMIT 1`) so correctness doesn't
/// depend on a superseded row never having a later timestamp than its replacement.
pub(crate) async fn fetch_active_pdf_key(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(Uuid, Option<String>)>, ApiError> {
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, pdf_storage_key FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled', 'superseded')
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
        SELECT result_data
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
}

/// Update an existing offer and return the full row.
///
/// **Caller**: `offers::build_offer_with_overrides` (regenerate path)
/// **Why**: Regenerating an offer updates the price, PDF key, and pricing parameters.
///
/// `offer_number` is normally `None` — the KVA keeps the number it was issued under.
/// It is only `Some` when the regenerate path adopts an offer row that appeared while
/// this request was rendering its PDF: that PDF already carries the new number, so the
/// row has to follow it or the document and the KVA-Buch would disagree.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_returning(
    executor: impl PgExecutor<'_>,
    offer_id: Uuid,
    price_cents: i64,
    pdf_storage_key: Option<&str>,
    status: &str,
    persons: i32,
    hours_estimated: f64,
    rate_per_hour_cents: i64,
    line_items_json: &Option<serde_json::Value>,
    fahrt_override_cents: Option<i32>,
    offer_number: Option<&str>,
) -> Result<OfferFullRow, sqlx::Error> {
    sqlx::query_as(
        r#"
        UPDATE offers
        SET price_cents = $1, pdf_storage_key = $2, status = $3,
            persons = $4, hours_estimated = $5, rate_per_hour_cents = $6,
            line_items_json = $7,
            fahrt_override_cents = $8,
            offer_number = COALESCE($9, offer_number)
        WHERE id = $10
        RETURNING id, inquiry_id, price_cents, currency, valid_until, pdf_storage_key, status,
                  created_at, sent_at, offer_number, persons, hours_estimated,
                  rate_per_hour_cents, line_items_json
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
    .bind(offer_number)
    .bind(offer_id)
    .fetch_one(executor)
    .await
}

/// Insert a new offer and return the full row.
///
/// **Caller**: `offers::build_offer_with_overrides` (new offer path)
/// **Why**: Creates the offer record with pricing, PDF key, and all line item data.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_returning(
    executor: impl PgExecutor<'_>,
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
                  offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json
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
    .fetch_one(executor)
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
               status, estimated_volume_m3, distance_km, scheduled_date, notes, services,
               source, custom_fields, offer_sent_at, accepted_at, created_at, updated_at,
               billing_address_id AS inquiry_billing_address_id
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
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled', 'superseded')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Fetch one specific offer's PDF storage key.
///
/// **Caller**: `admin::kva_buch_pdf`
/// **Why**: The KVA-Buch lists every offer including rejected and expired ones, so it
/// cannot go through `fetch_active_pdf_key`, which only resolves the active offer.
/// Outer `None` = no such offer; inner `None` = offer exists but has no rendered file.
pub(crate) async fn fetch_pdf_key_by_id(
    pool: &PgPool,
    offer_id: Uuid,
) -> Result<Option<Option<String>>, sqlx::Error> {
    sqlx::query_scalar("SELECT pdf_storage_key FROM offers WHERE id = $1")
        .bind(offer_id)
        .fetch_optional(pool)
        .await
}

/// Flat projection for the KVA-Buch — one row per Kostenvoranschlag.
#[derive(Debug, FromRow)]
pub(crate) struct KvaBuchRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub offer_number: Option<String>,
    pub price_cents: i64,
    pub status: String,
    pub pdf_storage_key: Option<String>,
    pub valid_until: Option<NaiveDate>,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub customer_name: Option<String>,
    pub scheduled_date: Option<NaiveDate>,
    /// Set once the inquiry has an invoice — the KVA became a real job.
    pub invoice_number: Option<String>,
    /// The inquiry's own status — the ONLY trustworthy win/loss signal.
    ///
    /// `offers.status` is not maintained in practice (126 of 133 production rows
    /// sit at `draft`), so every derived statistic in the KVA-Buch reads this
    /// instead. `None` only when the inquiry row is missing entirely.
    pub inquiry_status: Option<String>,
    /// Follow-up nag state — see `migrations/20260823140000_kva_followup.sql`.
    pub followup_last_pinged_on: Option<NaiveDate>,
    pub followup_muted: bool,
}

/// List every Kostenvoranschlag for the KVA-Buch.
///
/// **Caller**: `admin::kva_buch`
/// **Why**: Alex wants the same yearly register he has for invoices, but for KVAs
/// (feedback report fa436f07). Deliberately mirrors
/// `invoice_repo::list_for_rechnungsausgangsbuch`: inquiry and customer are LEFT
/// joins so a KVA never drops out of the register, and `superseded` drafts are
/// excluded because a replaced draft was never a document Alex handed out.
pub(crate) async fn list_for_kva_buch(pool: &PgPool) -> Result<Vec<KvaBuchRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            o.id,
            o.inquiry_id,
            o.offer_number,
            o.price_cents,
            o.status,
            o.pdf_storage_key,
            o.valid_until,
            o.sent_at,
            o.created_at,
            o.followup_last_pinged_on,
            o.followup_muted,
            c.name AS customer_name,
            i.scheduled_date,
            i.status AS inquiry_status,
            (SELECT inv.invoice_number FROM invoices inv
              WHERE inv.inquiry_id = o.inquiry_id
              ORDER BY inv.created_at ASC LIMIT 1) AS invoice_number
        FROM offers o
        LEFT JOIN inquiries i ON i.id = o.inquiry_id
        LEFT JOIN customers c ON c.id = i.customer_id
        WHERE o.status <> 'superseded'
        ORDER BY COALESCE(o.sent_at, o.created_at) ASC
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Inquiry statuses that mean the KVA is still undecided — the only ones the
/// Nachfassliste and the follow-up nag consider.
///
/// Deliberately reads `inquiries.status`, not `offers.status`: the latter is not
/// maintained (see `KvaBuchRow::inquiry_status`).
pub(crate) const OPEN_INQUIRY_STATUSES: &[&str] = &["offer_sent", "offer_ready"];

/// One KVA that has gone quiet and is still worth chasing.
#[derive(Debug, FromRow)]
pub(crate) struct FollowupCandidate {
    pub id: Uuid,
    pub offer_number: Option<String>,
    pub customer_name: Option<String>,
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub scheduled_date: NaiveDate,
    pub followup_last_pinged_on: Option<NaiveDate>,
}

/// Fetch the KVAs eligible for a follow-up ping.
///
/// **Caller**: `kva_followup_service::fire_due_followups`
/// **Why**: Chasing a KVA only earns money when both conditions hold — it has been
/// quiet longer than the threshold AND the move still lies ahead. A KVA whose
/// `scheduled_date` has passed is dead; pinging about it is pure noise. On
/// production 17 of 33 open KVAs are in exactly that state, which is why the
/// `scheduled_date > today` filter is not optional.
///
/// A NULL `scheduled_date` does **not** qualify: "both conditions fit" is strict,
/// and a KVA without a move date cannot be shown to still be live. Those rows are
/// surfaced separately in the UI instead of being pinged.
pub(crate) async fn fetch_followup_candidates(
    pool: &PgPool,
    today: NaiveDate,
    threshold_days: i64,
) -> Result<Vec<FollowupCandidate>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            o.id,
            o.offer_number,
            o.price_cents,
            o.created_at,
            o.followup_last_pinged_on,
            c.name AS customer_name,
            i.scheduled_date
        FROM offers o
        JOIN inquiries i ON i.id = o.inquiry_id
        LEFT JOIN customers c ON c.id = i.customer_id
        WHERE o.status <> 'superseded'
          AND NOT o.followup_muted
          AND i.status = ANY($3)
          AND i.scheduled_date IS NOT NULL
          AND i.scheduled_date > $1
          -- Exclusive, matching `age_days > threshold_days` in the KVA-Buch route:
          -- a KVA exactly at the threshold is not yet overdue. The two must agree
          -- or a row would ping on Telegram without being flagged in the UI.
          AND (o.created_at AT TIME ZONE 'Europe/Berlin')::date < $1 - ($2 || ' days')::interval
        ORDER BY o.created_at ASC
        "#,
    )
    .bind(today)
    .bind(threshold_days.to_string())
    .bind(OPEN_INQUIRY_STATUSES)
    .fetch_all(pool)
    .await
}

/// Record that a follow-up ping went out for `offer_id` on `today`.
///
/// **Caller**: `kva_followup_service::fire_due_followups`, after Telegram accepts.
pub(crate) async fn mark_followup_pinged(
    pool: &PgPool,
    offer_id: Uuid,
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE offers SET followup_last_pinged_on = $2 WHERE id = $1")
        .bind(offer_id)
        .bind(today)
        .execute(pool)
        .await?;
    Ok(())
}

/// Silence (or un-silence) follow-up pings for one KVA.
///
/// **Caller**: `admin::set_kva_followup_mute` — the mute toggle in the KVA-Buch.
/// **Why**: Alex needs to drop a single KVA off the Nachfassliste without moving
/// the threshold for every other one.
pub(crate) async fn set_followup_muted(
    pool: &PgPool,
    offer_id: Uuid,
    muted: bool,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("UPDATE offers SET followup_muted = $2 WHERE id = $1")
        .bind(offer_id)
        .bind(muted)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Fetch all PDF storage keys for an inquiry's offers.
/// Used for S3 cleanup when deleting an inquiry.
pub(crate) async fn fetch_all_pdf_keys(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT pdf_storage_key FROM offers WHERE inquiry_id = $1 AND pdf_storage_key IS NOT NULL",
    )
    .bind(inquiry_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(k,)| k).collect())
}

#[cfg(test)]
mod kva_buch_tests {
    use super::*;
    use crate::test_helpers;

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

    /// Pins the KVA-Buch projection (a runtime-typed `query_as`) and the one row class
    /// it deliberately hides: `superseded` drafts were replaced before anyone saw them.
    #[sqlx::test(migrations = "../../migrations")]
    async fn lists_offers_and_hides_superseded_drafts(pool: PgPool) {
        let kept = seed_inquiry(&pool, "offer_sent").await;
        let replaced = seed_inquiry(&pool, "estimated").await;

        let kept_offer = test_helpers::insert_test_offer(&pool, kept, "sent").await;
        test_helpers::insert_test_offer(&pool, replaced, "superseded").await;

        let rows = list_for_kva_buch(&pool).await.unwrap();
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

        assert!(ids.contains(&kept_offer), "sent KVA missing from the register");
        assert_eq!(rows.len(), 1, "superseded draft must not appear: {rows:?}");

        let row = &rows[0];
        assert_eq!(row.inquiry_id, kept);
        assert_eq!(row.status, "sent");
        assert_eq!(row.price_cents, 50_000);
        assert!(row.customer_name.is_some());
        assert_eq!(row.pdf_storage_key.as_deref(), Some("test.pdf"));
        // No invoice exists yet — the KVA has not become a job.
        assert_eq!(row.invoice_number, None);
    }

    /// A rejected or expired KVA still belongs in the register — only `superseded`
    /// is filtered, and `offers.status` is NOT NULL so nothing drops silently.
    #[sqlx::test(migrations = "../../migrations")]
    async fn keeps_rejected_and_expired_offers(pool: PgPool) {
        let a = seed_inquiry(&pool, "rejected").await;
        let b = seed_inquiry(&pool, "expired").await;
        test_helpers::insert_test_offer(&pool, a, "rejected").await;
        test_helpers::insert_test_offer(&pool, b, "expired").await;

        let rows = list_for_kva_buch(&pool).await.unwrap();
        let statuses: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
        assert!(statuses.contains(&"rejected"), "{statuses:?}");
        assert!(statuses.contains(&"expired"), "{statuses:?}");
    }

    /// Regression guard for the Kostenvoranschlag letterhead: this query used to omit
    /// `billing_address_id`, and because `InquiryRow` marked the field `#[sqlx(default)]`
    /// the omission silently produced `None`. Every KVA therefore ignored the
    /// Rechnungsadresse and addressed the customer at the Beladestelle instead — and
    /// regenerating the offer could not fix it, because the value never reached the
    /// builder. Live example: offer 2026-0267 (erfi Ernst Fischer GmbH) went to
    /// "Bundesallee 100, Braunschweig" instead of "Alte Poststr. 8, Freudenstadt".
    #[sqlx::test(migrations = "../../migrations")]
    async fn offer_query_carries_the_inquiry_billing_address(pool: PgPool) {
        let customer_id = test_helpers::insert_test_customer(&pool).await;
        let origin = test_helpers::insert_test_address(
            &pool, "Bundesallee 100", "Braunschweig", "38116", None, None,
        )
        .await;
        let dest = test_helpers::insert_test_address(
            &pool, "Zielstr. 5", "Hannover", "30159", None, None,
        )
        .await;
        let billing = test_helpers::insert_test_address(
            &pool, "Alte Poststr. 8", "Freudenstadt", "72250", None, None,
        )
        .await;
        let inquiry_id = test_helpers::insert_test_inquiry_full(
            &pool, customer_id, origin, dest, "offer_ready", "termin", None,
        )
        .await;
        sqlx::query("UPDATE inquiries SET billing_address_id = $1 WHERE id = $2")
            .bind(billing)
            .bind(inquiry_id)
            .execute(&pool)
            .await
            .expect("set billing address");

        let row = fetch_inquiry_for_offer(&pool, inquiry_id)
            .await
            .unwrap()
            .expect("inquiry missing");

        assert_eq!(
            row.inquiry_billing_address_id,
            Some(billing),
            "the KVA builder must see the Rechnungsadresse, not fall back to origin"
        );
    }

    /// Regenerating a KVA reuses the *active* offer. A `superseded` row is newer than the
    /// draft that replaced it in `created_at` terms whenever the draft was updated in place,
    /// so picking it would UPDATE it back to `draft` and collide with the live draft on
    /// `offers_inquiry_active_unique` (prod 500 on 2026-08-27).
    #[sqlx::test(migrations = "../../migrations")]
    async fn active_offer_lookup_skips_superseded_rows(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool, "offer_ready").await;
        let active = test_helpers::insert_test_offer(&pool, inquiry_id, "draft").await;
        let superseded = test_helpers::insert_test_offer(&pool, inquiry_id, "superseded").await;
        assert!(superseded > active, "the superseded row must be the newer one");

        assert_eq!(
            fetch_active_id_for_inquiry(&pool, inquiry_id).await.unwrap(),
            Some(active)
        );
    }

    /// A regeneration keeps the KVA number it was issued under; only the concurrent-write
    /// takeover in `build_offer_with_overrides` passes a number, because its PDF already
    /// carries a different one.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_keeps_the_offer_number_unless_one_is_passed(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool, "offer_ready").await;
        let offer_id = test_helpers::insert_test_offer(&pool, inquiry_id, "draft").await;
        sqlx::query("UPDATE offers SET offer_number = '2026-9001' WHERE id = $1")
            .bind(offer_id)
            .execute(&pool)
            .await
            .expect("set offer number");

        let kept = update_returning(
            &pool, offer_id, 60000, Some("neu.pdf"), "draft", 3, 5.0, 3500, &None, None, None,
        )
        .await
        .unwrap();
        assert_eq!(kept.offer_number.as_deref(), Some("2026-9001"));

        let adopted = update_returning(
            &pool, offer_id, 60000, Some("neu.pdf"), "draft", 3, 5.0, 3500, &None, None,
            Some("2026-9002"),
        )
        .await
        .unwrap();
        assert_eq!(adopted.offer_number.as_deref(), Some("2026-9002"));
    }

    /// The register links each row to its own document, including non-active offers.
    #[sqlx::test(migrations = "../../migrations")]
    async fn fetches_a_specific_offers_pdf_key(pool: PgPool) {
        let inquiry_id = seed_inquiry(&pool, "rejected").await;
        let offer_id = test_helpers::insert_test_offer(&pool, inquiry_id, "rejected").await;

        assert_eq!(
            fetch_pdf_key_by_id(&pool, offer_id).await.unwrap(),
            Some(Some("test.pdf".to_string()))
        );
        // Outer None distinguishes "no such offer" from "offer without a file".
        assert_eq!(fetch_pdf_key_by_id(&pool, Uuid::now_v7()).await.unwrap(), None);
    }
}
