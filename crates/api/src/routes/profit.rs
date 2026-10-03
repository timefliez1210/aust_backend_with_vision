//! Gewinn tab — mounted at `/api/v1/admin/profit` (admin JWT layer, admin role only).
//!
//! Expenses, Daueraufträge, the monthly hours transfer and the margin views. Money
//! comes in as BRUTTO cents plus a VAT rate (Alex thinks brutto); netto/USt are split
//! here at the boundary. All math lives in [`crate::services::profit_service`].

use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use aust_core::models::TokenClaims;

use crate::repositories::accounting_repo::{self, ExpenseFilter, ExpenseInput, HourlyCalcSettings, RecurringInput};
use crate::repositories::settings_repo;
use crate::routes::admin::require_admin;
use crate::services::profit_service::{self, month_start, parse_month};
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/overview", get(overview))
        .route("/jobs", get(jobs))
        .route("/inquiries/{id}", get(inquiry_margin))
        .route("/employees", get(employees))
        .route("/labor/{month}", get(labor_preview))
        .route("/labor/{month}/transfer", post(labor_transfer))
        .route("/hourly-rate", get(hourly_rate))
        .route("/categories", get(list_categories).post(create_category))
        .route("/categories/{id}", axum::routing::patch(update_category))
        .route("/positions", get(position_names))
        .route("/recharge", get(recharge))
        .route("/expenses", get(list_expenses).post(create_expense))
        .route("/expenses/{id}", axum::routing::patch(update_expense).delete(delete_expense))
        .route("/expenses/{id}/storno", post(storno_expense))
        .route("/expenses/{id}/confirm", post(confirm_expense))
        .route("/expenses/{id}/receipt", get(download_receipt).post(upload_receipt))
        .route("/recurring", get(list_recurring).post(create_recurring))
        .route("/recurring/{id}", axum::routing::patch(update_recurring).delete(delete_recurring))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/audit", get(audit))
}

/// Receipts are capped well above a phone photo or a scanned PDF.
const MAX_RECEIPT_BYTES: usize = 15 * 1024 * 1024;

fn actor(claims: &TokenClaims) -> String {
    claims.email.clone()
}

/// Split a brutto amount into (netto, USt) for a German VAT rate.
pub(crate) fn split_brutto(brutto_cents: i64, vat_rate: i16) -> (i64, i64) {
    let netto = (brutto_cents as f64 * 100.0 / (100.0 + vat_rate as f64)).round() as i64;
    (netto, brutto_cents - netto)
}

fn validate_vat(rate: i16) -> Result<(), ApiError> {
    if ![0, 7, 19].contains(&rate) {
        return Err(ApiError::Validation("MwSt-Satz muss 0, 7 oder 19 % sein".into()));
    }
    Ok(())
}

fn clean(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn month_param(s: &str) -> Result<NaiveDate, ApiError> {
    parse_month(s).ok_or_else(|| ApiError::BadRequest("Ungültiges Monatsformat. Erwartet: YYYY-MM".into()))
}

// ── Views ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct YearQuery {
    year: Option<i32>,
}

async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<YearQuery>,
) -> Result<Json<profit_service::Overview>, ApiError> {
    require_admin(&claims)?;
    let year = q.year.unwrap_or_else(|| profit_service::today_berlin().year());
    if !(2000..=2100).contains(&year) {
        return Err(ApiError::BadRequest("Ungültiges Jahr".into()));
    }
    Ok(Json(profit_service::overview(&state.db, year).await?))
}

#[derive(Debug, Deserialize)]
struct MonthQuery {
    month: Option<String>,
}

async fn jobs(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<MonthQuery>,
) -> Result<Json<profit_service::JobsResponse>, ApiError> {
    require_admin(&claims)?;
    let month = match q.month {
        Some(m) => month_param(&m)?,
        None => month_start(profit_service::today_berlin()),
    };
    Ok(Json(profit_service::jobs(&state.db, month).await?))
}

async fn inquiry_margin(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<profit_service::InquiryMargin>, ApiError> {
    require_admin(&claims)?;
    let rate = current_rate(&state).await?;
    Ok(Json(profit_service::inquiry_margin(&state.db, id, rate).await?))
}

/// The KVA rate per person-hour (netto) from Einstellungen → Preise.
async fn current_rate(state: &AppState) -> Result<i64, ApiError> {
    Ok(settings_repo::get_pricing(&state.db, &state.config).await?.rate_per_person_hour_cents)
}

/// `GET /admin/profit/hourly-rate` — the Stundensatz-Kalkulation. `null` until the
/// first month with sold crew hours exists.
async fn hourly_rate(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<Option<profit_service::HourlyRate>>, ApiError> {
    require_admin(&claims)?;
    let rate = current_rate(&state).await?;
    Ok(Json(profit_service::hourly_rate(&state.db, rate).await?))
}

async fn employees(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<profit_service::EmployeesResponse>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(profit_service::employees(&state.db).await?))
}

async fn labor_preview(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(month): Path<String>,
) -> Result<Json<profit_service::TransferPreview>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(profit_service::transfer_preview(&state.db, month_param(&month)?).await?))
}

async fn labor_transfer(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(month): Path<String>,
) -> Result<Json<profit_service::TransferPreview>, ApiError> {
    require_admin(&claims)?;
    let month = month_param(&month)?;
    if month > month_start(profit_service::today_berlin()) {
        return Err(ApiError::Validation("Zukünftige Monate können nicht übernommen werden".into()));
    }
    let result = profit_service::transfer(&state.db, month, &actor(&claims)).await?;
    tracing::info!(admin = %claims.sub, %month, hours = result.total_hours, "Stunden übernommen");
    Ok(Json(result))
}

// ── Categories ──────────────────────────────────────────────────────────────

async fn list_categories(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<Vec<accounting_repo::CategoryRow>>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(accounting_repo::list_categories(&state.db).await?))
}

#[derive(Debug, Deserialize)]
struct CategoryBody {
    name: String,
    kind: String,
    default_vat_rate: i16,
}

async fn create_category(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(body): Json<CategoryBody>,
) -> Result<(StatusCode, Json<accounting_repo::CategoryRow>), ApiError> {
    require_admin(&claims)?;
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::Validation("Name muss 1–80 Zeichen lang sein".into()));
    }
    if !["fixed", "variable", "wages"].contains(&body.kind.as_str()) {
        return Err(ApiError::Validation("Ungültige Kostenart".into()));
    }
    validate_vat(body.default_vat_rate)?;
    let row = accounting_repo::insert_category(&state.db, name, &body.kind, body.default_vat_rate).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Debug, Deserialize)]
struct CategoryPatch {
    /// Position names this category is recharged through; empty = Eigene Kosten.
    recharge_positions: Vec<String>,
}

async fn update_category(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<CategoryPatch>,
) -> Result<Json<accounting_repo::CategoryRow>, ApiError> {
    require_admin(&claims)?;
    let mut positions: Vec<String> = Vec::new();
    for p in body.recharge_positions.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        if p.chars().count() > 120 {
            return Err(ApiError::Validation("Positionsname zu lang".into()));
        }
        if !positions.iter().any(|x| x.eq_ignore_ascii_case(p)) {
            positions.push(p.to_string());
        }
    }
    if positions.len() > 30 {
        return Err(ApiError::Validation("Höchstens 30 Positionen pro Kategorie".into()));
    }
    Ok(Json(accounting_repo::set_category_recharge(&state.db, id, &positions, &actor(&claims)).await?))
}

/// `GET /admin/profit/positions` — names for the recharge picker: the KVA catalogue,
/// the Fahrkostenpauschale, and every non-labor position used in the last year.
async fn position_names(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<Vec<String>>, ApiError> {
    require_admin(&claims)?;
    let mut names: Vec<String> = vec!["Fahrkostenpauschale".into()];
    names.extend(settings_repo::POSITION_CATALOG.iter().map(|p| p.label.to_string()));
    for n in accounting_repo::used_position_names(&state.db).await? {
        if !names.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
            names.push(n);
        }
    }
    Ok(Json(names))
}

async fn recharge(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<YearQuery>,
) -> Result<Json<profit_service::RechargeReport>, ApiError> {
    require_admin(&claims)?;
    let year = q.year.unwrap_or_else(|| profit_service::today_berlin().year());
    Ok(Json(profit_service::recharge_report(&state.db, year).await?))
}

// ── Expenses ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ExpenseQuery {
    from: Option<String>,
    to: Option<String>,
    category_id: Option<Uuid>,
    vehicle_id: Option<Uuid>,
    inquiry_id: Option<Uuid>,
    employee_id: Option<Uuid>,
    status: Option<String>,
}

async fn list_expenses(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<ExpenseQuery>,
) -> Result<Json<Vec<accounting_repo::ExpenseRow>>, ApiError> {
    require_admin(&claims)?;
    accounting_repo::generate_recurring_drafts(&state.db, month_start(profit_service::today_berlin())).await?;
    let filter = ExpenseFilter {
        from_month: q.from.as_deref().map(month_param).transpose()?,
        to_month: q.to.as_deref().map(month_param).transpose()?,
        category_id: q.category_id,
        vehicle_id: q.vehicle_id,
        inquiry_id: q.inquiry_id,
        employee_id: q.employee_id,
        status: q.status,
    };
    Ok(Json(accounting_repo::list_expenses(&state.db, &filter).await?))
}

#[derive(Debug, Deserialize)]
struct ExpenseBody {
    category_id: Uuid,
    receipt_date: NaiveDate,
    #[serde(default)]
    paid_on: Option<NaiveDate>,
    /// `YYYY-MM`; defaults to the receipt month.
    #[serde(default)]
    period_month: Option<String>,
    #[serde(default)]
    supplier: Option<String>,
    #[serde(default)]
    receipt_number: Option<String>,
    #[serde(default)]
    description: Option<String>,
    brutto_cents: i64,
    vat_rate: i16,
    #[serde(default)]
    vehicle_id: Option<Uuid>,
    #[serde(default)]
    inquiry_id: Option<Uuid>,
    #[serde(default)]
    employee_id: Option<Uuid>,
}

async fn expense_input(state: &AppState, body: ExpenseBody, status: String) -> Result<ExpenseInput, ApiError> {
    validate_vat(body.vat_rate)?;
    if body.brutto_cents <= 0 {
        return Err(ApiError::Validation("Betrag muss größer als 0 sein".into()));
    }
    let category = accounting_repo::fetch_category(&state.db, body.category_id)
        .await?
        .ok_or_else(|| ApiError::Validation("Unbekannte Kategorie".into()))?;
    let period_month = match body.period_month.as_deref() {
        Some(m) if !m.is_empty() => month_param(m)?,
        _ => month_start(body.receipt_date),
    };
    // A wage line without VAT is the only sensible reading — Lohn carries no USt.
    let vat_rate = if category.kind == "wages" { 0 } else { body.vat_rate };
    let (netto, vat) = split_brutto(body.brutto_cents, vat_rate);
    Ok(ExpenseInput {
        category_id: body.category_id,
        status,
        receipt_date: body.receipt_date,
        paid_on: body.paid_on,
        period_month,
        supplier: clean(body.supplier),
        receipt_number: clean(body.receipt_number),
        description: clean(body.description),
        netto_cents: netto,
        vat_rate,
        vat_cents: vat,
        brutto_cents: body.brutto_cents,
        vehicle_id: body.vehicle_id,
        inquiry_id: body.inquiry_id,
        employee_id: body.employee_id,
    })
}

async fn create_expense(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(body): Json<ExpenseBody>,
) -> Result<(StatusCode, Json<accounting_repo::ExpenseRow>), ApiError> {
    require_admin(&claims)?;
    let input = expense_input(&state, body, "booked".into()).await?;
    let row = accounting_repo::insert_expense(&state.db, &input, &actor(&claims)).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_expense(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<ExpenseBody>,
) -> Result<Json<accounting_repo::ExpenseRow>, ApiError> {
    require_admin(&claims)?;
    let existing = accounting_repo::fetch_expense(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    let input = expense_input(&state, body, existing.status).await?;
    Ok(Json(accounting_repo::update_expense(&state.db, id, &input, &actor(&claims)).await?))
}

async fn delete_expense(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_admin(&claims)?;
    let keys = accounting_repo::delete_expense(&state.db, id, &actor(&claims)).await?;
    for key in keys {
        if let Err(e) = state.storage.delete(&key).await {
            tracing::warn!("Beleg {key} konnte nicht gelöscht werden: {e}");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn storno_expense(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<accounting_repo::ExpenseRow>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(accounting_repo::storno_expense(&state.db, id, &actor(&claims)).await?))
}

async fn confirm_expense(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<accounting_repo::ExpenseRow>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(accounting_repo::confirm_expense(&state.db, id, &actor(&claims)).await?))
}

async fn upload_receipt(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<accounting_repo::ExpenseRow>, ApiError> {
    require_admin(&claims)?;
    let existing = accounting_repo::fetch_expense(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;

    let mut file: Option<(bytes::Bytes, String, String)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Fehler beim Lesen der Datei: {e}")))?
    {
        if field.name() == Some("file") {
            let name = field.file_name().unwrap_or("beleg").to_string();
            let ct = field.content_type().unwrap_or("application/octet-stream").to_string();
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("Fehler beim Lesen der Dateidaten: {e}")))?;
            file = Some((data, name, ct));
            break;
        }
    }
    let (data, filename, mime) = file.ok_or_else(|| ApiError::BadRequest("Kein Dateifeld gefunden".into()))?;
    if data.is_empty() {
        return Err(ApiError::Validation("Datei ist leer".into()));
    }
    if data.len() > MAX_RECEIPT_BYTES {
        return Err(ApiError::Validation("Beleg ist größer als 15 MB".into()));
    }
    if !(mime.starts_with("image/") || mime == "application/pdf") {
        return Err(ApiError::Validation("Nur Bilder oder PDF als Beleg".into()));
    }

    let sha = hex::encode(Sha256::digest(&data));
    let ext = filename
        .rsplit('.')
        .next()
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()) && *e != filename)
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| if mime == "application/pdf" { "pdf".into() } else { "jpg".into() });
    // Content-addressed: a replaced receipt never overwrites the old object.
    let key = format!("belege/{}/{id}-{}.{ext}", existing.period_month.format("%Y-%m"), &sha[..16]);
    state.storage.upload(&key, data, &mime).await.map_err(|e| {
        tracing::error!("S3 upload error for receipt: {e}");
        ApiError::Internal("Datei-Upload fehlgeschlagen".into())
    })?;

    let row = accounting_repo::set_receipt(&state.db, id, &key, &filename, &mime, &sha, &actor(&claims)).await?;
    Ok(Json(row))
}

async fn download_receipt(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_admin(&claims)?;
    let row = accounting_repo::fetch_expense(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Buchung nicht gefunden".into()))?;
    let key = row.receipt_s3_key.ok_or_else(|| ApiError::NotFound("Kein Beleg vorhanden".into()))?;
    let data = state.storage.download(&key).await.map_err(|e| {
        tracing::error!("S3 download error for receipt: {e}");
        ApiError::NotFound("Beleg nicht abrufbar".into())
    })?;
    let filename = row
        .receipt_filename
        .unwrap_or_else(|| "beleg".into())
        .replace(['"', '\r', '\n'], "");
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, row.receipt_mime.unwrap_or_else(|| "application/octet-stream".into()))
        .header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{filename}\""))
        .body(axum::body::Body::from(data))
        .unwrap())
}

// ── Recurring ───────────────────────────────────────────────────────────────

async fn list_recurring(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<Vec<accounting_repo::RecurringRow>>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(accounting_repo::list_recurring(&state.db).await?))
}

#[derive(Debug, Deserialize)]
struct RecurringBody {
    category_id: Uuid,
    label: String,
    #[serde(default)]
    supplier: Option<String>,
    brutto_cents: i64,
    vat_rate: i16,
    #[serde(default = "one")]
    interval_months: i16,
    #[serde(default = "one")]
    day_of_month: i16,
    /// `YYYY-MM`
    start_month: String,
    #[serde(default)]
    end_month: Option<String>,
    #[serde(default)]
    vehicle_id: Option<Uuid>,
    #[serde(default = "yes")]
    active: bool,
    #[serde(default)]
    notes: Option<String>,
}

fn one() -> i16 {
    1
}
fn yes() -> bool {
    true
}

async fn recurring_input(state: &AppState, body: RecurringBody) -> Result<RecurringInput, ApiError> {
    validate_vat(body.vat_rate)?;
    let label = body.label.trim().to_string();
    if label.is_empty() || label.chars().count() > 120 {
        return Err(ApiError::Validation("Bezeichnung muss 1–120 Zeichen lang sein".into()));
    }
    if body.brutto_cents <= 0 {
        return Err(ApiError::Validation("Betrag muss größer als 0 sein".into()));
    }
    if ![1, 3, 6, 12].contains(&body.interval_months) {
        return Err(ApiError::Validation("Intervall muss 1, 3, 6 oder 12 Monate sein".into()));
    }
    if !(1..=28).contains(&body.day_of_month) {
        return Err(ApiError::Validation("Fälligkeitstag muss zwischen 1 und 28 liegen".into()));
    }
    let category = accounting_repo::fetch_category(&state.db, body.category_id)
        .await?
        .ok_or_else(|| ApiError::Validation("Unbekannte Kategorie".into()))?;
    let start = month_param(&body.start_month)?;
    let end = match body.end_month.as_deref() {
        Some(m) if !m.is_empty() => Some(month_param(m)?),
        _ => None,
    };
    if end.is_some_and(|e| e < start) {
        return Err(ApiError::Validation("Ende liegt vor dem Beginn".into()));
    }
    let vat_rate = if category.kind == "wages" { 0 } else { body.vat_rate };
    let (netto, vat) = split_brutto(body.brutto_cents, vat_rate);
    Ok(RecurringInput {
        category_id: body.category_id,
        label,
        supplier: clean(body.supplier),
        netto_cents: netto,
        vat_rate,
        vat_cents: vat,
        brutto_cents: body.brutto_cents,
        interval_months: body.interval_months,
        day_of_month: body.day_of_month,
        start_month: start,
        end_month: end,
        vehicle_id: body.vehicle_id,
        active: body.active,
        notes: clean(body.notes),
    })
}

async fn create_recurring(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(body): Json<RecurringBody>,
) -> Result<(StatusCode, Json<accounting_repo::RecurringRow>), ApiError> {
    require_admin(&claims)?;
    let input = recurring_input(&state, body).await?;
    let row = accounting_repo::insert_recurring(&state.db, &input, &actor(&claims)).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_recurring(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(body): Json<RecurringBody>,
) -> Result<Json<accounting_repo::RecurringRow>, ApiError> {
    require_admin(&claims)?;
    let input = recurring_input(&state, body).await?;
    Ok(Json(accounting_repo::update_recurring(&state.db, id, &input, &actor(&claims)).await?))
}

async fn delete_recurring(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_admin(&claims)?;
    accounting_repo::delete_recurring(&state.db, id, &actor(&claims)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Settings + audit ────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize, Deserialize)]
struct ProfitSettings {
    default_rate_cents: i64,
    /// Assumptions of the Stundensatz-Kalkulation. Optional on PUT so older
    /// clients that only send the wage rate keep working.
    #[serde(default)]
    hourly: Option<HourlyCalcSettings>,
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<ProfitSettings>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(ProfitSettings {
        default_rate_cents: accounting_repo::get_default_rate(&state.db).await?,
        hourly: Some(accounting_repo::get_hourly_calc(&state.db).await?),
    }))
}

fn validate_hourly(h: &HourlyCalcSettings) -> Result<(), ApiError> {
    if !(1..=24).contains(&h.window_months) {
        return Err(ApiError::Validation("Zeitraum muss 1–24 Monate sein".into()));
    }
    if !(0..=100_000_000).contains(&h.target_profit_cents) {
        return Err(ApiError::Validation("Zielgewinn muss zwischen 0 und 1 Mio. € liegen".into()));
    }
    if h.planned_hours_per_month.is_some_and(|x| !(0.0..=100_000.0).contains(&x)) {
        return Err(ApiError::Validation("Geplante Stunden ungültig".into()));
    }
    if h.capacity_crew.is_some_and(|c| !(0..=500).contains(&c)) {
        return Err(ApiError::Validation("Teamgröße ungültig".into()));
    }
    if !(0.5..=16.0).contains(&h.capacity_hours_per_day) {
        return Err(ApiError::Validation("Stunden pro Tag müssen zwischen 0,5 und 16 liegen".into()));
    }
    if !(1.0..=31.0).contains(&h.capacity_days_per_month) {
        return Err(ApiError::Validation("Arbeitstage pro Monat müssen zwischen 1 und 31 liegen".into()));
    }
    Ok(())
}

async fn put_settings(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Json(body): Json<ProfitSettings>,
) -> Result<Json<ProfitSettings>, ApiError> {
    require_admin(&claims)?;
    if !(500..=10_000).contains(&body.default_rate_cents) {
        return Err(ApiError::Validation("Stundensatz muss zwischen 5 € und 100 € liegen".into()));
    }
    if let Some(h) = &body.hourly {
        validate_hourly(h)?;
    }
    if accounting_repo::get_default_rate(&state.db).await? != body.default_rate_cents {
        accounting_repo::set_default_rate(&state.db, body.default_rate_cents, &actor(&claims)).await?;
    }
    if let Some(h) = &body.hourly {
        accounting_repo::set_hourly_calc(&state.db, h, &actor(&claims)).await?;
    }
    get_settings(State(state), Extension(claims)).await
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    entity_id: Option<Uuid>,
}

async fn audit(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<accounting_repo::AuditRow>>, ApiError> {
    require_admin(&claims)?;
    Ok(Json(accounting_repo::list_audit(&state.db, q.entity_id, 200).await?))
}

#[cfg(test)]
mod tests {
    use super::split_brutto;

    #[test]
    fn brutto_split_keeps_the_cent() {
        assert_eq!(split_brutto(11_900, 19), (10_000, 1_900));
        assert_eq!(split_brutto(10_700, 7), (10_000, 700));
        assert_eq!(split_brutto(5_000, 0), (5_000, 0));
        // Rounding never loses a cent: netto + USt == brutto.
        let (n, v) = split_brutto(1_999, 19);
        assert_eq!(n + v, 1_999);
    }
}
