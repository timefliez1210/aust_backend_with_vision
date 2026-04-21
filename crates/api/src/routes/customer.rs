use axum::{
    extract::{Path, State},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::middleware::customer_auth::CustomerClaims;
use crate::repositories::customer_auth_repo;
use crate::services::otp_service::{
    self, OtpBackend, OtpRequest, OtpResponse, VerifyRequest,
};
use crate::{ApiError, AppState};

/// Protected customer routes (require customer session token).
/// Middleware is applied in lib.rs.
pub fn protected_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/me", get(get_profile))
        .route("/inquiries", get(list_inquiries))
        .route("/inquiries/{id}", get(get_inquiry_detail))
        .route("/inquiries/{id}/accept", post(accept_inquiry))
        .route("/inquiries/{id}/reject", post(reject_inquiry))
        .route("/inquiries/{id}/pdf", get(download_inquiry_pdf))
}

/// Public auth routes (no token required).
pub fn auth_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/request", post(request_otp))
        .route("/auth/verify", post(verify_otp))
}

// --- OTP Auth (delegates to shared otp_service) ---

/// Customer OTP backend — routes OTP operations to `customer_otps` / `customer_sessions` tables.
///
/// **Caller**: `request_otp`, `verify_otp` handlers below.
/// **Why**: Implements `OtpBackend` so the shared OTP logic works for customer auth.
///          Customers are always sent a code (upserted on verify), so `check_existence_before_send`
///          returns `false`.
struct CustomerOtpBackend;

impl OtpBackend for CustomerOtpBackend {
    fn check_existence_before_send(&self) -> bool {
        false
    }

    async fn user_exists(&self, _pool: &PgPool, _email: &str) -> Result<bool, sqlx::Error> {
        // Never called because check_existence_before_send is false
        Ok(true)
    }

    async fn count_recent_otps(&self, pool: &PgPool, email: &str) -> Result<i64, sqlx::Error> {
        customer_auth_repo::count_recent_otps(pool, email).await
    }

    async fn insert_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        customer_auth_repo::insert_otp(pool, email, code, expires_at).await
    }

    async fn find_valid_otp(
        &self,
        pool: &PgPool,
        email: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        customer_auth_repo::find_valid_otp(pool, email, code, now).await
    }

    async fn mark_otp_used(&self, pool: &PgPool, otp_id: Uuid) -> Result<(), sqlx::Error> {
        customer_auth_repo::mark_otp_used(pool, otp_id).await
    }

    fn otp_email_subject(&self) -> &str {
        "Ihr Zugangscode — Aust Umzüge"
    }

    fn request_success_message(&self) -> &str {
        "Code wurde gesendet"
    }

    fn user_label(&self) -> &str {
        "Customer"
    }
}

/// POST /customer/auth/request — generate 6-digit OTP and send via email.
async fn request_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OtpRequest>,
) -> Result<Json<OtpResponse>, ApiError> {
    let resp = otp_service::handle_request_otp(
        &CustomerOtpBackend,
        &state.db,
        &state.config.email,
        &body.email,
    )
    .await?;
    Ok(Json(resp))
}

#[derive(Debug, Serialize)]
struct VerifyResponse {
    token: String,
    customer: CustomerInfo,
}

#[derive(Debug, Serialize)]
struct CustomerInfo {
    id: Uuid,
    email: String,
    name: Option<String>,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    phone: Option<String>,
}

/// POST /customer/auth/verify — verify OTP, upsert customer, return session token.
async fn verify_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let email = body.email.trim().to_lowercase();

    // Shared OTP validation + token generation
    let token =
        otp_service::handle_verify_otp(&CustomerOtpBackend, &state.db, &body.email, &body.code)
            .await?;

    // Upsert customer by email
    let now = Utc::now();
    let customer = customer_auth_repo::upsert_customer_minimal(&state.db, &email, now).await?;

    // Create session
    let expires_at = now + chrono::Duration::days(30);
    customer_auth_repo::create_session(&state.db, customer.0, &token, expires_at).await?;

    tracing::info!(customer_id = %customer.0, email = %email, "Customer authenticated via OTP");

    Ok(Json(VerifyResponse {
        token,
        customer: CustomerInfo {
            id: customer.0,
            email: customer.1,
            name: customer.2,
            salutation: customer.3,
            first_name: customer.4,
            last_name: customer.5,
            phone: customer.6,
        },
    }))
}

// --- Profile ---

/// GET /customer/me — return current customer profile.
async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
) -> Result<Json<CustomerInfo>, ApiError> {
    let (id, email, name, salutation, first_name, last_name, phone) =
        customer_auth_repo::fetch_customer_profile(&state.db, claims.customer_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Kunde nicht gefunden".into()))?;

    Ok(Json(CustomerInfo {
        id,
        email,
        name,
        salutation,
        first_name,
        last_name,
        phone,
    }))
}

// --- Inquiries ---

#[derive(Debug, Serialize)]
struct InquirySummary {
    id: Uuid,
    status: String,
    scheduled_date: Option<NaiveDate>,
    created_at: DateTime<Utc>,
    origin_city: Option<String>,
    destination_city: Option<String>,
    estimated_volume_m3: Option<f64>,
    price_cents: Option<i64>,
}

/// GET /customer/inquiries — list customer's inquiries with latest offer price.
async fn list_inquiries(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
) -> Result<Json<Vec<InquirySummary>>, ApiError> {
    let rows = customer_auth_repo::list_customer_inquiries(&state.db, claims.customer_id).await?;

    let inquiries: Vec<InquirySummary> = rows
        .into_iter()
        .map(|r| InquirySummary {
            id: r.0,
            status: r.1,
            scheduled_date: r.2,
            created_at: r.3,
            origin_city: r.4,
            destination_city: r.5,
            estimated_volume_m3: r.6,
            price_cents: r.7,
        })
        .collect();

    Ok(Json(inquiries))
}

// --- Inquiry Detail ---

#[derive(Debug, Serialize)]
struct InquiryDetail {
    id: Uuid,
    status: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    scheduled_date: Option<NaiveDate>,
    origin_address: Option<AddressInfo>,
    destination_address: Option<AddressInfo>,
    estimation: Option<EstimationInfo>,
    offers: Vec<OfferInfo>,
}

#[derive(Debug, Serialize)]
struct AddressInfo {
    street: String,
    city: String,
    postal_code: String,
    floor: Option<String>,
}

#[derive(Debug, Serialize)]
struct EstimationInfo {
    total_volume_m3: f64,
    confidence_score: f64,
    items: Vec<EstimationItem>,
}

#[derive(Debug, Serialize)]
struct EstimationItem {
    name: String,
    volume_m3: f64,
    quantity: i32,
}

#[derive(Debug, Serialize)]
struct OfferInfo {
    id: Uuid,
    price_cents: i64,
    status: String,
    valid_until: Option<NaiveDate>,
    persons: Option<i32>,
    hours_estimated: Option<f64>,
}

/// GET /customer/inquiries/{id} — inquiry detail with estimation + offers.
/// Validates ownership (inquiry must belong to the authenticated customer).
async fn get_inquiry_detail(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<InquiryDetail>, ApiError> {
    // Fetch inquiry with ownership check
    let inquiry_row = customer_auth_repo::fetch_inquiry_owned(&state.db, id, claims.customer_id).await?;

    let (qid, status, volume, distance, pdate, origin_id, dest_id) =
        inquiry_row.ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Fetch addresses
    let origin_address = if let Some(addr_id) = origin_id {
        fetch_address(&state.db, addr_id).await?
    } else {
        None
    };

    let destination_address = if let Some(addr_id) = dest_id {
        fetch_address(&state.db, addr_id).await?
    } else {
        None
    };

    // Fetch latest estimation
    let estimation = fetch_estimation(&state.db, id).await?;

    // Fetch offers
    let offer_rows = customer_auth_repo::fetch_inquiry_offers(&state.db, id).await?;

    let offers: Vec<OfferInfo> = offer_rows
        .into_iter()
        .map(|r| OfferInfo {
            id: r.0,
            price_cents: r.1,
            status: r.2,
            valid_until: r.3,
            persons: r.4,
            hours_estimated: r.5,
        })
        .collect();

    Ok(Json(InquiryDetail {
        id: qid,
        status,
        estimated_volume_m3: volume,
        distance_km: distance,
        scheduled_date: pdate,
        origin_address,
        destination_address,
        estimation,
        offers,
    }))
}

async fn fetch_address(
    db: &sqlx::PgPool,
    id: Uuid,
) -> Result<Option<AddressInfo>, ApiError> {
    let row = customer_auth_repo::fetch_address_display(db, id).await?;
    Ok(row.map(|(street, city, postal_code, floor)| AddressInfo {
        street,
        city,
        postal_code,
        floor,
    }))
}

async fn fetch_estimation(
    db: &sqlx::PgPool,
    inquiry_id: Uuid,
) -> Result<Option<EstimationInfo>, ApiError> {
    let row = customer_auth_repo::fetch_latest_estimation(db, inquiry_id).await?;

    let Some((total_volume_m3, confidence_score, result_data)) = row else {
        return Ok(None);
    };

    // Parse items from result_data
    let items = parse_estimation_items(result_data);

    Ok(Some(EstimationInfo {
        total_volume_m3,
        confidence_score,
        items,
    }))
}

/// Parse detected items from volume estimation result_data JSON.
fn parse_estimation_items(result_data: Option<serde_json::Value>) -> Vec<EstimationItem> {
    let Some(data) = result_data else {
        return Vec::new();
    };

    // result_data may be an array of items directly, or a nested structure
    let items_array = if data.is_array() {
        data.as_array().cloned().unwrap_or_default()
    } else if let Some(items) = data.get("detected_items").and_then(|v| v.as_array()) {
        items.clone()
    } else if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
        items.clone()
    } else {
        // Try to extract from LLM vision results (array of analysis results)
        let mut all_items = Vec::new();
        if let Some(results) = data.as_array() {
            for result in results {
                if let Some(items) = result.get("items").and_then(|v| v.as_array()) {
                    all_items.extend(items.iter().cloned());
                }
                if let Some(items) = result.get("detected_items").and_then(|v| v.as_array()) {
                    all_items.extend(items.iter().cloned());
                }
            }
        }
        all_items
    };

    items_array
        .iter()
        .filter_map(|item| {
            let name = item
                .get("name")
                .or_else(|| item.get("german_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unbekannt")
                .to_string();
            let volume_m3 = item
                .get("volume_m3")
                .or_else(|| item.get("total_volume_m3"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let quantity = item
                .get("quantity")
                .and_then(|v| v.as_i64())
                .unwrap_or(1) as i32;

            Some(EstimationItem {
                name,
                volume_m3,
                quantity,
            })
        })
        .collect()
}

// --- Accept / Reject Inquiries ---

/// POST /customer/inquiries/{id}/accept — accept the active offer for this inquiry,
/// sync statuses, notify admin.
///
/// Takes an `inquiry_id` from the path, finds the latest non-rejected/non-cancelled
/// offer, validates ownership via `inquiry.customer_id`, then updates both the offer
/// and inquiry status.
async fn accept_inquiry(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Validate ownership: inquiry must belong to this customer
    let (_inq_id, customer_name) =
        customer_auth_repo::validate_inquiry_ownership(&state.db, inquiry_id, claims.customer_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Find the active offer (latest non-rejected/non-cancelled) for this inquiry
    let (offer_id, offer_status) =
        customer_auth_repo::fetch_active_offer_with_status(&state.db, inquiry_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    if !["draft", "sent"].contains(&offer_status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Angebot kann im Status '{}' nicht angenommen werden",
            offer_status
        )));
    }

    let now = Utc::now();

    // Update offer → accepted
    customer_auth_repo::update_offer_status(&state.db, offer_id, "accepted").await?;

    // Update inquiry → accepted
    crate::repositories::inquiry_repo::update_status(&state.db, inquiry_id, "accepted", now).await?;

    // Notify admin via Telegram
    notify_admin_telegram(
        &state.config.telegram,
        &format!("✅ Kunde hat Angebot angenommen: {customer_name}"),
    )
    .await;

    tracing::info!(
        inquiry_id = %inquiry_id,
        offer_id = %offer_id,
        customer_id = %claims.customer_id,
        "Customer accepted offer via inquiry"
    );

    Ok(Json(serde_json::json!({
        "message": "Angebot angenommen",
        "status": "accepted",
    })))
}

/// POST /customer/inquiries/{id}/reject — reject the active offer for this inquiry,
/// sync statuses, notify admin.
///
/// Takes an `inquiry_id` from the path, finds the latest non-rejected/non-cancelled
/// offer, validates ownership via `inquiry.customer_id`, then updates both the offer
/// and inquiry status.
async fn reject_inquiry(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Validate ownership: inquiry must belong to this customer
    let (_inq_id, customer_name) =
        customer_auth_repo::validate_inquiry_ownership(&state.db, inquiry_id, claims.customer_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Find the active offer (latest non-rejected/non-cancelled) for this inquiry
    let (offer_id, offer_status) =
        customer_auth_repo::fetch_active_offer_with_status(&state.db, inquiry_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    if !["draft", "sent"].contains(&offer_status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Angebot kann im Status '{}' nicht abgelehnt werden",
            offer_status
        )));
    }

    let now = Utc::now();

    // Update offer → rejected
    customer_auth_repo::update_offer_status(&state.db, offer_id, "rejected").await?;

    // Update inquiry → rejected
    crate::repositories::inquiry_repo::update_status(&state.db, inquiry_id, "rejected", now).await?;

    // Notify admin via Telegram
    notify_admin_telegram(
        &state.config.telegram,
        &format!("❌ Kunde hat Angebot abgelehnt: {customer_name}"),
    )
    .await;

    tracing::info!(
        inquiry_id = %inquiry_id,
        offer_id = %offer_id,
        customer_id = %claims.customer_id,
        "Customer rejected offer via inquiry"
    );

    Ok(Json(serde_json::json!({
        "message": "Angebot abgelehnt",
        "status": "rejected",
    })))
}

// --- PDF Download ---

/// GET /customer/inquiries/{id}/pdf — download the active offer PDF for this inquiry.
///
/// Takes an `inquiry_id` from the path, validates ownership, finds the latest
/// non-rejected/non-cancelled offer with a PDF, and streams the PDF.
async fn download_inquiry_pdf(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
    Path(inquiry_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    // Validate ownership: inquiry must belong to this customer
    let owns = customer_auth_repo::check_inquiry_ownership(&state.db, inquiry_id, claims.customer_id).await?;
    if !owns {
        return Err(ApiError::NotFound("Anfrage nicht gefunden".into()));
    }

    // Find the active offer's PDF storage key
    let offer_row = crate::repositories::offer_repo::fetch_active_pdf_key(&state.db, inquiry_id).await?;

    let (offer_id, storage_key) =
        offer_row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    let storage_key =
        storage_key.ok_or_else(|| ApiError::BadRequest("Angebot hat kein PDF".into()))?;

    let pdf_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| match e {
            aust_storage::StorageError::NotFound(_) => {
                ApiError::NotFound("Angebot-PDF nicht gefunden.".into())
            }
            _ => ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")),
        })?;

    Ok(axum::response::Response::builder()
        .header("Content-Type", "application/pdf")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"Angebot_{offer_id}.pdf\""),
        )
        .body(axum::body::Body::from(pdf_bytes))
        .unwrap())
}

// --- Helpers ---

/// Send a notification to the admin via Telegram.
async fn notify_admin_telegram(config: &aust_core::config::TelegramConfig, text: &str) {
    let client = reqwest::Client::new();
    let api_url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        config.bot_token
    );
    let payload = serde_json::json!({
        "chat_id": config.admin_chat_id,
        "text": text,
    });

    if let Err(e) = client.post(&api_url).json(&payload).send().await {
        tracing::error!("Failed to send Telegram notification: {e}");
    }
}
