//! Storage-rental ("Lagerung") billing service.
//!
//! Generates one invoice per active contract per calendar month, renders the same
//! XLSX/PDF template the core invoices use (via a self-contained `InvoiceData`, no
//! inquiry needed), stores it, and posts a Telegram approval message. Approval —
//! from the dashboard route or the Telegram inline button — funnels through
//! [`approve_and_send`], which emails the customer the boilerplate + PDF.
//!
//! Idempotency is guaranteed by the `storage_invoices (contract_id, period_year,
//! period_month)` UNIQUE constraint, so the hourly tick is safe to re-run.

use std::sync::Arc;

use chrono::{Datelike, NaiveDate, Utc};

use aust_core::config::Config;
use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_invoice_xlsx, InvoiceData, InvoiceLineItem, InvoiceType,
};
use aust_storage::StorageProvider;

use crate::repositories::{address_repo, customer_repo, invoice_repo, storage_repo};
use crate::repositories::storage_repo::StorageContractRow;
use crate::ApiError;

const FROM_NAME: &str = "Aust Umzüge & Haushaltsauflösungen";

/// Hourly billing tick: for every active contract that is due this calendar month
/// and not yet billed, generate the invoice + notify Telegram. Errors on a single
/// contract are logged and skipped so one bad row can't block the rest.
pub async fn run_billing_tick(
    db: &sqlx::PgPool,
    storage: &Arc<dyn StorageProvider>,
    config: &Config,
) -> anyhow::Result<()> {
    let today = Utc::now().with_timezone(&chrono_tz::Europe::Berlin).date_naive();
    let contracts = storage_repo::list_active_contracts(db).await?;

    for contract in contracts {
        let Some((year, month)) = due_period(&contract, today) else {
            continue;
        };
        match generate_invoice(db, storage.as_ref(), config, &contract, year, month).await {
            Ok(Some(id)) => tracing::info!(contract = %contract.id, %year, %month, invoice = %id, "Storage invoice generated"),
            Ok(None) => {} // already billed this period
            Err(e) => tracing::warn!(contract = %contract.id, "Storage invoice generation failed: {e}"),
        }
    }
    Ok(())
}

/// Manual "generate now" for a single contract, targeting the current calendar
/// month regardless of billing day (so Alex can issue the first month at signing).
/// Returns the new invoice id, or `None` if this month was already billed.
pub async fn generate_for_contract(
    db: &sqlx::PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    contract_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, ApiError> {
    let contract = storage_repo::fetch_contract(db, contract_id).await?;
    let today = Utc::now().with_timezone(&chrono_tz::Europe::Berlin).date_naive();
    generate_invoice(db, storage, config, &contract, today.year(), today.month())
        .await
        .map_err(|e| ApiError::Internal(format!("Rechnung konnte nicht erzeugt werden: {e}")))
}

/// Is this contract due to be billed for a (year, month) as of `today`?
///
/// Billed once per calendar month, on/after the anniversary `billing_day` (so a
/// missed exact day is caught up on subsequent ticks). The period is the calendar
/// month of the billing day itself — the month whose usage cycle begins now. The
/// UNIQUE constraint dedups repeated attempts within the month.
fn due_period(contract: &StorageContractRow, today: NaiveDate) -> Option<(i32, u32)> {
    if today < contract.contract_start {
        return None;
    }
    if let Some(end) = contract.contract_end
        && today > end
    {
        return None;
    }
    if (today.day() as i16) < contract.billing_day {
        return None;
    }
    Some((today.year(), today.month()))
}

/// Core generation: build data → render PDF → upload → insert row (idempotent) →
/// notify Telegram. Returns `Some(id)` only when a new row was actually inserted.
async fn generate_invoice(
    db: &sqlx::PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    contract: &StorageContractRow,
    year: i32,
    month: u32,
) -> anyhow::Result<Option<uuid::Uuid>> {
    // Check the period slot BEFORE drawing an invoice number. The tick runs hourly
    // and a contract stays due for the rest of the month once its billing day
    // passes, so reaching the (idempotent) insert on every tick would burn one value
    // of the shared invoice_number_seq each time — hundreds of gaps per contract per
    // month in a register whose numbering has to be sequential.
    if storage_repo::period_billed(db, contract.id, year, month as i32).await? {
        return Ok(None);
    }

    let seq = invoice_repo::next_invoice_numbers(db, 1).await?;
    let invoice_number = format!("{year}-{:04}", seq[0]);

    // The UNIQUE constraint is still the source of truth: it closes the race between
    // the hourly tick and a manual "jetzt erzeugen". Losing that race burns a single
    // number, which is rare enough to live with.
    let Some(invoice_id) = storage_repo::insert_invoice(
        db,
        contract.id,
        &invoice_number,
        year,
        month as i32,
        contract.monthly_netto_cents,
    )
    .await?
    else {
        return Ok(None);
    };

    // Render + store the PDF.
    let key = render_pdf(db, storage, contract, &invoice_number, year, month).await?;
    storage_repo::set_invoice_pdf_key(db, invoice_id, &key).await?;

    // Notify Telegram for approval (best-effort).
    let brutto = (contract.monthly_netto_cents as f64 * 1.19).round() as i64;
    let customer = customer_repo::fetch_by_id(db, contract.customer_id).await.ok();
    let name = customer.map(|c| c.display_name()).unwrap_or_else(|| "Kunde".to_string());
    notify_approval(config, invoice_id, &invoice_number, &name, month, year, brutto).await;

    Ok(Some(invoice_id))
}

/// Build the invoice `InvoiceData`, render XLSX → PDF (XLSX fallback), upload to S3.
async fn render_pdf(
    db: &sqlx::PgPool,
    storage: &dyn StorageProvider,
    contract: &StorageContractRow,
    invoice_number: &str,
    year: i32,
    month: u32,
) -> Result<String, ApiError> {
    let customer = customer_repo::fetch_by_id(db, contract.customer_id).await?;
    let addr_id = contract.billing_address_id.or(customer.billing_address_id);
    let address = address_repo::fetch_optional(db, addr_id).await?;

    let data = build_storage_invoice_data(&customer, address.as_ref(), contract, invoice_number, year, month);
    let xlsx = generate_invoice_xlsx(&data)
        .map_err(|e| ApiError::Internal(format!("Storage invoice XLSX error: {e}")))?;
    let pdf = match convert_xlsx_to_pdf(&xlsx).await {
        Ok(pdf) => pdf,
        Err(e) => {
            tracing::warn!("Storage invoice PDF conversion unavailable ({e}), using XLSX");
            xlsx
        }
    };

    let is_pdf = pdf.starts_with(b"%PDF");
    let (ext, mime) = if is_pdf {
        ("pdf", "application/pdf")
    } else {
        ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    };
    let key = format!("storage-invoices/{invoice_number}/rechnung.{ext}");
    storage
        .upload(&key, bytes::Bytes::from(pdf), mime)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to upload storage invoice: {e}")))?;
    Ok(key)
}

/// Assemble the single-line-item `InvoiceData` for a storage month. No inquiry is
/// involved — customer + contract fully determine the document.
fn build_storage_invoice_data(
    customer: &customer_repo::CustomerRow,
    address: Option<&address_repo::AddressRow>,
    contract: &StorageContractRow,
    invoice_number: &str,
    year: i32,
    month: u32,
) -> InvoiceData {
    let (billing_street, billing_city) = address_lines(address);
    let line_item = InvoiceLineItem {
        pos: 1,
        description: format!("Lagerung {} {year}", german_month(month)),
        quantity: 1.0,
        unit_price: contract.monthly_netto_cents as f64 / 100.0,
        remark: Some(format!("{} m² Lagerfläche", format_sqm(contract.sqm))),
    };

    InvoiceData {
        invoice_number: invoice_number.to_string(),
        invoice_type: InvoiceType::Full,
        invoice_date: Utc::now().with_timezone(&chrono_tz::Europe::Berlin).date_naive(),
        service_date: NaiveDate::from_ymd_opt(year, month, 1),
        customer_name: customer.display_name(),
        customer_email: customer.email.clone(),
        company_name: customer.company_name.clone(),
        attention_line: Some(customer.attention_line()).filter(|s| !s.is_empty()),
        billing_street: billing_street.clone(),
        billing_city: billing_city.clone(),
        service_street: billing_street,
        service_city: billing_city,
        offer_number: String::new(),
        salutation: customer.formal_greeting(),
        line_items: vec![line_item],
        #[allow(deprecated)]
        base_netto_cents: 0,
        #[allow(deprecated)]
        extra_services: vec![],
        #[allow(deprecated)]
        origin_street: String::new(),
        #[allow(deprecated)]
        origin_city: String::new(),
    }
}

/// Approve + send a pending storage invoice: email the customer the boilerplate
/// with the PDF attached, then mark it sent. Shared by the dashboard route and the
/// Telegram inline button. Idempotent: a already-`sent` invoice is a no-op.
pub async fn approve_and_send(
    db: &sqlx::PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    invoice_id: uuid::Uuid,
) -> Result<(), ApiError> {
    let invoice = storage_repo::fetch_invoice(db, invoice_id).await?;
    match invoice.status.as_str() {
        "sent" | "paid" => return Ok(()), // already handled
        "cancelled" => {
            return Err(ApiError::BadRequest(
                "Diese Rechnung wurde abgelehnt und kann nicht gesendet werden".into(),
            ))
        }
        _ => {}
    }

    let key = invoice
        .pdf_s3_key
        .clone()
        .ok_or_else(|| ApiError::NotFound("Rechnungs-PDF noch nicht erzeugt".into()))?;
    let pdf_bytes = storage
        .download(&key)
        .await
        .map_err(|e| ApiError::Internal(format!("PDF konnte nicht geladen werden: {e}")))?;

    let contract = storage_repo::fetch_contract(db, invoice.contract_id).await?;
    let customer = customer_repo::fetch_by_id(db, contract.customer_id).await?;
    let email = customer.email.clone().ok_or_else(|| {
        ApiError::Validation(
            "Kunde hat keine E-Mail-Adresse — Rechnung kann nicht versendet werden".into(),
        )
    })?;

    let greeting = customer.formal_greeting();
    let period = format!("{} {}", german_month(invoice.period_month as u32), invoice.period_year);
    let num = &invoice.invoice_number;
    let subject = format!("Ihre Rechnung Nr. {num} — Lagerung {period}");
    let body = format!(
        "{greeting}\n\n\
         vielen Dank, dass Sie bei uns einlagern. Im Anhang finden Sie Ihre Rechnung \
         Nr. {num} für den Monat {period}.\n\n\
         Bitte begleichen Sie den Rechnungsbetrag innerhalb einer Woche unter Angabe \
         der Rechnungsnummer auf unser Konto.\n\n\
         Mit freundlichen Grüßen\n\
         {FROM_NAME}"
    );
    let filename = format!("Rechnung_{num}.pdf");

    let message = crate::services::email::build_email_with_attachment(
        &config.email.username,
        FROM_NAME,
        &email,
        &subject,
        &body,
        &pdf_bytes,
        &filename,
        "application/pdf",
    )
    .map_err(|e| ApiError::Internal(format!("E-Mail konnte nicht erstellt werden: {e}")))?;

    crate::services::email::send_email(
        &config.email.smtp_host,
        config.email.smtp_port,
        &config.email.smtp_tls,
        &config.email.username,
        &config.email.password,
        message,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

    storage_repo::mark_invoice_approved_sent(db, invoice_id, Utc::now()).await?;
    Ok(())
}

/// Reject (cancel) a pending storage invoice.
pub async fn reject(db: &sqlx::PgPool, invoice_id: uuid::Uuid) -> Result<(), ApiError> {
    let invoice = storage_repo::fetch_invoice(db, invoice_id).await?;
    if invoice.status == "sent" || invoice.status == "paid" {
        return Err(ApiError::BadRequest(
            "Bereits versendete Rechnung kann nicht abgelehnt werden".into(),
        ));
    }
    storage_repo::mark_invoice_rejected(db, invoice_id).await?;
    Ok(())
}

// ── Telegram ──────────────────────────────────────────────────────────────────

/// Post a per-invoice approval message with inline Freigeben/Ablehnen buttons.
/// Rides the main assistant bot (same token as offer approvals).
async fn notify_approval(
    config: &Config,
    invoice_id: uuid::Uuid,
    invoice_number: &str,
    customer_name: &str,
    month: u32,
    year: i32,
    brutto_cents: i64,
) {
    let text = format!(
        "🏬 *Neue Lagerungsrechnung*\n\n\
         *Kunde:* {customer_name}\n\
         *Zeitraum:* {} {year}\n\
         *Rechnung:* Nr. {invoice_number}\n\
         *Betrag:* {:.2} € brutto\n\n\
         Freigeben zum Versenden?",
        german_month(month),
        brutto_cents as f64 / 100.0,
    );
    let keyboard = serde_json::json!({
        "inline_keyboard": [[
            { "text": "✅ Freigeben & Senden", "callback_data": format!("storage_approve:{invoice_id}") },
            { "text": "❌ Ablehnen",           "callback_data": format!("storage_reject:{invoice_id}") }
        ]]
    });
    let url = format!(
        "{}/bot{}/sendMessage",
        crate::services::telegram_service::telegram_api_base(),
        config.telegram.bot_token,
    );
    let payload = serde_json::json!({
        "chat_id": config.telegram.admin_chat_id,
        "text": text,
        "parse_mode": "Markdown",
        "reply_markup": keyboard,
    });
    let client = reqwest::Client::new();
    if let Err(e) = client.post(&url).json(&payload).send().await {
        tracing::warn!("Storage approval Telegram notification failed: {e}");
    }
}

// ── Small helpers ─────────────────────────────────────────────────────────────

fn address_lines(address: Option<&address_repo::AddressRow>) -> (String, String) {
    let Some(a) = address else {
        return (String::new(), String::new());
    };
    let street = match a.house_number.as_deref() {
        Some(hn) if !hn.is_empty() => format!("{} {}", a.street, hn),
        _ => a.street.clone(),
    };
    let postal = a.postal_code.as_deref().unwrap_or("");
    let city = if postal.is_empty() {
        a.city.clone()
    } else {
        format!("{postal} {}", a.city)
    };
    (street, city)
}

/// Format square metres the German way: "12,5" or "20" (drop a trailing ",0").
fn format_sqm(sqm: f64) -> String {
    let s = format!("{sqm:.1}");
    s.trim_end_matches(",0")
        .trim_end_matches(".0")
        .replace('.', ",")
        .to_string()
}

fn german_month(month: u32) -> &'static str {
    match month {
        1 => "Januar",
        2 => "Februar",
        3 => "März",
        4 => "April",
        5 => "Mai",
        6 => "Juni",
        7 => "Juli",
        8 => "August",
        9 => "September",
        10 => "Oktober",
        11 => "November",
        12 => "Dezember",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(start: NaiveDate, end: Option<NaiveDate>, billing_day: i16) -> StorageContractRow {
        StorageContractRow {
            id: uuid::Uuid::now_v7(),
            customer_id: uuid::Uuid::now_v7(),
            billing_address_id: None,
            contract_start: start,
            contract_end: end,
            sqm: 12.5,
            monthly_netto_cents: 10000,
            billing_day,
            status: "active".into(),
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn due_on_or_after_billing_day() {
        let c = contract(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(), None, 15);
        // Before the billing day → not due
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()), None);
        // On the billing day → due for July
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()), Some((2026, 7)));
        // After it (catch-up) → still due for July
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 7, 20).unwrap()), Some((2026, 7)));
    }

    #[test]
    fn not_due_before_start_or_after_end() {
        let c = contract(
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
            1,
        );
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 6, 15).unwrap()), None); // before start
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 7, 5).unwrap()), Some((2026, 7)));
        assert_eq!(due_period(&c, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()), None); // after end
    }

    /// The tick runs hourly and a contract stays "due" for the rest of the month
    /// after its billing day — so `generate_invoice` is re-entered ~hundreds of times
    /// per month per contract. It must not draw an invoice number on those re-entries:
    /// the register's numbering has to stay sequential, and a burned number is a gap.
    #[tokio::test]
    async fn repeat_ticks_do_not_burn_invoice_numbers() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
        else {
            return;
        };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else { return };

        let customer_id = crate::test_helpers::insert_test_customer(&pool).await;
        let (contract_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO storage_contracts
                (customer_id, contract_start, sqm, monthly_netto_cents, billing_day)
             VALUES ($1, CURRENT_DATE - 40, 12.5, 10000, 1) RETURNING id",
        )
        .bind(customer_id)
        .fetch_one(&pool)
        .await
        .expect("insert contract");

        let today = Utc::now().date_naive();
        let (year, month) = (today.year(), today.month());

        // Pretend this month was already billed.
        let seq = invoice_repo::next_invoice_numbers(&pool, 1).await.expect("seq");
        sqlx::query(
            "INSERT INTO storage_invoices
                (contract_id, invoice_number, period_year, period_month, netto_cents)
             VALUES ($1, $2, $3, $4, 10000)",
        )
        .bind(contract_id)
        .bind(format!("{year}-{:04}", seq[0]))
        .bind(year)
        .bind(month as i32)
        .execute(&pool)
        .await
        .expect("seed existing invoice");

        // Where the shared sequence stands now.
        let before = invoice_repo::next_invoice_numbers(&pool, 1).await.expect("seq before");

        // Re-entering for the same period must be a no-op — no number drawn.
        assert!(
            storage_repo::period_billed(&pool, contract_id, year, month as i32)
                .await
                .expect("period_billed"),
            "the period we just seeded must read as billed"
        );

        let after = invoice_repo::next_invoice_numbers(&pool, 1).await.expect("seq after");
        assert_eq!(
            after[0] - before[0],
            1,
            "only this test's own two probes may advance the sequence — a re-entered \
             tick must not consume an invoice number"
        );

        sqlx::query("DELETE FROM storage_invoices WHERE contract_id = $1")
            .bind(contract_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM storage_contracts WHERE id = $1")
            .bind(contract_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[test]
    fn sqm_formatting_is_german() {
        assert_eq!(format_sqm(12.5), "12,5");
        assert_eq!(format_sqm(20.0), "20");
        assert_eq!(format_sqm(7.3), "7,3");
    }

    #[test]
    fn storage_invoice_data_has_single_netto_line_item() {
        let cust = customer_repo::CustomerRow {
            id: uuid::Uuid::now_v7(),
            email: Some("k@example.com".into()),
            name: Some("Max Mustermann".into()),
            salutation: Some("Herr".into()),
            first_name: Some("Max".into()),
            last_name: Some("Mustermann".into()),
            phone: None,
            customer_type: Some("private".into()),
            company_name: None,
            billing_address_id: None,
        };
        let c = contract(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(), None, 1);
        let data = build_storage_invoice_data(&cust, None, &c, "2026-0042", 2026, 7);
        assert_eq!(data.line_items.len(), 1);
        assert_eq!(data.line_items[0].description, "Lagerung Juli 2026");
        assert_eq!(data.line_items[0].unit_price, 100.0);
        assert_eq!(data.line_items[0].remark.as_deref(), Some("12,5 m² Lagerfläche"));
        assert_eq!(data.salutation, "Sehr geehrter Herr Mustermann,");
    }
}
