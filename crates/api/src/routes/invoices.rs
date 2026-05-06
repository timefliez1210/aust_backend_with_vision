//! Invoice (Rechnung) routes — creation, PDF download, status updates, and email dispatch.
//!
//! All routes are nested under `/api/v1/inquiries/{id}/invoices`.
//!
//! Two invoice modes:
//! - **Full**: single invoice for the complete job amount + any on-site extras.
//! - **Partial**: two linked invoices — `partial_first` (Anzahlung, sendable immediately)
//!   and `partial_final` (Restbetrag + extras, sendable after inquiry = `completed`).

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::repositories::{address_repo, invoice_repo, CustomerRow};
use crate::ApiError;
use crate::AppState;
use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_invoice_xlsx, InvoiceData, InvoiceLineItem, InvoiceType,
    OfferLineItem,
};

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Sub-router for all invoice routes nested under `/inquiries`.
///
/// **Caller**: `routes/inquiries.rs::router()` via `.merge()`
/// **Why**: Keeps invoice logic separate from inquiry CRUD while sharing the
/// `/{id}` path segment.
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/{id}/invoices",
            get(list_invoices).post(create_invoice),
        )
        .route(
            "/{id}/invoices/{inv_id}",
            get(get_invoice).patch(update_invoice),
        )
        .route("/{id}/invoices/{inv_id}/pdf", get(get_invoice_pdf))
        .route("/{id}/invoices/{inv_id}/send", post(send_invoice))
}

// Row types re-imported from invoice_repo
use invoice_repo::{ActiveOfferRow, InvoiceRow};

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Request body for `POST /inquiries/{id}/invoices`.
#[derive(Debug, Deserialize)]
pub struct CreateInvoiceRequest {
    /// `"full"` or `"partial"`.
    pub invoice_type: String,
    /// Required when `invoice_type = "partial"`. Range: 1–99.
    pub partial_percent: Option<u8>,
    /// Manual netto amount in cents. Used when no active offer exists for the inquiry.
    /// Ignored when an offer is present — the offer price always takes precedence.
    pub price_cents_netto: Option<i64>,
}

/// Optional request body for `POST /inquiries/{id}/invoices/{inv_id}/send`.
///
/// All fields are optional — if omitted the server falls back to the default template.
#[derive(Debug, Deserialize, Default)]
pub struct SendInvoiceRequest {
    /// Custom email subject. Falls back to "Ihre Rechnung Nr. {n} — Aust Umzüge…".
    pub subject: Option<String>,
    /// Custom email body. Falls back to the standard payment-request template.
    pub body: Option<String>,
}

/// Request body for `PATCH /inquiries/{id}/invoices/{inv_id}`.
#[derive(Debug, Deserialize)]
pub struct UpdateInvoiceRequest {
    /// Set to `"paid"` to mark the invoice as paid (sets `paid_at`).
    pub status: Option<String>,
    /// Replace the extra services list (only allowed on `full` and `partial_final`).
    pub extra_services: Option<Vec<ExtraServiceRequest>>,
}

/// A single extra service as provided by the API caller.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExtraServiceRequest {
    pub description: String,
    /// Netto price in cents.
    pub price_cents: i64,
}

/// Full invoice representation returned by all read endpoints.
#[derive(Debug, Serialize)]
pub struct InvoiceResponse {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub invoice_number: String,
    pub invoice_type: String,
    pub partial_group_id: Option<Uuid>,
    pub partial_percent: Option<i32>,
    pub status: String,
    pub extra_services: Vec<ExtraServiceRequest>,
    /// Netto total (base + extras) in cents, for display.
    pub total_netto_cents: i64,
    /// Brutto total (netto × 1.19) in cents, for display.
    pub total_brutto_cents: i64,
    pub pdf_s3_key: Option<String>,
    pub sent_at: Option<chrono::DateTime<Utc>>,
    pub paid_at: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/inquiries/{id}/invoices` — List all invoices for an inquiry.
///
/// **Caller**: Admin dashboard Rechnungen card
/// **Why**: Shows all invoices (full or partial pair) for the given inquiry.
///
/// # Returns
/// Array of `InvoiceResponse` ordered by creation date.
async fn list_invoices(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<Vec<InvoiceResponse>>, ApiError> {
    let rows = invoice_repo::list_by_inquiry(&state.db, inquiry_id).await?;

    // Need the active offer to compute display amounts
    let offer = invoice_repo::fetch_active_offer(&state.db, inquiry_id).await?;
    let offer_netto = offer.map(|o| o.price_cents).unwrap_or(0);

    let responses: Vec<InvoiceResponse> = rows
        .into_iter()
        .map(|row| build_invoice_response(row, offer_netto))
        .collect();

    Ok(Json(responses))
}

/// `POST /api/v1/inquiries/{id}/invoices` — Create a new invoice or partial pair.
///
/// **Caller**: Admin dashboard "Rechnung Erstellen" / "Partielle Rechnung Erstellen"
/// **Why**: Triggers XLSX + PDF generation and S3 upload for one or two invoices.
///
/// For `partial`, two invoices are created atomically sharing a `partial_group_id`:
/// - `partial_first` → status `ready` (sendable immediately)
/// - `partial_final` → status `draft` (sendable after inquiry = `completed`)
///
/// # Errors
/// - 400 if inquiry status is not ≥ `accepted`
/// - 400 if `invoice_type = "partial"` and `partial_percent` is missing or out of range
/// - 404 if inquiry or active offer not found
async fn create_invoice(
    State(state): State<Arc<AppState>>,
    Path(inquiry_id): Path<Uuid>,
    Json(req): Json<CreateInvoiceRequest>,
) -> Result<Json<Vec<InvoiceResponse>>, ApiError> {
    // Validate invoice type
    let is_partial = match req.invoice_type.as_str() {
        "full" => false,
        "partial" => true,
        other => {
            return Err(ApiError::BadRequest(format!(
                "invoice_type must be 'full' or 'partial', got '{other}'"
            )));
        }
    };

    let percent = if is_partial {
        let p = req.partial_percent.ok_or_else(|| {
            ApiError::BadRequest("partial_percent is required for partial invoices".into())
        })?;
        if p == 0 || p >= 100 {
            return Err(ApiError::BadRequest(
                "partial_percent must be between 1 and 99".into(),
            ));
        }
        p
    } else {
        0
    };

    // Load inquiry and validate status
    let status_str = invoice_repo::fetch_inquiry_status(&state.db, inquiry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Inquiry {inquiry_id} not found")))?;

    let status = status_str.as_str();
    let allowed = matches!(
        status,
        "accepted" | "scheduled" | "completed" | "invoiced" | "paid"
    );
    if !allowed {
        return Err(ApiError::BadRequest(format!(
            "Invoices can only be created for accepted or later inquiries (current status: {status})"
        )));
    }

    // Idempotent: if invoice(s) already exist for this inquiry, return them so
    // the caller can continue the send flow instead of being blocked by a 400.
    // Self-heal any row whose PDF is missing in storage (e.g. restored-from-backup
    // DBs whose object store lacks the corresponding files) — otherwise the
    // caller would be deadlocked: create returns the existing row, download 404s.
    let existing_rows = invoice_repo::list_by_inquiry(&state.db, inquiry_id).await?;
    if !existing_rows.is_empty() {
        for row in &existing_rows {
            ensure_invoice_pdf(&state, row).await?;
        }
        // Re-fetch to pick up any updated pdf_s3_key values.
        let refreshed = invoice_repo::list_by_inquiry(&state.db, inquiry_id).await?;
        let offer_netto = invoice_repo::fetch_active_offer(&state.db, inquiry_id)
            .await?
            .map(|o| o.price_cents)
            .unwrap_or(0);
        let responses: Vec<InvoiceResponse> = refreshed
            .into_iter()
            .map(|row| build_invoice_response(row, offer_netto))
            .collect();
        return Ok(Json(responses));
    }

    // Load data needed for PDF generation
    let invoice_context = load_invoice_context(&state.db, inquiry_id, req.price_cents_netto).await?;
    let offer_netto = invoice_context.offer.price_cents;

    // Guard: offer must have a positive amount
    if offer_netto <= 0 {
        return Err(ApiError::BadRequest(
            "Offer price must be greater than 0 to create invoices".into(),
        ));
    }

    let offer_brutto = (offer_netto as f64 * 1.19).round() as i64;

    let now = Utc::now();
    let today = now.date_naive();

    if is_partial {
        // Create both partial invoices atomically
        let group_id = Uuid::now_v7();

        // Compute amounts — derive final_netto from offer_netto to ensure totals sum exactly
        let first_brutto = (offer_brutto as f64 * percent as f64 / 100.0).round() as i64;
        let first_netto = (first_brutto as f64 / 1.19).round() as i64;
        let final_netto = offer_netto - first_netto; // exact: first + final == offer

        // Generate invoice numbers — single round-trip to avoid sequence gaps on failure
        let (first_num, final_num) = {
            let seqs = invoice_repo::next_invoice_numbers(&state.db, 2).await?;
            (
                format!("{}-{:04}", today.format("%Y"), seqs[0]),
                format!("{}-{:04}", today.format("%Y"), seqs[1]),
            )
        };

        let first_id = Uuid::now_v7();
        let final_id = Uuid::now_v7();

        // Generate PDFs
        // PartialFirst: single "Anzahlung" line item per KVA reference
        let kva_nr = invoice_context.offer.offer_number.as_deref().unwrap_or("");
        let first_line_items = vec![InvoiceLineItem {
            pos: 1,
            description: format!(
                "Anzahlung {percent}% gemäß Kostenvoranschlag Nr. {kva_nr}"
            ),
            quantity: 1.0,
            unit_price: first_netto as f64 / 100.0,
            remark: None,
        }];
        let first_data = build_invoice_data_from_items(
            &invoice_context,
            InvoiceType::PartialFirst { percent },
            &first_num,
            today,
            first_line_items,
        );
        let first_xlsx = generate_invoice_xlsx(&first_data)
            .map_err(|e| ApiError::Internal(format!("Invoice XLSX error: {e}")))?;
        let first_pdf = generate_pdf_bytes(&first_xlsx).await;

        // PartialFinal: KVA line items + "Abzgl. Anzahlung" deduction line
        let final_line_items = build_final_line_items(
            &state.db,
            &invoice_context,
            first_netto,
            &first_num,
            None, // no extras at creation time
        ).await?;
        let final_data = build_invoice_data_from_items(
            &invoice_context,
            InvoiceType::PartialFinal,
            &final_num,
            today,
            final_line_items,
        );
        let final_xlsx = generate_invoice_xlsx(&final_data)
            .map_err(|e| ApiError::Internal(format!("Invoice XLSX error: {e}")))?;
        let final_pdf = generate_pdf_bytes(&final_xlsx).await;

        // Upload PDFs to S3
        let first_key = upload_invoice_pdf(
            &*state.storage,
            first_id,
            &first_pdf,
        )
        .await?;
        let final_key = upload_invoice_pdf(
            &*state.storage,
            final_id,
            &final_pdf,
        )
        .await?;

        // Insert both rows atomically so a partial failure can't leave an orphaned first row
        let mut tx = state.db.begin().await?;
        invoice_repo::insert_partial_first(
            &mut tx, first_id, inquiry_id, &first_num, group_id, percent as i32, &first_key, now,
        ).await?;
        invoice_repo::insert_partial_final(
            &mut tx, final_id, inquiry_id, &final_num, group_id, percent as i32, first_id, &final_key, now,
        ).await?;
        tx.commit().await?;

        let first_row = fetch_invoice_row(&state.db, first_id).await?;
        let final_row = fetch_invoice_row(&state.db, final_id).await?;

        Ok(Json(vec![
            build_invoice_response(first_row, offer_netto),
            build_invoice_response(final_row, offer_netto),
        ]))
    } else {
        // Single full invoice
        let seqs = invoice_repo::next_invoice_numbers(&state.db, 1).await?;
        let invoice_num = format!("{}-{:04}", today.format("%Y"), seqs[0]);
        let inv_id = Uuid::now_v7();

        // Full invoice: KVA line items, falling back to a lump-sum if none stored
        let kva_nr = invoice_context.offer.offer_number.as_deref().unwrap_or("");
        let kva_items = kva_line_items_from_offer(&invoice_context, kva_nr);
        let full_line_items = if kva_items.is_empty() {
            vec![InvoiceLineItem {
                pos: 1,
                description: format!("Umzugsdienstleistung gemäß Angebot Nr. {kva_nr}"),
                quantity: 1.0,
                unit_price: offer_netto as f64 / 100.0,
                remark: None,
            }]
        } else {
            kva_items
        };
        let data = build_invoice_data_from_items(
            &invoice_context,
            InvoiceType::Full,
            &invoice_num,
            today,
            full_line_items,
        );
        let xlsx = generate_invoice_xlsx(&data)
            .map_err(|e| ApiError::Internal(format!("Invoice XLSX error: {e}")))?;
        let pdf = generate_pdf_bytes(&xlsx).await;
        let s3_key = upload_invoice_pdf(&*state.storage, inv_id, &pdf).await?;

        invoice_repo::insert_full(&state.db, inv_id, inquiry_id, &invoice_num, &s3_key, now).await?;

        let row = fetch_invoice_row(&state.db, inv_id).await?;
        Ok(Json(vec![build_invoice_response(row, offer_netto)]))
    }
}

/// `GET /api/v1/inquiries/{id}/invoices/{inv_id}` — Get a single invoice.
async fn get_invoice(
    State(state): State<Arc<AppState>>,
    Path((inquiry_id, inv_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    let row = invoice_repo::fetch_by_id_and_inquiry(&state.db, inv_id, inquiry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Invoice {inv_id} not found")))?;

    let offer_netto = get_offer_netto(&state.db, inquiry_id).await?;
    Ok(Json(build_invoice_response(row, offer_netto)))
}

/// `GET /api/v1/inquiries/{id}/invoices/{inv_id}/pdf` — Download invoice PDF.
///
/// **Caller**: Admin dashboard PDF download button
/// **Why**: Streams the stored PDF (or XLSX fallback) from S3.
async fn get_invoice_pdf(
    State(state): State<Arc<AppState>>,
    Path((inquiry_id, inv_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let (key_opt, invoice_number) = invoice_repo::fetch_pdf_key(&state.db, inv_id, inquiry_id)
        .await?
        .ok_or_else(|| {
            tracing::warn!(%inv_id, %inquiry_id, "Invoice row not found");
            ApiError::NotFound(format!("Invoice {inv_id} not found"))
        })?;
    let key = key_opt.ok_or_else(|| {
        tracing::warn!(%inv_id, "Invoice PDF key is NULL");
        ApiError::NotFound("Invoice PDF not yet generated".into())
    })?;

    let bytes = state
        .storage
        .download(&key)
        .await
        .map_err(|e| match e {
            aust_storage::StorageError::NotFound(k) => {
                tracing::warn!(key = %k, "Storage NotFound for invoice PDF");
                ApiError::NotFound("Rechnung-PDF nicht gefunden.".into())
            }
            other => {
                tracing::error!(error = %other, %key, "Storage download failed");
                ApiError::Internal(format!("Failed to download invoice PDF: {other}"))
            }
        })?;

    let (content_type, filename) = if key.ends_with(".pdf") {
        (
            "application/pdf",
            format!("Rechnung_{invoice_number}.pdf"),
        )
    } else {
        (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            format!("Rechnung_{invoice_number}.xlsx"),
        )
    };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(bytes::Bytes::from(bytes)))
        .map_err(|e| ApiError::Internal(format!("Response build error: {e}")))?;

    Ok(response)
}

/// `PATCH /api/v1/inquiries/{id}/invoices/{inv_id}` — Update invoice status or extra services.
///
/// **Caller**: Admin dashboard — "Als bezahlt markieren" button, or extra services editor
/// **Why**: Two update paths:
/// 1. `{ status: "paid" }` → sets `paid_at`, updates status.
///    When all invoices for an inquiry are paid, auto-transitions inquiry to `paid`.
/// 2. `{ extra_services: [...] }` → replaces the extra services list and regenerates the PDF.
///    Only allowed on `full` and `partial_final` invoice types.
///
/// # Errors
/// - 400 if trying to set `extra_services` on a `partial_first` invoice
/// - 404 if invoice not found
async fn update_invoice(
    State(state): State<Arc<AppState>>,
    Path((inquiry_id, inv_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateInvoiceRequest>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    // Load existing invoice
    let row = fetch_invoice_by_inquiry(&state.db, inv_id, inquiry_id).await?;

    let now = Utc::now();

    // Handle status update
    if let Some(ref new_status) = req.status {
        if new_status == "paid" {
            invoice_repo::mark_paid(&state.db, inv_id, now).await?;

            // Auto-transition inquiry to 'paid' when all invoices are paid
            let unpaid = invoice_repo::count_unpaid(&state.db, inquiry_id).await?;
            if unpaid == 0 {
                invoice_repo::transition_inquiry_to_paid(&state.db, inquiry_id, now).await?;
            }
        }
    }

    // Handle extra services update + PDF regeneration
    if let Some(ref extras) = req.extra_services {
        if row.invoice_type == "partial_first" {
            return Err(ApiError::BadRequest(
                "Extra services can only be added to full or partial_final invoices".into(),
            ));
        }

        // Validate each extra service — price_cents may be negative (Gutschrift)
        for (i, extra) in extras.iter().enumerate() {
            if extra.price_cents == 0 {
                return Err(ApiError::BadRequest(format!(
                    "extra_services[{i}].price_cents must not be zero"
                )));
            }
            if extra.description.trim().is_empty() {
                return Err(ApiError::BadRequest(format!(
                    "extra_services[{i}].description must not be empty"
                )));
            }
        }

        let extra_services_json = serde_json::to_value(extras)
            .map_err(|e| ApiError::Internal(format!("JSON error: {e}")))?;

        // Persist extra services
        invoice_repo::update_extra_services(&state.db, inv_id, &extra_services_json).await?;

        // Regenerate PDF with updated extras
        let invoice_context = load_invoice_context(&state.db, inquiry_id, None).await?;
        let today = now.date_naive();

        let inv_type = match row.invoice_type.as_str() {
            "partial_final" => InvoiceType::PartialFinal,
            _ => InvoiceType::Full,
        };

        let regen_items = match inv_type {
            InvoiceType::PartialFinal => {
                // Resolve deposit netto (first_netto) from partial_percent
                let offer_netto = invoice_context.offer.price_cents;
                let offer_brutto = (offer_netto as f64 * 1.19).round() as i64;
                let pct = row.partial_percent.unwrap_or_else(|| {
                    // fall back to sibling lookup (pre-migration rows)
                    row.deposit_percent.map(|p| p as i32).unwrap_or(0)
                });
                let first_brutto = (offer_brutto as f64 * pct as f64 / 100.0).round() as i64;
                let first_netto = (first_brutto as f64 / 1.19).round() as i64;

                // Resolve deposit invoice number
                let deposit_number = resolve_deposit_number(&state.db, &row).await;

                build_final_line_items(
                    &state.db,
                    &invoice_context,
                    first_netto,
                    &deposit_number,
                    Some(extras),
                ).await?
            }
            _ => {
                // Full invoice: base line + extras
                let offer_netto = invoice_context.offer.price_cents;
                let kva_nr = invoice_context.offer.offer_number.as_deref().unwrap_or("");
                let kva_items = kva_line_items_from_offer(&invoice_context, kva_nr);
                let base_items = if kva_items.is_empty() {
                    vec![InvoiceLineItem {
                        pos: 1,
                        description: format!(
                            "Umzugsdienstleistung gemäß Angebot Nr. {kva_nr}"
                        ),
                        quantity: 1.0,
                        unit_price: offer_netto as f64 / 100.0,
                        remark: None,
                    }]
                } else {
                    kva_items
                };
                let extra_offset = base_items.len() as u32 + 1;
                let extra_items: Vec<InvoiceLineItem> = extras
                    .iter()
                    .enumerate()
                    .map(|(i, e)| InvoiceLineItem {
                        pos: extra_offset + i as u32,
                        description: e.description.clone(),
                        quantity: 1.0,
                        unit_price: e.price_cents as f64 / 100.0,
                        remark: None,
                    })
                    .collect();
                let mut items = base_items;
                items.extend(extra_items);
                items
            }
        };

        let data = build_invoice_data_from_items(
            &invoice_context,
            inv_type,
            &row.invoice_number,
            today,
            regen_items,
        );

        let xlsx = generate_invoice_xlsx(&data)
            .map_err(|e| ApiError::Internal(format!("Invoice XLSX error: {e}")))?;
        let pdf = generate_pdf_bytes(&xlsx).await;
        let new_key = upload_invoice_pdf(&*state.storage, inv_id, &pdf).await?;

        invoice_repo::update_pdf_key(&state.db, inv_id, &new_key).await?;
    }

    let updated_row = fetch_invoice_row(&state.db, inv_id).await?;
    let offer_netto = get_offer_netto(&state.db, inquiry_id).await?;
    Ok(Json(build_invoice_response(updated_row, offer_netto)))
}

/// `POST /api/v1/inquiries/{id}/invoices/{inv_id}/send` — Send invoice by email.
///
/// **Caller**: Admin dashboard "Senden" button
/// **Why**: Attaches the invoice PDF and sends it to the customer via SMTP.
///
/// Sendability rules:
/// - `partial_first`: always sendable if status = `ready` or `draft`
/// - `full` / `partial_final`: require inquiry status = `completed`
///
/// On success: sets `status = 'sent'`, `sent_at = now()`.
/// Auto-transitions inquiry to `invoiced` if not already past that stage.
///
/// # Errors
/// - 400 if the sendability gate is not met
/// - 404 if invoice PDF is not yet generated
async fn send_invoice(
    State(state): State<Arc<AppState>>,
    Path((inquiry_id, inv_id)): Path<(Uuid, Uuid)>,
    body: Option<Json<SendInvoiceRequest>>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let row = fetch_invoice_by_inquiry(&state.db, inv_id, inquiry_id).await?;

    // Sendability gate
    if row.invoice_type == "partial_first" {
        if row.status == "draft" {
            return Err(ApiError::BadRequest(
                "Diese Anzahlungsrechnung ist noch im Entwurfsstatus und kann nicht gesendet werden".into(),
            ));
        }
    } else {
        let inq_status = invoice_repo::fetch_inquiry_status(&state.db, inquiry_id)
            .await?
            .unwrap_or_default();
        if inq_status != "completed" {
            return Err(ApiError::BadRequest(
                "Diese Rechnung kann erst nach Auftragsabschluss (Status: abgeschlossen) gesendet werden".into(),
            ));
        }
    }

    let pdf_key = row
        .pdf_s3_key
        .clone()
        .ok_or_else(|| ApiError::NotFound("Invoice PDF not yet generated".into()))?;

    // Load PDF bytes from S3
    let pdf_bytes = state
        .storage
        .download(&pdf_key)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to load invoice PDF: {e}")))?;

    // Load customer email
    let (customer_email, customer_name) = invoice_repo::fetch_customer_for_invoice(&state.db, inquiry_id)
        .await
        .map_err(|_| ApiError::NotFound("Customer not found for inquiry".into()))?
        .ok_or_else(|| ApiError::NotFound("Customer not found for inquiry".into()))?;

    // Customer may not have an email address (e.g. elderly walk-in customers)
    let customer_email = customer_email.ok_or_else(|| ApiError::Validation(
        "Kunde hat keine E-Mail-Adresse — Rechnung kann nicht per E-Mail versendet werden".into(),
    ))?;

    let display_name = customer_name.as_deref().unwrap_or("Kunde");
    let invoice_num = &row.invoice_number;
    let subject = req.subject.unwrap_or_else(|| {
        format!("Ihre Rechnung Nr. {invoice_num} — Aust Umzüge & Haushaltsauflösungen")
    });
    let body = req.body.unwrap_or_else(|| {
        format!(
            "Sehr geehrte/r {display_name},\n\n\
             im Anhang finden Sie Ihre Rechnung Nr. {invoice_num}.\n\n\
             Bitte überweisen Sie den Rechnungsbetrag innerhalb von 7 Tagen \
             unter Angabe der Rechnungsnummer auf unser Konto.\n\n\
             Mit freundlichen Grüßen\n\
             Aust Umzüge & Haushaltsauflösungen"
        )
    });

    let filename = format!("Rechnung_{invoice_num}.pdf");
    let email = crate::services::email::build_email_with_attachment(
        &state.config.email.username,
        "Aust Umzüge & Haushaltsauflösungen",
        &customer_email,
        &subject,
        &body,
        &pdf_bytes,
        &filename,
        "application/pdf",
    )
    .map_err(|e| ApiError::Internal(format!("Failed to build invoice email: {e}")))?;

    crate::services::email::send_email(
        &state.config.email.smtp_host,
        state.config.email.smtp_port,
        &state.config.email.username,
        &state.config.email.password,
        email,
    )
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to send invoice email: {e}")))?;

    // Update invoice status
    let now = Utc::now();
    invoice_repo::mark_sent(&state.db, inv_id, now).await?;

    // Auto-transition inquiry to 'invoiced' if still in an earlier stage
    invoice_repo::transition_inquiry_to_invoiced(&state.db, inquiry_id, now).await?;

    // Schedule a payment-reminder dashboard alert for 7 days from now
    let remind_after = now.date_naive() + chrono::Days::new(7);
    let _ = crate::repositories::invoice_reminder_repo::create(&state.db, inv_id, remind_after).await;
    // (non-fatal — invoice sending succeeds even if reminder creation fails)

    let updated_row = fetch_invoice_row(&state.db, inv_id).await?;
    let offer_netto = get_offer_netto(&state.db, inquiry_id).await?;
    Ok(Json(build_invoice_response(updated_row, offer_netto)))
}

// ---------------------------------------------------------------------------
// Invoice data loading helpers
// ---------------------------------------------------------------------------

/// All DB data needed to generate an invoice PDF.
struct InvoiceContext {
    offer: ActiveOfferRow,
    customer: CustomerRow,
    billing_street: String,
    billing_city: String,
    moving_date: Option<chrono::NaiveDate>,
}

/// Load all data needed for invoice generation from the database.
///
/// **Why**: Centralised data loading used by both create and update (PDF regeneration) paths.
///
/// `price_fallback_netto` is used when no active offer exists (e.g. direct Termin invoicing).
/// When an offer exists the offer price always takes precedence over the fallback.
///
/// # Errors
/// Returns `NotFound` if there is no active offer and no fallback price was provided.
async fn load_invoice_context(
    db: &sqlx::PgPool,
    inquiry_id: Uuid,
    price_fallback_netto: Option<i64>,
) -> Result<InvoiceContext, ApiError> {
    // Active offer (most recent); fall back to manual price when absent
    let offer: ActiveOfferRow = match invoice_repo::fetch_active_offer(db, inquiry_id).await? {
        Some(o) => o,
        None => {
            let cents = price_fallback_netto.ok_or_else(|| {
                ApiError::NotFound(
                    "Kein Angebot vorhanden — bitte erst ein Angebot erstellen oder einen Betrag angeben".into(),
                )
            })?;
            ActiveOfferRow { price_cents: cents, offer_number: None, line_items_json: None, persons: None }
        }
    };

    // Customer + moving date
    let customer: CustomerRow =
        crate::repositories::customer_repo::fetch_by_inquiry_id(db, inquiry_id).await?;

    let moving_date = invoice_repo::fetch_moving_date(db, inquiry_id).await?;

    // Resolve billing address: explicit > destination (post-move) > origin (pre-move)
    let billing_addr_id = invoice_repo::resolve_billing_address_id(db, inquiry_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let billing = address_repo::fetch_optional(db, billing_addr_id).await?;

    let billing_street = billing
        .as_ref()
        .map(|a| {
            match a.house_number.as_deref() {
                Some(hn) if !hn.is_empty() => format!("{} {}", a.street, hn),
                _ => a.street.clone(),
            }
        })
        .unwrap_or_default();
    let billing_city = billing
        .as_ref()
        .map(|a| {
            let postal = a.postal_code.as_deref().unwrap_or("");
            let city = a.city.as_str();
            if postal.is_empty() { city.to_string() } else { format!("{postal} {city}") }
        })
        .unwrap_or_default();

    Ok(InvoiceContext {
        offer,
        customer,
        billing_street,
        billing_city,
        moving_date,
    })
}

/// Build an `InvoiceData` struct from a loaded `InvoiceContext` and line items.
///
/// **Why**: Centralises the conversion from domain objects to the XLSX generator's
/// input type. The `line_items` field is the single source of truth; legacy fields
/// (`base_netto_cents`, `extra_services`, `origin_street`, `origin_city`) are filled
/// with empty/zero defaults for backward compatibility.
fn build_invoice_data_from_items(
    ctx: &InvoiceContext,
    invoice_type: InvoiceType,
    invoice_number: &str,
    invoice_date: chrono::NaiveDate,
    line_items: Vec<InvoiceLineItem>,
) -> InvoiceData {
    let customer_name = match (ctx.customer.first_name.as_deref(), ctx.customer.last_name.as_deref()) {
        (Some(f), Some(l)) => format!("{f} {l}"),
        _ => ctx.customer.name.clone().unwrap_or_else(|| ctx.customer.email.clone().unwrap_or_else(|| "Kunde".to_string())),
    };

    InvoiceData {
        invoice_number: invoice_number.to_string(),
        invoice_type,
        invoice_date,
        service_date: ctx.moving_date,
        customer_name,
        customer_email: ctx.customer.email.clone(),
        company_name: ctx.customer.company_name.clone(),
        attention_line: Some(ctx.customer.attention_line()).filter(|s| !s.is_empty()),
        billing_street: ctx.billing_street.clone(),
        billing_city: ctx.billing_city.clone(),
        offer_number: ctx.offer.offer_number.clone().unwrap_or_default(),
        salutation: ctx.customer.formal_greeting(),
        line_items,
        // Legacy fields — filled with zeros/empty for backward compat
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

// ---------------------------------------------------------------------------
// Final-invoice line-item helpers
// ---------------------------------------------------------------------------

/// Convert offer `line_items_json` (OfferLineItem array) to InvoiceLineItem vec.
///
/// Returns an empty vec when `line_items_json` is NULL or cannot be parsed —
/// the caller falls back to a single lump-sum line.
fn kva_line_items_from_offer(ctx: &InvoiceContext, _kva_nr: &str) -> Vec<InvoiceLineItem> {
    let json = match ctx.offer.line_items_json.as_ref() {
        Some(j) => j,
        None => return vec![],
    };
    let offer_items: Vec<OfferLineItem> = match serde_json::from_value(json.clone()) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    // Nürnbergerversicherung is included on the KVA for transparency but is a
    // free service — suppress it on the invoice to keep the line list focused
    // on billable positions.
    offer_items
        .into_iter()
        .filter(|item| !item.description.trim().to_lowercase().starts_with("nürnberger"))
        .enumerate()
        .map(|(i, item)| {
            // Compute netto unit price in EUR for the invoice line.
            // OfferLineItem stores unit_price per hour/unit in EUR.
            // For labor items the offer formula is hours × rate × persons (J50);
            // for flat_total items we use the flat value directly; otherwise quantity × unit_price.
            // We flatten to a single invoice line: quantity=1, unit_price=total.
            let persons = ctx.offer.persons.unwrap_or(1).max(1) as f64;
            let unit_price_eur = if let Some(flat) = item.flat_total {
                flat
            } else if item.is_labor {
                item.unit_price * item.quantity * persons
            } else {
                item.unit_price * item.quantity
            };
            InvoiceLineItem {
                pos: (i + 1) as u32,
                description: item.description.clone(),
                quantity: 1.0,
                unit_price: unit_price_eur,
                remark: item.remark.clone(),
            }
        })
        .collect()
}

/// Build the complete line-item list for a PartialFinal (Schlussrechnung).
///
/// Layout:
/// 1..N  KVA items (copied verbatim from offer line_items_json, or a lump-sum fallback)
/// N+1.. extras (Zusatzleistungen / Gutschriften, positive or negative)
/// last  "Abzüglich Anzahlung gemäß Rechnung Nr. {deposit_number}" (negative amount)
///
/// `first_netto` is the Anzahlung amount (netto cents) to deduct.
/// `extras` is `None` at creation time (no extras yet) or `Some(&[…])` on PATCH regen.
async fn build_final_line_items(
    _db: &sqlx::PgPool,
    ctx: &InvoiceContext,
    first_netto: i64,
    deposit_number: &str,
    extras: Option<&[ExtraServiceRequest]>,
) -> Result<Vec<InvoiceLineItem>, ApiError> {
    let kva_nr = ctx.offer.offer_number.as_deref().unwrap_or("");

    // Build KVA base items
    let kva_items = kva_line_items_from_offer(ctx, kva_nr);
    let mut items: Vec<InvoiceLineItem> = if kva_items.is_empty() {
        // Fallback: single lump-sum for the full offer amount
        vec![InvoiceLineItem {
            pos: 1,
            description: format!("Umzugsdienstleistung gemäß Angebot Nr. {kva_nr}"),
            quantity: 1.0,
            unit_price: ctx.offer.price_cents as f64 / 100.0,
            remark: None,
        }]
    } else {
        kva_items
    };

    // Append extras (if any)
    let extra_offset = items.len() as u32 + 1;
    if let Some(ex) = extras {
        for (i, e) in ex.iter().enumerate() {
            items.push(InvoiceLineItem {
                pos: extra_offset + i as u32,
                description: e.description.clone(),
                quantity: 1.0,
                unit_price: e.price_cents as f64 / 100.0,
                remark: None,
            });
        }
    }

    // Append deduction line (negative)
    let deduction_pos = items.len() as u32 + 1;
    items.push(InvoiceLineItem {
        pos: deduction_pos,
        description: format!(
            "Abzüglich Anzahlung gemäß Rechnung Nr. {deposit_number}"
        ),
        quantity: 1.0,
        unit_price: -(first_netto as f64 / 100.0),
        remark: None,
    });

    Ok(items)
}

/// Resolve the deposit (Anzahlung) invoice number for a partial_final row.
///
/// Priority: `deposit_invoice_id` column → `partial_group_id` sibling lookup → empty string.
async fn resolve_deposit_number(db: &sqlx::PgPool, row: &InvoiceRow) -> String {
    if let Some(dep_id) = row.deposit_invoice_id {
        if let Ok(Some(n)) = invoice_repo::fetch_deposit_invoice_number(db, dep_id).await {
            return n;
        }
    }
    if let Some(gid) = row.partial_group_id {
        if let Ok(Some(n)) = invoice_repo::fetch_deposit_number_by_group(db, gid).await {
            return n;
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// DB helper functions
// ---------------------------------------------------------------------------

async fn fetch_invoice_row(db: &sqlx::PgPool, inv_id: Uuid) -> Result<InvoiceRow, ApiError> {
    invoice_repo::fetch_by_id(db, inv_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Invoice {inv_id} not found")))
}

async fn fetch_invoice_by_inquiry(
    db: &sqlx::PgPool,
    inv_id: Uuid,
    inquiry_id: Uuid,
) -> Result<InvoiceRow, ApiError> {
    invoice_repo::fetch_by_id_and_inquiry(db, inv_id, inquiry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Invoice {inv_id} not found")))
}

async fn get_offer_netto(db: &sqlx::PgPool, inquiry_id: Uuid) -> Result<i64, ApiError> {
    invoice_repo::fetch_offer_netto(db, inquiry_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

// ---------------------------------------------------------------------------
// PDF / S3 helpers
// ---------------------------------------------------------------------------

/// Convert XLSX bytes to PDF via LibreOffice, falling back to XLSX on failure.
async fn generate_pdf_bytes(xlsx: &[u8]) -> Vec<u8> {
    match convert_xlsx_to_pdf(xlsx).await {
        Ok(pdf) => pdf,
        Err(e) => {
            tracing::warn!("Invoice PDF conversion unavailable ({e}), using XLSX");
            xlsx.to_vec()
        }
    }
}

/// Regenerate the PDF for an existing invoice row when its stored PDF is
/// missing or unreadable. Updates `pdf_s3_key` in the DB on success.
///
/// **Why**: DB rows restored from backups may point at objects that no longer
/// exist in storage, or a prior generation may have failed after the row was
/// inserted. Without this self-heal, the user is deadlocked — the "create"
/// button returns the existing row (idempotent), the download returns 404,
/// and nothing can progress.
async fn ensure_invoice_pdf(
    state: &AppState,
    row: &InvoiceRow,
) -> Result<(), ApiError> {
    // Fast path: key exists in DB and storage has the object.
    if let Some(key) = row.pdf_s3_key.as_deref() {
        match state.storage.download(key).await {
            Ok(_) => return Ok(()),
            Err(aust_storage::StorageError::NotFound(_)) => {
                tracing::warn!(invoice_id = %row.id, %key, "Invoice PDF missing in storage — regenerating");
            }
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "Storage check failed for invoice {}: {e}", row.id
                )));
            }
        }
    } else {
        tracing::warn!(invoice_id = %row.id, "Invoice row has no pdf_s3_key — regenerating");
    }

    // Load invoice context. No manual price fallback here — we are healing an
    // existing row, the offer price (or its absence) is already reflected in
    // the invoice's stored amounts, and we cannot safely invent a price.
    let ctx = load_invoice_context(&state.db, row.inquiry_id, None).await?;
    let today = Utc::now().date_naive();

    // Parse existing extras to preserve them across regeneration.
    let extras: Vec<ExtraServiceRequest> = row
        .extra_services
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default();

    let (inv_type, items) = match row.invoice_type.as_str() {
        "partial_first" => {
            let pct = row.partial_percent.unwrap_or(0);
            let offer_brutto = (ctx.offer.price_cents as f64 * 1.19).round() as i64;
            let first_brutto = (offer_brutto as f64 * pct as f64 / 100.0).round() as i64;
            let first_netto = (first_brutto as f64 / 1.19).round() as i64;
            let kva_nr = ctx.offer.offer_number.as_deref().unwrap_or("");
            let items = vec![InvoiceLineItem {
                pos: 1,
                description: format!("Anzahlung {pct}% gemäß Kostenvoranschlag Nr. {kva_nr}"),
                quantity: 1.0,
                unit_price: first_netto as f64 / 100.0,
                remark: None,
            }];
            (InvoiceType::PartialFirst { percent: pct as u8 }, items)
        }
        "partial_final" => {
            let offer_brutto = (ctx.offer.price_cents as f64 * 1.19).round() as i64;
            let pct = row.partial_percent
                .or_else(|| row.deposit_percent.map(|p| p as i32))
                .unwrap_or(0);
            let first_brutto = (offer_brutto as f64 * pct as f64 / 100.0).round() as i64;
            let first_netto = (first_brutto as f64 / 1.19).round() as i64;
            let deposit_number = resolve_deposit_number(&state.db, row).await;
            let items = build_final_line_items(
                &state.db,
                &ctx,
                first_netto,
                &deposit_number,
                if extras.is_empty() { None } else { Some(&extras) },
            ).await?;
            (InvoiceType::PartialFinal, items)
        }
        _ => {
            // Full invoice
            let offer_netto = ctx.offer.price_cents;
            let kva_nr = ctx.offer.offer_number.as_deref().unwrap_or("");
            let kva_items = kva_line_items_from_offer(&ctx, kva_nr);
            let base_items = if kva_items.is_empty() {
                vec![InvoiceLineItem {
                    pos: 1,
                    description: format!("Umzugsdienstleistung gemäß Angebot Nr. {kva_nr}"),
                    quantity: 1.0,
                    unit_price: offer_netto as f64 / 100.0,
                    remark: None,
                }]
            } else {
                kva_items
            };
            let extra_offset = base_items.len() as u32 + 1;
            let extra_items: Vec<InvoiceLineItem> = extras
                .iter()
                .enumerate()
                .map(|(i, e)| InvoiceLineItem {
                    pos: extra_offset + i as u32,
                    description: e.description.clone(),
                    quantity: 1.0,
                    unit_price: e.price_cents as f64 / 100.0,
                    remark: None,
                })
                .collect();
            let mut items = base_items;
            items.extend(extra_items);
            (InvoiceType::Full, items)
        }
    };

    let data = build_invoice_data_from_items(&ctx, inv_type, &row.invoice_number, today, items);
    let xlsx = generate_invoice_xlsx(&data)
        .map_err(|e| ApiError::Internal(format!("Invoice XLSX error: {e}")))?;
    let pdf = generate_pdf_bytes(&xlsx).await;
    let new_key = upload_invoice_pdf(&*state.storage, row.id, &pdf).await?;
    invoice_repo::update_pdf_key(&state.db, row.id, &new_key).await?;
    Ok(())
}

/// Upload invoice PDF (or XLSX fallback) to S3 and return the storage key.
async fn upload_invoice_pdf(
    storage: &dyn aust_storage::StorageProvider,
    inv_id: Uuid,
    bytes: &[u8],
) -> Result<String, ApiError> {
    let is_pdf = bytes.starts_with(b"%PDF");
    let (ext, mime) = if is_pdf {
        ("pdf", "application/pdf")
    } else {
        (
            "xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
    };
    let key = format!("invoices/{inv_id}/rechnung.{ext}");
    storage
        .upload(&key, bytes::Bytes::from(bytes.to_vec()), mime)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to upload invoice PDF: {e}")))?;
    Ok(key)
}

// ---------------------------------------------------------------------------
// Response builder
// ---------------------------------------------------------------------------

/// Build an `InvoiceResponse` from a DB row plus the offer netto for amount calculation.
fn build_invoice_response(row: InvoiceRow, offer_netto_cents: i64) -> InvoiceResponse {
    let extra_services: Vec<ExtraServiceRequest> = row
        .extra_services
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let extra_netto: i64 = extra_services.iter().map(|e| e.price_cents).sum();
    let offer_brutto = (offer_netto_cents as f64 * 1.19).round() as i64;

    let (base_netto, total_netto) = match row.invoice_type.as_str() {
        "partial_first" => {
            let pct = row.partial_percent.unwrap_or(0) as f64;
            let first_brutto = (offer_brutto as f64 * pct / 100.0).round() as i64;
            let n = (first_brutto as f64 / 1.19).round() as i64;
            (n, n) // no extras on partial_first
        }
        "partial_final" => {
            // Mirror creation math: first_netto derived from first_brutto, final = offer - first
            let pct = row.partial_percent.unwrap_or(0) as f64;
            let first_brutto = (offer_brutto as f64 * pct / 100.0).round() as i64;
            let first_netto = (first_brutto as f64 / 1.19).round() as i64;
            let n = offer_netto_cents - first_netto;
            (n, n + extra_netto)
        }
        _ => {
            // full
            (offer_netto_cents, offer_netto_cents + extra_netto)
        }
    };

    let _ = base_netto; // used indirectly via total_netto
    let total_brutto = (total_netto as f64 * 1.19).round() as i64;

    InvoiceResponse {
        id: row.id,
        inquiry_id: row.inquiry_id,
        invoice_number: row.invoice_number,
        invoice_type: row.invoice_type,
        partial_group_id: row.partial_group_id,
        partial_percent: row.partial_percent,
        status: row.status,
        extra_services,
        total_netto_cents: total_netto,
        total_brutto_cents: total_brutto,
        pdf_s3_key: row.pdf_s3_key,
        sent_at: row.sent_at,
        paid_at: row.paid_at,
        created_at: row.created_at,
    }
}
