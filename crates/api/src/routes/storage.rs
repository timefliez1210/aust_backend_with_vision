//! Storage-rental ("Lagerung") admin routes — mounted at `/api/v1/admin/storage`.
//!
//! Manages storage contracts and their auto-generated monthly invoices. Prices are
//! entered/returned in BRUTTO cents (Alex thinks brutto); the netto stored in the
//! DB is derived here at the boundary. Approval/send is delegated to
//! [`crate::services::storage_billing_service`], the same funnel the Telegram inline
//! button uses.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use chrono::Datelike;

use crate::repositories::{customer_repo, storage_repo};
use crate::repositories::storage_repo::{StorageContractListRow, StorageContractRow, StorageInvoiceListRow};
use crate::services::storage_billing_service;
use crate::{ApiError, AppState};

/// VAT rate applied to convert the brutto price Alex enters into stored netto.
const MWST: f64 = 1.19;

fn brutto_to_netto_cents(brutto: i64) -> i64 {
    (brutto as f64 / MWST).round() as i64
}
fn netto_to_brutto_cents(netto: i64) -> i64 {
    (netto as f64 * MWST).round() as i64
}

/// Billing day derived from the contract start, clamped to 28 so it is valid in
/// every month (matches the DB CHECK).
fn billing_day_for(start: NaiveDate) -> i16 {
    (start.day() as i16).min(28)
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/contracts", get(list_contracts).post(create_contract))
        .route("/contracts/{id}", axum::routing::patch(update_contract).delete(delete_contract))
        .route("/contracts/{id}/generate-now", post(generate_now))
        .route("/invoices", get(list_invoices))
        .route("/invoices/{id}/pdf", get(download_invoice_pdf))
        .route("/invoices/{id}/approve", post(approve_invoice))
        .route("/invoices/{id}/reject", post(reject_invoice))
}

// ── Contracts ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ContractRequest {
    customer_id: Uuid,
    #[serde(default)]
    billing_address_id: Option<Uuid>,
    contract_start: NaiveDate,
    #[serde(default)]
    contract_end: Option<NaiveDate>,
    sqm: f64,
    /// Monthly price in BRUTTO cents (incl. 19% MwSt).
    monthly_brutto_cents: i64,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct ContractResponse {
    id: Uuid,
    customer_id: Uuid,
    customer_name: Option<String>,
    billing_address_id: Option<Uuid>,
    contract_start: NaiveDate,
    contract_end: Option<NaiveDate>,
    sqm: f64,
    monthly_netto_cents: i64,
    monthly_brutto_cents: i64,
    billing_day: i16,
    status: String,
    note: Option<String>,
}

impl ContractResponse {
    fn from_row(r: StorageContractRow, customer_name: Option<String>) -> Self {
        Self {
            id: r.id,
            customer_id: r.customer_id,
            customer_name,
            billing_address_id: r.billing_address_id,
            contract_start: r.contract_start,
            contract_end: r.contract_end,
            sqm: r.sqm,
            monthly_netto_cents: r.monthly_netto_cents,
            monthly_brutto_cents: netto_to_brutto_cents(r.monthly_netto_cents),
            billing_day: r.billing_day,
            status: r.status,
            note: r.note,
        }
    }

    fn from_list_row(r: StorageContractListRow) -> Self {
        Self {
            id: r.id,
            customer_id: r.customer_id,
            customer_name: r.customer_name,
            billing_address_id: None,
            contract_start: r.contract_start,
            contract_end: r.contract_end,
            sqm: r.sqm,
            monthly_netto_cents: r.monthly_netto_cents,
            monthly_brutto_cents: netto_to_brutto_cents(r.monthly_netto_cents),
            billing_day: r.billing_day,
            status: r.status,
            note: r.note,
        }
    }
}

/// Validate common contract-request invariants shared by create + update.
fn validate_contract(req: &ContractRequest) -> Result<(), ApiError> {
    if req.sqm <= 0.0 {
        return Err(ApiError::Validation("Fläche (m²) muss größer als 0 sein".into()));
    }
    if req.monthly_brutto_cents <= 0 {
        return Err(ApiError::Validation("Monatspreis muss größer als 0 sein".into()));
    }
    if let Some(end) = req.contract_end
        && end < req.contract_start
    {
        return Err(ApiError::Validation("Vertragsende liegt vor dem Vertragsbeginn".into()));
    }
    Ok(())
}

async fn list_contracts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ContractResponse>>, ApiError> {
    let rows = storage_repo::list_contracts(&state.db).await?;
    Ok(Json(rows.into_iter().map(ContractResponse::from_list_row).collect()))
}

async fn create_contract(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ContractRequest>,
) -> Result<Json<ContractResponse>, ApiError> {
    validate_contract(&req)?;
    // Validate customer exists + capture display name for the response.
    let customer = customer_repo::fetch_by_id(&state.db, req.customer_id).await?;

    let row = storage_repo::insert_contract(
        &state.db,
        req.customer_id,
        req.billing_address_id,
        req.contract_start,
        req.contract_end,
        req.sqm,
        brutto_to_netto_cents(req.monthly_brutto_cents),
        billing_day_for(req.contract_start),
        req.note.as_deref(),
    )
    .await?;
    Ok(Json(ContractResponse::from_row(row, Some(customer.display_name()))))
}

async fn update_contract(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ContractRequest>,
) -> Result<Json<ContractResponse>, ApiError> {
    validate_contract(&req)?;
    let status = req.status.clone().unwrap_or_else(|| "active".into());
    if !["active", "ended", "cancelled"].contains(&status.as_str()) {
        return Err(ApiError::Validation(format!("Ungültiger Status: {status}")));
    }
    let customer = customer_repo::fetch_by_id(&state.db, req.customer_id).await?;

    let row = storage_repo::update_contract(
        &state.db,
        id,
        req.billing_address_id,
        req.contract_start,
        req.contract_end,
        req.sqm,
        brutto_to_netto_cents(req.monthly_brutto_cents),
        billing_day_for(req.contract_start),
        &status,
        req.note.as_deref(),
    )
    .await?;
    Ok(Json(ContractResponse::from_row(row, Some(customer.display_name()))))
}

async fn delete_contract(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    match storage_repo::delete_contract(&state.db, id).await {
        Ok(0) => Err(ApiError::NotFound(format!("Storage contract {id} not found"))),
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        // FK RESTRICT: invoices reference this contract.
        Err(e) if e.as_database_error().map(|d| d.is_foreign_key_violation()).unwrap_or(false) => {
            Err(ApiError::BadRequest(
                "Vertrag hat bereits Rechnungen und kann nicht gelöscht werden — bitte stattdessen beenden".into(),
            ))
        }
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

async fn generate_now(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let created =
        storage_billing_service::generate_for_contract(&state.db, state.storage.as_ref(), &state.config, id).await?;
    Ok(Json(serde_json::json!({
        "created": created.is_some(),
        "invoice_id": created,
    })))
}

// ── Invoices ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct InvoiceQuery {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct InvoiceResponse {
    id: Uuid,
    contract_id: Uuid,
    invoice_number: String,
    period_year: i32,
    period_month: i32,
    period_label: String,
    netto_cents: i64,
    brutto_cents: i64,
    status: String,
    payment_method: Option<String>,
    customer_name: Option<String>,
    sqm: f64,
    has_pdf: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl InvoiceResponse {
    fn from_list_row(r: StorageInvoiceListRow) -> Self {
        Self {
            id: r.id,
            contract_id: r.contract_id,
            invoice_number: r.invoice_number,
            period_year: r.period_year,
            period_month: r.period_month,
            period_label: format!("{} {}", german_month(r.period_month as u32), r.period_year),
            netto_cents: r.netto_cents,
            brutto_cents: netto_to_brutto_cents(r.netto_cents),
            status: r.status,
            payment_method: r.payment_method,
            customer_name: r.customer_name,
            sqm: r.sqm,
            has_pdf: r.pdf_s3_key.is_some(),
            created_at: r.created_at,
        }
    }
}

async fn list_invoices(
    State(state): State<Arc<AppState>>,
    Query(q): Query<InvoiceQuery>,
) -> Result<Json<Vec<InvoiceResponse>>, ApiError> {
    let rows = storage_repo::list_invoices(&state.db, q.status.as_deref()).await?;
    Ok(Json(rows.into_iter().map(InvoiceResponse::from_list_row).collect()))
}

async fn download_invoice_pdf(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let invoice = storage_repo::fetch_invoice(&state.db, id).await?;
    let key = invoice
        .pdf_s3_key
        .ok_or_else(|| ApiError::NotFound("Rechnungs-PDF noch nicht erzeugt".into()))?;
    let bytes = state.storage.download(&key).await.map_err(|e| match e {
        aust_storage::StorageError::NotFound(_) => ApiError::NotFound("Rechnung-PDF nicht gefunden.".into()),
        other => ApiError::Internal(format!("Failed to download storage invoice PDF: {other}")),
    })?;

    let (content_type, filename) = if key.ends_with(".pdf") {
        ("application/pdf", format!("Rechnung_{}.pdf", invoice.invoice_number))
    } else {
        (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            format!("Rechnung_{}.xlsx", invoice.invoice_number),
        )
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .body(axum::body::Body::from(bytes))
        .map_err(|e| ApiError::Internal(format!("Response build error: {e}")))
}

async fn approve_invoice(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    storage_billing_service::approve_and_send(&state.db, state.storage.as_ref(), &state.config, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reject_invoice(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    storage_billing_service::reject(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
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
