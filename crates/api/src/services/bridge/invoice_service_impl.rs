//! Bridge impl for `InvoiceService`.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::services::{
    DueDunning, InvoiceDetail, InvoiceReminder, InvoiceService, InvoiceSummary, ServiceError,
};
use aust_storage::StorageProvider;

use crate::repositories::invoice_repo;
use crate::services::billing_reminder_service;
use aust_offer_generator::{convert_xlsx_to_pdf, generate_invoice_xlsx, InvoiceType, InvoiceLineItem};

pub struct InvoiceServiceImpl {
    pool: PgPool,
    storage: Arc<dyn StorageProvider>,
    /// Needed to send dunning mail — the assistant can escalate a Mahnung itself.
    config: Arc<aust_core::Config>,
}

impl InvoiceServiceImpl {
    pub fn new(
        pool: PgPool,
        storage: Arc<dyn StorageProvider>,
        config: Arc<aust_core::Config>,
    ) -> Self {
        Self { pool, storage, config }
    }
}

#[async_trait]
impl InvoiceService for InvoiceServiceImpl {
    async fn create_from_inquiry(
        &self,
        inquiry_id: Uuid,
    ) -> Result<InvoiceSummary, ServiceError> {
        // Validate inquiry status — must be accepted or later.
        let status_str = invoice_repo::fetch_inquiry_status(&self.pool, inquiry_id)
            .await
            .map_err(super::map_sqlx)?
            .ok_or_else(|| ServiceError::NotFound(format!("Anfrage {inquiry_id} nicht gefunden")))?;

        let allowed = matches!(
            status_str.as_str(),
            "accepted" | "scheduled" | "completed" | "invoiced" | "paid"
        );
        if !allowed {
            return Err(ServiceError::Validation(format!(
                "Rechnungen können nur für angenommene oder spätere Anfragen erstellt werden (aktueller Status: {status_str})"
            )));
        }

        // Load active offer.
        let offer = invoice_repo::fetch_active_offer(&self.pool, inquiry_id)
            .await
            .map_err(super::map_sqlx)?
            .ok_or_else(|| ServiceError::Validation(
                "Kein aktives Angebot vorhanden — bitte erst ein Angebot erstellen.".to_string(),
            ))?;

        if offer.price_cents <= 0 {
            return Err(ServiceError::Validation(
                "Angebotsbetrag muss größer als 0 sein, um eine Rechnung zu erstellen.".to_string(),
            ));
        }

        // Return existing invoices idempotently.
        let existing = invoice_repo::list_by_inquiry(&self.pool, inquiry_id)
            .await
            .map_err(super::map_sqlx)?;
        if let Some(row) = existing.into_iter().next() {
            return Ok(InvoiceSummary {
                id: row.id,
                invoice_number: row.invoice_number,
                status: row.status,
                due_date: None,
                sent_at: row.sent_at,
            });
        }

        // Generate invoice number.
        let now = Utc::now();
        let today = now.date_naive();
        let year = today.year();
        let seqs = invoice_repo::next_invoice_numbers(&self.pool, 1, year)
            .await
            .map_err(super::map_sqlx)?;
        let invoice_num = crate::services::invoice_number::format(year, seqs[0]);
        let inv_id = Uuid::now_v7();

        // Build a minimal line item from the offer.
        let kva_nr = offer.offer_number.as_deref().unwrap_or("");
        let line_items = vec![InvoiceLineItem {
            pos: 1,
            description: format!("Umzugsdienstleistung gemäß Angebot Nr. {kva_nr}"),
            quantity: 1.0,
            unit_price: offer.price_cents as f64 / 100.0,
            remark: None,
        }];

        // Load customer info for the invoice.
        let customer = crate::repositories::customer_repo::fetch_by_inquiry_id(
            &self.pool,
            inquiry_id,
        )
        .await
        .map_err(|e| ServiceError::External(anyhow::anyhow!(e.to_string())))?;

        let moving_date = invoice_repo::fetch_moving_date(&self.pool, inquiry_id)
            .await
            .map_err(super::map_sqlx)?;

        let customer_name = match (customer.first_name.as_deref(), customer.last_name.as_deref()) {
            (Some(f), Some(l)) => format!("{f} {l}"),
            _ => customer.name.clone().unwrap_or_else(|| "Kunde".to_string()),
        };

        let invoice_data = aust_offer_generator::InvoiceData {
            invoice_number: invoice_num.clone(),
            invoice_type: InvoiceType::Full,
            invoice_date: today,
            service_date: moving_date,
            customer_name,
            customer_email: customer.email.clone(),
            company_name: customer.company_name.clone(),
            attention_line: Some(customer.attention_line()).filter(|s| !s.is_empty()),
            billing_street: String::new(),
            billing_city: String::new(),
            service_street: String::new(),
            service_city: String::new(),
            offer_number: kva_nr.to_string(),
            salutation: customer.formal_greeting(),
            line_items,
            #[allow(deprecated)]
            base_netto_cents: 0,
            #[allow(deprecated)]
            extra_services: vec![],
            #[allow(deprecated)]
            origin_street: String::new(),
            #[allow(deprecated)]
            origin_city: String::new(),
        };

        let xlsx = generate_invoice_xlsx(&invoice_data)
            .map_err(|e| ServiceError::External(anyhow::anyhow!("Invoice XLSX error: {e}")))?;

        let pdf = match convert_xlsx_to_pdf(&xlsx).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Invoice PDF conversion failed ({e}), using XLSX fallback");
                xlsx.clone()
            }
        };

        let is_pdf = pdf.starts_with(b"%PDF");
        let (ext, mime) = if is_pdf {
            ("pdf", "application/pdf")
        } else {
            ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        };
        let s3_key = format!("invoices/{inv_id}/rechnung.{ext}");
        // The number is already off the sequence and cannot be handed back, so an S3
        // failure must not abort: that would leave a permanent gap in the register.
        // The row is written without a document and `ensure_invoice_pdf` rebuilds it.
        let stored_key = match self.storage.upload(&s3_key, bytes::Bytes::from(pdf), mime).await {
            Ok(_) => Some(s3_key.as_str()),
            Err(e) => {
                tracing::error!(
                    invoice_id = %inv_id,
                    invoice_number = %invoice_num,
                    "Invoice upload failed; keeping the numbered row so the register stays gapless: {e}"
                );
                None
            }
        };

        invoice_repo::insert_full(
            &self.pool,
            inv_id,
            inquiry_id,
            &invoice_num,
            offer.price_cents,
            stored_key,
            now,
        )
        .await
        .map_err(super::map_sqlx)?;

        Ok(InvoiceSummary {
            id: inv_id,
            invoice_number: invoice_num,
            status: "ready".to_string(),
            due_date: None,
            sent_at: None,
        })
    }

    async fn list(&self, status_filter: Option<&str>) -> Result<Vec<InvoiceSummary>, ServiceError> {
        let rows: Vec<(
            Uuid,
            String,
            String,
            Option<NaiveDate>,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            r#"
            SELECT id, invoice_number, status, due_date, sent_at
            FROM invoices
            WHERE ($1::TEXT IS NULL OR status = $1)
            ORDER BY created_at DESC
            LIMIT 50
            "#,
        )
        .bind(status_filter)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, invoice_number, status, due_date, sent_at)| InvoiceSummary {
                id,
                invoice_number,
                status,
                due_date,
                sent_at,
            })
            .collect())
    }

    async fn get(&self, id: Uuid) -> Result<InvoiceDetail, ServiceError> {
        let row: Option<(
            Uuid,
            String,
            Option<Uuid>,
            String,
            Option<NaiveDate>,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT id, invoice_number, inquiry_id, status, due_date, sent_at, created_at
            FROM invoices
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, invoice_number, inquiry_id, status, due_date, sent_at, created_at) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Rechnung {id}")))?;

        Ok(InvoiceDetail { id, invoice_number, inquiry_id, status, due_date, sent_at, created_at })
    }

    async fn list_reminders(
        &self,
        invoice_id: Uuid,
    ) -> Result<Vec<InvoiceReminder>, ServiceError> {
        let rows: Vec<(
            Uuid,
            Uuid,
            i32,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            r#"
            SELECT id, invoice_id, level, sent_at, created_at
            FROM invoice_reminders
            WHERE invoice_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(invoice_id)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, invoice_id, level, sent_at, created_at)| InvoiceReminder {
                id,
                invoice_id,
                level,
                sent_at,
                created_at,
            })
            .collect())
    }

    async fn update_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<InvoiceDetail, ServiceError> {
        // "paid" is not just a status write: it has to stamp paid_at, close the
        // dunning step, and settle the inquiry. Route it through the shared path so
        // the assistant can't leave an invoice that reads paid but still nags.
        if status == "paid" {
            self.mark_paid(id).await?;
            return self.get(id).await;
        }

        sqlx::query("UPDATE invoices SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        self.get(id).await
    }

    async fn list_due_dunning(&self) -> Result<Vec<DueDunning>, ServiceError> {
        let rows = billing_reminder_service::list_due_invoice_reminders(&self.pool)
            .await
            .map_err(super::map_api)?;
        Ok(rows
            .into_iter()
            .map(|r| DueDunning {
                reminder_id: r.id,
                invoice_id: r.invoice_id,
                inquiry_id: r.inquiry_id,
                invoice_number: r.invoice_number,
                level: r.level,
                level_label: r.level_label.to_string(),
                remind_after: r.remind_after,
                days_overdue: r.days_overdue,
                customer_name: r.customer_name,
                customer_email: r.customer_email,
            })
            .collect())
    }

    async fn send_dunning(&self, reminder_id: Uuid) -> Result<(i32, String), ServiceError> {
        let (level, label) =
            billing_reminder_service::send_dunning(&self.pool, &self.config.email, reminder_id)
                .await
                .map_err(super::map_api)?;
        Ok((level, label.to_string()))
    }

    async fn mark_paid(&self, invoice_id: Uuid) -> Result<(), ServiceError> {
        billing_reminder_service::mark_invoice_paid(&self.pool, invoice_id, Utc::now())
            .await
            .map_err(super::map_api)?;
        Ok(())
    }

    async fn record_payment(
        &self,
        invoice_id: Uuid,
        amount_cents: i64,
        date: NaiveDate,
        method: &str,
        ref_text: Option<&str>,
    ) -> Result<Uuid, ServiceError> {
        // 1. Insert the payment record row.
        let payment_id: (Uuid,) = sqlx::query_as(
            r#"
            INSERT INTO payment_records (invoice_id, amount_cents, paid_at, method, reference)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id
            "#,
        )
        .bind(invoice_id)
        .bind(amount_cents)
        .bind(date)
        .bind(method)
        .bind(ref_text)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        // 2. Check whether cumulative payments cover the invoice total.
        //    `invoices` has no price column — the total is derived from the invoice's
        //    own stored fields, exactly as the register derives it, so an Anzahlung is
        //    settled at its own 30% and not at the whole job. The previous version
        //    joined a non-existent `invoices.price_cents`, so this statement always
        //    failed *after* the payment row was written: the payment was stored and the
        //    tool reported an error.
        let (total_paid,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(amount_cents), 0)::bigint FROM payment_records WHERE invoice_id = $1",
        )
        .bind(invoice_id)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let invoice_total = match invoice_repo::fetch_by_id(&self.pool, invoice_id)
            .await
            .map_err(super::map_sqlx)?
        {
            Some(row) => {
                let offer_netto = invoice_repo::fetch_offer_netto(&self.pool, row.inquiry_id)
                    .await
                    .map_err(super::map_sqlx)?;
                crate::routes::invoices::compute_invoice_amounts(
                    crate::routes::invoices::InvoiceAmountInput {
                        invoice_type: &row.invoice_type,
                        partial_percent: row.partial_percent,
                        deposit_percent: row.deposit_percent,
                        is_manual: row.is_manual,
                        extra_services: &row.extra_services,
                        line_items_json: row.line_items_json.as_ref(),
                        base_netto_cents: row.base_netto_cents,
                        offer_netto_cents: offer_netto,
                    },
                )
                .total_brutto_cents
            }
            None => 0,
        };

        // 3. If fully paid, mark the invoice — status and `paid_at` together. Stamping
        //    only the status left the register with a paid row and an empty Bezahlt-Datum,
        //    and the dunning ladder reads that timestamp.
        if invoice_total > 0 && total_paid >= invoice_total {
            sqlx::query("UPDATE invoices SET status = 'paid', paid_at = COALESCE(paid_at, $2) WHERE id = $1")
                .bind(invoice_id)
                .bind(Utc::now())
                .execute(&self.pool)
                .await
                .map_err(super::map_sqlx)?;
        }

        Ok(payment_id.0)
    }
}
