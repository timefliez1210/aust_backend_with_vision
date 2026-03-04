use axum::{
    extract::{Path, State},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::middleware::customer_auth::CustomerClaims;
use crate::services::db::find_active_booking_id;
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

// --- OTP Auth ---

#[derive(Debug, Deserialize)]
struct OtpRequest {
    email: String,
}

#[derive(Debug, Serialize)]
struct OtpResponse {
    message: String,
}

/// POST /customer/auth/request — generate 6-digit OTP and send via email.
async fn request_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OtpRequest>,
) -> Result<Json<OtpResponse>, ApiError> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::Validation("Ungültige E-Mail-Adresse".into()));
    }

    // Rate limit: max 3 OTPs per email in last 10 minutes
    let recent_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM customer_otps WHERE email = $1 AND created_at > NOW() - INTERVAL '10 minutes'",
    )
    .bind(&email)
    .fetch_one(&state.db)
    .await?;

    if recent_count.0 >= 3 {
        return Err(ApiError::BadRequest(
            "Zu viele Anfragen. Bitte warten Sie einige Minuten.".into(),
        ));
    }

    // Generate 6-digit code
    let code: String = {
        let mut rng = rand::rng();
        format!("{:06}", rng.random_range(0..1_000_000u32))
    };

    let expires_at = Utc::now() + chrono::Duration::minutes(10);

    sqlx::query(
        "INSERT INTO customer_otps (email, code, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(&email)
    .bind(&code)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    // Send OTP via SMTP
    let subject = "Ihr Zugangscode — Aust Umzüge";
    let body_text = format!(
        "Guten Tag,\n\nIhr Zugangscode lautet: {code}\n\nDieser Code ist 10 Minuten gültig.\n\nMit freundlichen Grüßen,\nAust Umzüge"
    );

    send_otp_email(&state.config.email, &email, subject, &body_text)
        .await
        .map_err(|e| {
            tracing::error!("Failed to send OTP email to {email}: {e}");
            ApiError::Internal("E-Mail konnte nicht gesendet werden".into())
        })?;

    tracing::info!(email = %email, "OTP sent");

    Ok(Json(OtpResponse {
        message: "Code wurde gesendet".to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    email: String,
    code: String,
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
    phone: Option<String>,
}

/// POST /customer/auth/verify — verify OTP, upsert customer, return session token.
async fn verify_otp(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let email = body.email.trim().to_lowercase();
    let code = body.code.trim().to_string();

    if code.len() != 6 {
        return Err(ApiError::Validation("Code muss 6 Stellen haben".into()));
    }

    // Find matching unused OTP
    let otp_row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT id FROM customer_otps
        WHERE email = $1 AND code = $2 AND used = FALSE AND expires_at > $3
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(&email)
    .bind(&code)
    .bind(Utc::now())
    .fetch_optional(&state.db)
    .await?;

    let (otp_id,) = otp_row.ok_or_else(|| {
        ApiError::Unauthorized("Ungültiger oder abgelaufener Code".into())
    })?;

    // Mark OTP as used
    sqlx::query("UPDATE customer_otps SET used = TRUE WHERE id = $1")
        .bind(otp_id)
        .execute(&state.db)
        .await?;

    let now = Utc::now();

    // Upsert customer by email
    let customer: (Uuid, String, Option<String>, Option<String>) = sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, created_at, updated_at)
        VALUES ($1, $2, $3, $3)
        ON CONFLICT (email) DO UPDATE SET updated_at = $3
        RETURNING id, email, name, phone
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Generate session token (64 random hex chars)
    let token = generate_session_token();
    let expires_at = now + chrono::Duration::days(30);

    sqlx::query(
        "INSERT INTO customer_sessions (customer_id, token, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(customer.0)
    .bind(&token)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    tracing::info!(customer_id = %customer.0, email = %email, "Customer authenticated via OTP");

    Ok(Json(VerifyResponse {
        token,
        customer: CustomerInfo {
            id: customer.0,
            email: customer.1,
            name: customer.2,
            phone: customer.3,
        },
    }))
}

// --- Profile ---

/// GET /customer/me — return current customer profile.
async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<CustomerClaims>,
) -> Result<Json<CustomerInfo>, ApiError> {
    let row: Option<(Uuid, String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT id, email, name, phone FROM customers WHERE id = $1")
            .bind(claims.customer_id)
            .fetch_optional(&state.db)
            .await?;

    let (id, email, name, phone) =
        row.ok_or_else(|| ApiError::NotFound("Kunde nicht gefunden".into()))?;

    Ok(Json(CustomerInfo {
        id,
        email,
        name,
        phone,
    }))
}

// --- Inquiries ---

#[derive(Debug, Serialize)]
struct InquirySummary {
    id: Uuid,
    status: String,
    preferred_date: Option<DateTime<Utc>>,
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
    let rows: Vec<(
        Uuid,
        String,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<i64>,
    )> = sqlx::query_as(
        r#"
        SELECT
            q.id, q.status, q.preferred_date, q.created_at,
            oa.city AS origin_city,
            da.city AS destination_city,
            q.estimated_volume_m3,
            (SELECT o.price_cents FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1)
        FROM inquiries q
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE q.customer_id = $1
        ORDER BY q.created_at DESC
        "#,
    )
    .bind(claims.customer_id)
    .fetch_all(&state.db)
    .await?;

    let inquiries: Vec<InquirySummary> = rows
        .into_iter()
        .map(|r| InquirySummary {
            id: r.0,
            status: r.1,
            preferred_date: r.2,
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
    preferred_date: Option<DateTime<Utc>>,
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
    let inquiry_row: Option<(
        Uuid,
        String,
        Option<f64>,
        Option<f64>,
        Option<DateTime<Utc>>,
        Option<Uuid>,
        Option<Uuid>,
    )> = sqlx::query_as(
        r#"
        SELECT id, status, estimated_volume_m3, distance_km, preferred_date,
               origin_address_id, destination_address_id
        FROM inquiries
        WHERE id = $1 AND customer_id = $2
        "#,
    )
    .bind(id)
    .bind(claims.customer_id)
    .fetch_optional(&state.db)
    .await?;

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
    let offer_rows: Vec<(Uuid, i64, String, Option<NaiveDate>, Option<i32>, Option<f64>)> =
        sqlx::query_as(
            r#"
            SELECT id, price_cents, status, valid_until, persons, hours_estimated
            FROM offers
            WHERE inquiry_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(id)
        .fetch_all(&state.db)
        .await?;

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
        preferred_date: pdate,
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
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT COALESCE(street, ''), COALESCE(city, ''), COALESCE(postal_code, ''), floor FROM addresses WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;

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
    let row: Option<(f64, f64, Option<serde_json::Value>)> = sqlx::query_as(
        r#"
        SELECT total_volume_m3, confidence_score, result_data
        FROM volume_estimations
        WHERE inquiry_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(db)
    .await?;

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
    let inquiry_row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT q.id, COALESCE(c.name, c.email)
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.id = $1 AND q.customer_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(claims.customer_id)
    .fetch_optional(&state.db)
    .await?;

    let (_inq_id, customer_name) =
        inquiry_row.ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Find the active offer (latest non-rejected/non-cancelled) for this inquiry
    let offer_row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT id, status
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?;

    let (offer_id, offer_status) =
        offer_row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    if !["draft", "sent"].contains(&offer_status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Angebot kann im Status '{}' nicht angenommen werden",
            offer_status
        )));
    }

    let now = Utc::now();

    // Update offer → accepted
    sqlx::query("UPDATE offers SET status = 'accepted' WHERE id = $1")
        .bind(offer_id)
        .execute(&state.db)
        .await?;

    // Update inquiry → accepted
    sqlx::query("UPDATE inquiries SET status = 'accepted', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    // Confirm booking if exists
    if let Some(booking_id) = find_active_booking_id(&state.db, inquiry_id).await? {
        let _ = state.calendar.confirm_booking(booking_id).await;
    }

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
    let inquiry_row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT q.id, COALESCE(c.name, c.email)
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.id = $1 AND q.customer_id = $2
        "#,
    )
    .bind(inquiry_id)
    .bind(claims.customer_id)
    .fetch_optional(&state.db)
    .await?;

    let (_inq_id, customer_name) =
        inquiry_row.ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Find the active offer (latest non-rejected/non-cancelled) for this inquiry
    let offer_row: Option<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT id, status
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?;

    let (offer_id, offer_status) =
        offer_row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    if !["draft", "sent"].contains(&offer_status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Angebot kann im Status '{}' nicht abgelehnt werden",
            offer_status
        )));
    }

    let now = Utc::now();

    // Update offer → rejected
    sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(offer_id)
        .execute(&state.db)
        .await?;

    // Update inquiry → rejected
    sqlx::query("UPDATE inquiries SET status = 'rejected', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(&state.db)
        .await?;

    // Cancel booking if exists
    if let Some(booking_id) = find_active_booking_id(&state.db, inquiry_id).await? {
        let _ = state.calendar.cancel_booking(booking_id).await;
    }

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
    let inquiry_row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM inquiries WHERE id = $1 AND customer_id = $2",
    )
    .bind(inquiry_id)
    .bind(claims.customer_id)
    .fetch_optional(&state.db)
    .await?;

    inquiry_row.ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;

    // Find the active offer's PDF storage key
    let offer_row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, pdf_storage_key
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await?;

    let (offer_id, storage_key) =
        offer_row.ok_or_else(|| ApiError::NotFound("Kein aktives Angebot gefunden".into()))?;

    let storage_key =
        storage_key.ok_or_else(|| ApiError::BadRequest("Angebot hat kein PDF".into()))?;

    let pdf_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")))?;

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

/// Generate a secure 64-character hex session token.
fn generate_session_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Send OTP email via SMTP.
async fn send_otp_email(
    email_config: &aust_core::config::EmailConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_plain_email, send_email};

    let message = build_plain_email(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())
}

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
