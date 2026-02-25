use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use sqlx::{FromRow, PgPool};
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, AppState};
use crate::routes::shared::QuoteRow;
use aust_core::models::{
    DepthSensorResult, DetectedItem, Offer, OfferStatus, PricingInput, Quote,
    VisionAnalysisResult,
};
use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_offer_xlsx, parse_floor, DetectedItemRow, OfferData,
    OfferLineItem, PricingEngine,
};
use aust_storage::StorageProvider;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/generate", post(generate_offer))
        .route("/{id}", get(get_offer))
        .route("/{id}/pdf", get(get_offer_pdf))
}

#[derive(Debug, Deserialize)]
struct GenerateOfferRequest {
    quote_id: Uuid,
    valid_days: Option<i64>,
    #[serde(default)]
    price_cents_netto: Option<i64>,
    #[serde(default)]
    persons: Option<u32>,
    #[serde(default)]
    hours: Option<f64>,
    #[serde(default)]
    rate: Option<f64>,
}


#[derive(Debug, FromRow)]
struct OfferRow {
    id: Uuid,
    quote_id: Uuid,
    price_cents: i64,
    currency: String,
    valid_until: Option<chrono::NaiveDate>,
    pdf_storage_key: Option<String>,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    sent_at: Option<chrono::DateTime<chrono::Utc>>,
    offer_number: Option<String>,
    persons: Option<i32>,
    hours_estimated: Option<f64>,
    rate_per_hour_cents: Option<i64>,
    line_items_json: Option<serde_json::Value>,
}

impl From<OfferRow> for Offer {
    fn from(row: OfferRow) -> Self {
        let status = match row.status.as_str() {
            "draft" => OfferStatus::Draft,
            "sent" => OfferStatus::Sent,
            "viewed" => OfferStatus::Viewed,
            "accepted" => OfferStatus::Accepted,
            "rejected" => OfferStatus::Rejected,
            "expired" => OfferStatus::Expired,
            _ => OfferStatus::Draft,
        };

        Offer {
            id: row.id,
            quote_id: row.quote_id,
            price_cents: row.price_cents,
            currency: row.currency,
            valid_until: row.valid_until,
            pdf_storage_key: row.pdf_storage_key,
            status,
            created_at: row.created_at,
            sent_at: row.sent_at,
            offer_number: row.offer_number,
            persons: row.persons,
            hours_estimated: row.hours_estimated,
            rate_per_hour_cents: row.rate_per_hour_cents,
            line_items_json: row.line_items_json,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct CustomerRow {
    #[allow(dead_code)]
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, FromRow)]
pub(crate) struct AddressRow {
    #[allow(dead_code)]
    pub id: Uuid,
    pub street: String,
    pub city: String,
    pub postal_code: Option<String>,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
}

#[derive(Debug, FromRow)]
pub struct VolumeEstimationRow {
    pub result_data: Option<serde_json::Value>,
    pub source_data: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub total_volume_m3: Option<f64>,
    #[allow(dead_code)]
    pub method: String,
}

/// Summary data for the Telegram caption — populated during offer generation.
pub struct TelegramSummary {
    pub customer_phone: String,
    pub origin_address: String,
    pub origin_floor: String,
    pub origin_elevator: Option<bool>,
    pub dest_address: String,
    pub dest_floor: String,
    pub dest_elevator: Option<bool>,
    pub preferred_date: String,
    pub volume_m3: f64,
    pub items_count: usize,
    pub distance_km: f64,
    pub services: String,
    pub persons: u32,
    pub hours: f64,
    pub rate: f64,
    pub netto_cents: i64,
    pub customer_message: String,
}

/// Result of offer generation — the offer record + PDF bytes for immediate use.
pub struct GeneratedOffer {
    pub offer: Offer,
    pub pdf_bytes: Vec<u8>,
    pub customer_email: String,
    pub customer_name: String,
    pub summary: TelegramSummary,
}

/// Optional overrides for the offer (e.g. when admin edits via Telegram).
#[derive(Default)]
pub struct OfferOverrides {
    pub price_cents: Option<i64>,
    pub persons: Option<u32>,
    pub hours: Option<f64>,
    pub rate: Option<f64>,
}

/// Core offer generation logic. Used by both the API endpoint and the orchestrator.
pub async fn build_offer(
    db: &PgPool,
    storage: &dyn StorageProvider,
    quote_id: Uuid,
    valid_days: Option<i64>,
) -> Result<GeneratedOffer, ApiError> {
    build_offer_with_overrides(db, storage, quote_id, valid_days, &OfferOverrides::default()).await
}

/// Core offer generation logic with optional overrides.
pub async fn build_offer_with_overrides(
    db: &PgPool,
    storage: &dyn StorageProvider,
    quote_id: Uuid,
    valid_days: Option<i64>,
    overrides: &OfferOverrides,
) -> Result<GeneratedOffer, ApiError> {
    // 1. Fetch quote
    let quote_row: QuoteRow = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status,
               estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes WHERE id = $1
        "#,
    )
    .bind(quote_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Quote not found".into()))?;

    let quote = Quote::from(quote_row);

    let volume = quote
        .estimated_volume_m3
        .ok_or_else(|| ApiError::BadRequest("Quote has no volume estimate".into()))?;

    let distance = quote.distance_km.unwrap_or(0.0);

    // 2. Fetch customer
    let customer: CustomerRow =
        sqlx::query_as("SELECT id, email, name, phone FROM customers WHERE id = $1")
            .bind(quote.customer_id)
            .fetch_optional(db)
            .await?
            .ok_or_else(|| ApiError::NotFound("Customer not found".into()))?;

    // 3. Fetch addresses
    let origin: Option<AddressRow> = if let Some(addr_id) = quote.origin_address_id {
        sqlx::query_as("SELECT id, street, city, postal_code, floor, elevator FROM addresses WHERE id = $1")
            .bind(addr_id)
            .fetch_optional(db)
            .await?
    } else {
        None
    };

    let destination: Option<AddressRow> = if let Some(addr_id) = quote.destination_address_id {
        sqlx::query_as("SELECT id, street, city, postal_code, floor, elevator FROM addresses WHERE id = $1")
            .bind(addr_id)
            .fetch_optional(db)
            .await?
    } else {
        None
    };

    // 4. Fetch latest volume estimation for detected items
    let estimation: Option<VolumeEstimationRow> = sqlx::query_as(
        r#"
        SELECT result_data, source_data, total_volume_m3, method
        FROM volume_estimations
        WHERE quote_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(quote_id)
    .fetch_optional(db)
    .await?;

    // 5. Parse detected items from result_data
    let detected_items = parse_detected_items(estimation.as_ref());

    // 6. Calculate pricing
    let origin_floor = origin
        .as_ref()
        .and_then(|a| a.floor.as_deref())
        .map(parse_floor);
    let dest_floor = destination
        .as_ref()
        .and_then(|a| a.floor.as_deref())
        .map(parse_floor);

    let pricing_input = PricingInput {
        volume_m3: volume,
        distance_km: distance,
        preferred_date: quote.preferred_date,
        floor_origin: origin_floor,
        floor_destination: dest_floor,
        has_elevator_origin: origin.as_ref().and_then(|a| a.elevator),
        has_elevator_destination: destination.as_ref().and_then(|a| a.elevator),
    };

    let pricing_engine = PricingEngine::new();
    let mut pricing_result = pricing_engine.calculate(&pricing_input);

    // Apply overrides
    if let Some(p) = overrides.persons {
        pricing_result.estimated_helpers = p;
    }
    if let Some(h) = overrides.hours {
        pricing_result.estimated_hours = h;
    }
    if let Some(price) = overrides.price_cents {
        pricing_result.total_price_cents = price;
    }

    // 7. Build line items from quote notes (services, parking bans) and pricing
    let line_items = build_line_items(
        quote.notes.as_deref(),
        distance,
        volume,
    );

    // Determine rate: if price was overridden, back-calculate so xlsx formula matches.
    // The xlsx netto total (G44) = labor (G38) + sum of other line items.
    // G38 = hours × rate × persons.
    // So: rate = (target_netto - other_items_netto) / (persons × hours)
    let rate_override = if let Some(r) = overrides.rate {
        r
    } else if overrides.price_cents.is_some() {
        let persons = pricing_result.estimated_helpers.max(1) as f64;
        let hours = pricing_result.estimated_hours.max(1.0);
        let target_netto = pricing_result.total_price_cents as f64 / 100.0;

        // Sum up non-labor line items that contribute to the XLSX netto total
        let other_items_netto: f64 = line_items
            .iter()
            .filter(|li| li.row != 38) // exclude labor row
            .map(|li| li.quantity * li.unit_price)
            .sum();

        let labor_netto = (target_netto - other_items_netto).max(0.0);
        labor_netto / (persons * hours)
    } else {
        30.0
    };

    // 8. Build OfferData
    let offer_id = Uuid::now_v7();
    let now = chrono::Utc::now();
    let today = now.date_naive();

    let customer_name = customer
        .name
        .clone()
        .unwrap_or_else(|| customer.email.clone());

    // Detect salutation (Herr/Frau) and greeting from customer name
    let (customer_salutation, greeting) = detect_salutation_and_greeting(&customer_name);

    let moving_date = quote
        .preferred_date
        .map(|d| d.format("%d.%m.%Y").to_string())
        .unwrap_or_else(|| "nach Vereinbarung".to_string());

    let origin_street = origin.as_ref().map(|a| a.street.clone()).unwrap_or_default();
    let origin_city = origin
        .as_ref()
        .map(|a| format_city(a))
        .unwrap_or_default();
    let origin_floor_info = origin
        .as_ref()
        .and_then(|a| a.floor.clone())
        .unwrap_or_default();

    let dest_street = destination
        .as_ref()
        .map(|a| a.street.clone())
        .unwrap_or_default();
    let dest_city = destination
        .as_ref()
        .map(|a| format_city(a))
        .unwrap_or_default();
    let dest_floor_info = destination
        .as_ref()
        .and_then(|a| a.floor.clone())
        .unwrap_or_default();

    let (seq_val,): (i64,) = sqlx::query_as("SELECT nextval('offer_number_seq')")
        .fetch_one(db)
        .await?;
    let offer_number = format!("{}-{:04}", today.format("%Y"), seq_val);

    // Extract services and customer message from notes
    let (services_str, customer_message) = extract_services_and_message(quote.notes.as_deref());

    let valid_until_date =
        valid_days.map(|days| (now + chrono::Duration::days(days)).date_naive());

    let offer_data = OfferData {
        offer_number: offer_number.clone(),
        date: today,
        valid_until: valid_until_date,
        customer_salutation,
        customer_name: customer_name.clone(),
        customer_street: origin_street.clone(),
        customer_city: origin_city.clone(),
        customer_phone: customer.phone.clone().unwrap_or_default(),
        customer_email: customer.email.clone(),
        greeting,
        moving_date: moving_date.clone(),
        origin_street: origin_street.clone(),
        origin_city: origin_city.clone(),
        origin_floor_info: origin_floor_info.clone(),
        dest_street: dest_street.clone(),
        dest_city: dest_city.clone(),
        dest_floor_info: dest_floor_info.clone(),
        volume_m3: volume,
        persons: pricing_result.estimated_helpers,
        estimated_hours: pricing_result.estimated_hours,
        rate_per_person_hour: rate_override,
        line_items,
        detected_items: detected_items.clone(),
    };

    // 8. Generate XLSX (direct XML manipulation of template)
    let xlsx_bytes = generate_offer_xlsx(&offer_data)
        .map_err(|e| ApiError::Internal(format!("XLSX generation error: {e}")))?;

    // 9. Try PDF conversion (LibreOffice), fall back to xlsx if not available
    let (s3_key, pdf_bytes) =
        match convert_xlsx_to_pdf(&xlsx_bytes).await {
            Ok(pdf_bytes) => {
                let key = format!("offers/{offer_id}/angebot.pdf");
                storage
                    .upload(&key, bytes::Bytes::from(pdf_bytes.clone()), "application/pdf")
                    .await
                    .map_err(|e| ApiError::Internal(format!("Failed to upload offer: {e}")))?;
                (key, pdf_bytes)
            }
            Err(e) => {
                tracing::warn!("PDF conversion unavailable ({e}), uploading xlsx directly");
                let key = format!("offers/{offer_id}/angebot.xlsx");
                let bytes = xlsx_bytes.clone();
                storage
                    .upload(
                        &key,
                        bytes::Bytes::from(bytes.clone()),
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    )
                    .await
                    .map_err(|e| ApiError::Internal(format!("Failed to upload offer: {e}")))?;
                (key, bytes)
            }
        };

    // 10. Insert offer record
    let valid_until =
        valid_days.map(|days| (now + chrono::Duration::days(days)).date_naive());

    // Serialize line items for storage
    let line_items_json = serde_json::to_value(&offer_data.line_items).ok();
    let rate_cents = (rate_override * 100.0).round() as i64;

    let row: OfferRow = sqlx::query_as(
        r#"
        INSERT INTO offers (id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at,
                            offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        RETURNING id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at,
                  offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json
        "#,
    )
    .bind(offer_id)
    .bind(quote_id)
    .bind(pricing_result.total_price_cents)
    .bind("EUR")
    .bind(valid_until)
    .bind(Some(&s3_key))
    .bind(OfferStatus::Draft.as_str())
    .bind(now)
    .bind(&offer_number)
    .bind(pricing_result.estimated_helpers as i32)
    .bind(pricing_result.estimated_hours)
    .bind(rate_cents)
    .bind(&line_items_json)
    .fetch_one(db)
    .await?;

    // Update quote status
    sqlx::query("UPDATE quotes SET status = $1, updated_at = $2 WHERE id = $3")
        .bind("offer_generated")
        .bind(now)
        .bind(quote_id)
        .execute(db)
        .await?;

    // Build full address strings for Telegram summary
    let origin_full = if origin_street.is_empty() {
        String::new()
    } else {
        format!("{}, {}", origin_street, origin_city)
    };
    let dest_full = if dest_street.is_empty() {
        String::new()
    } else {
        format!("{}, {}", dest_street, dest_city)
    };

    let summary = TelegramSummary {
        customer_phone: customer.phone.clone().unwrap_or_default(),
        origin_address: origin_full,
        origin_floor: origin_floor_info,
        origin_elevator: origin.as_ref().and_then(|a| a.elevator),
        dest_address: dest_full,
        dest_floor: dest_floor_info,
        dest_elevator: destination.as_ref().and_then(|a| a.elevator),
        preferred_date: moving_date,
        volume_m3: volume,
        items_count: detected_items.len(),
        distance_km: distance,
        services: services_str,
        persons: pricing_result.estimated_helpers,
        hours: pricing_result.estimated_hours,
        rate: rate_override,
        netto_cents: pricing_result.total_price_cents,
        customer_message,
    };

    Ok(GeneratedOffer {
        offer: Offer::from(row),
        pdf_bytes,
        customer_email: customer.email,
        customer_name,
        summary,
    })
}

/// Extract recognized services and any remaining free-text customer message from quote notes.
///
/// Notes format: "Halteverbot Auszug, Verpackungsservice, Montage, Bitte vorsichtig mit dem Klavier"
/// Known service keywords are extracted; everything else is the customer message.
fn extract_services_and_message(notes: Option<&str>) -> (String, String) {
    let Some(notes) = notes else {
        return (String::new(), String::new());
    };

    let known_services = [
        "halteverbot auszug",
        "halteverbot einzug",
        "verpackungsservice",
        "einpackservice",
        "montage",
        "demontage",
        "einlagerung",
        "entsorgung",
    ];

    let known_prefixes = ["auszug:", "einzug:"];

    let mut services = Vec::new();
    let mut message_parts = Vec::new();

    for part in notes.split(", ") {
        let lower = part.trim().to_lowercase();
        let is_service = known_services.iter().any(|s| lower == *s)
            || known_prefixes.iter().any(|p| lower.starts_with(p));
        if is_service {
            services.push(part.trim().to_string());
        } else if !part.trim().is_empty() {
            message_parts.push(part.trim().to_string());
        }
    }

    (services.join(", "), message_parts.join(", "))
}

fn format_city(addr: &AddressRow) -> String {
    format!(
        "{}{}",
        addr.postal_code
            .as_ref()
            .map(|p| format!("{p} "))
            .unwrap_or_default(),
        addr.city
    )
}

/// Detect the appropriate salutation and greeting from the customer name.
///
/// Returns (salutation for address block, greeting line).
/// Uses common German female first names as a heuristic.
fn detect_salutation_and_greeting(name: &str) -> (String, String) {
    // If the name contains "Frau" or "Herr" prefix, use that directly
    let name_trimmed = name.trim();
    if name_trimmed.starts_with("Frau ") {
        let after = name_trimmed.strip_prefix("Frau ").unwrap().trim();
        return (
            "Frau".to_string(),
            format!("Sehr geehrte Frau {after},"),
        );
    }
    if name_trimmed.starts_with("Herr ") {
        let after = name_trimmed.strip_prefix("Herr ").unwrap().trim();
        return (
            "Herrn".to_string(),
            format!("Sehr geehrter Herr {after},"),
        );
    }

    // Extract first name (first word)
    let first_name = name_trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    // Extract last name (last word) for greeting
    let last_name = name_trimmed
        .split_whitespace()
        .last()
        .unwrap_or(name_trimmed);

    // Common German/Austrian female first names
    const FEMALE_NAMES: &[&str] = &[
        "anna", "andrea", "angelika", "anita", "barbara", "birgit", "brigitte",
        "carina", "carmen", "caroline", "charlotte", "christa", "christina", "claudia",
        "daniela", "diana", "doris", "elisabeth", "elena", "elke", "emma", "erika",
        "eva", "franziska", "gabriele", "gabi", "gertrud", "gisela", "hannah",
        "heidi", "helga", "ines", "ingrid", "irene", "jana", "jessica", "johanna",
        "julia", "karin", "katharina", "katrin", "kristina", "laura", "lena", "lisa",
        "luisa", "manuela", "maria", "marie", "marina", "marion", "marlene",
        "martina", "melanie", "michaela", "monika", "nadine", "natalie", "nicole",
        "nina", "olivia", "patricia", "petra", "renate", "rita", "rosa", "ruth",
        "sabine", "sandra", "sara", "sarah", "silvia", "simone", "sofia", "sophie",
        "stefanie", "stephanie", "susanne", "sylvia", "tanja", "teresa", "theresia",
        "ursula", "ute", "valentina", "vanessa", "vera", "verena", "veronika",
    ];

    let is_female = FEMALE_NAMES.contains(&first_name.as_str());

    if name_trimmed.contains(' ') {
        // Have first + last name
        if is_female {
            (
                "Frau".to_string(),
                format!("Sehr geehrte Frau {last_name},"),
            )
        } else {
            (
                "Herrn".to_string(),
                format!("Sehr geehrter Herr {last_name},"),
            )
        }
    } else {
        // Only one word — can't determine reliably, use generic
        (
            String::new(),
            "Sehr geehrte Damen und Herren,".to_string(),
        )
    }
}

/// Build line items for the XLSX offer from quote notes, distance, and pricing.
///
/// Template row mapping:
///   31: De/Montage            — qty × €50
///   32: Halteverbotszone      — qty × €100 (per location)
///   33: Umzugsmaterial        — 1 × €30 (template default, override if Einpackservice)
///   34: Seidenpapier          — qty × €5 (if packing service)
///   35: U-Karton              — qty × €2.10 (if packing service)
///   37: Kleiderboxen          — qty × €10 (if packing service)
///   38: Personal              — hours × rate × persons (handled separately in xlsx.rs)
///   39: 3,5t Transporter      — qty × €60 (based on volume)
///   42: Anfahrt/Abfahrt       — 1 × distance-based price
fn build_line_items(
    notes: Option<&str>,
    distance_km: f64,
    volume_m3: f64,
) -> Vec<OfferLineItem> {
    let notes_lower = notes
        .map(|n| n.to_lowercase())
        .unwrap_or_default();
    let mut items = Vec::new();

    // Row 31: De/Montage — if assembly or disassembly service requested
    let has_montage = notes_lower.contains("montage") || notes_lower.contains("demontage");
    if has_montage {
        items.push(OfferLineItem {
            row: 31,
            description: None,
            quantity: 1.0,
            unit_price: 50.0, // template preset: €50 per unit
        });
    }

    // Row 32: Halteverbotszone — count parking ban locations
    let mut halteverbot_count = 0.0;
    if notes_lower.contains("halteverbot auszug") {
        halteverbot_count += 1.0;
    }
    if notes_lower.contains("halteverbot einzug") {
        halteverbot_count += 1.0;
    }
    if halteverbot_count > 0.0 {
        let desc = if halteverbot_count > 1.0 {
            Some("Beladestelle + Entladestelle".to_string())
        } else if notes_lower.contains("halteverbot auszug") {
            Some("Beladestelle".to_string())
        } else {
            None // template default says "Entladestelle"
        };
        items.push(OfferLineItem {
            row: 32,
            description: desc,
            quantity: halteverbot_count,
            unit_price: 100.0,
        });
    }

    // Row 33: Umzugsmaterial — if Verpackungsservice/Einpackservice, note it
    if notes_lower.contains("verpackungsservice") || notes_lower.contains("einpackservice") {
        items.push(OfferLineItem {
            row: 33,
            description: Some("Umzugsmaterial inkl. Einpackservice (nach Aufwand)".to_string()),
            quantity: 1.0,
            unit_price: 30.0, // template preset: €30 per unit
        });
    }

    // Row 39: Transporter — based on volume (2 trucks for >30m³)
    let truck_count = if volume_m3 > 30.0 { 2.0 } else { 1.0 };
    items.push(OfferLineItem {
        row: 39,
        description: None,
        quantity: truck_count,
        unit_price: 60.0,
    });

    // Row 42: Anfahrt/Abfahrt — adjust based on distance
    if distance_km > 0.0 {
        // Price scales with distance: base €30 + €1.50/km
        let anfahrt_price = 30.0 + (distance_km * 1.5);
        items.push(OfferLineItem {
            row: 42,
            description: None,
            quantity: 1.0,
            unit_price: anfahrt_price,
        });
    }

    items
}

// --- API handlers ---

async fn generate_offer(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateOfferRequest>,
) -> Result<Json<Offer>, ApiError> {
    let overrides = OfferOverrides {
        price_cents: request.price_cents_netto,
        persons: request.persons,
        hours: request.hours,
        rate: request.rate,
    };
    let result = build_offer_with_overrides(
        &state.db, &*state.storage, request.quote_id, request.valid_days, &overrides
    ).await?;
    Ok(Json(result.offer))
}

async fn get_offer(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Offer>, ApiError> {
    let row: Option<OfferRow> = sqlx::query_as(
        r#"
        SELECT id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at,
               offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json
        FROM offers WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Offer {id} not found")))?;
    Ok(Json(Offer::from(row)))
}

async fn get_offer_pdf(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let row: Option<OfferRow> = sqlx::query_as(
        r#"
        SELECT id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at,
               offer_number, persons, hours_estimated, rate_per_hour_cents, line_items_json
        FROM offers WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let offer =
        Offer::from(row.ok_or_else(|| ApiError::NotFound(format!("Offer {id} not found")))?);

    let storage_key = offer
        .pdf_storage_key
        .ok_or_else(|| ApiError::NotFound("Offer has no generated file".into()))?;

    let file_bytes = state
        .storage
        .download(&storage_key)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to download offer: {e}")))?;

    let (content_type, ext) = if storage_key.ends_with(".pdf") {
        ("application/pdf", "pdf")
    } else {
        ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx")
    };
    let filename = format!("Angebot-{}.{ext}", offer.id);

    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        file_bytes,
    ))
}

/// Parsed inventory item from VolumeCalculator items_list text.
/// Matches the format stored by orchestrator::parse_items_list_text().
#[derive(Debug, Clone, serde::Deserialize)]
struct ParsedInventoryItem {
    name: String,
    quantity: u32,
    volume_m3: f64,
}

/// Parse detected items from volume estimation result_data.
pub fn parse_detected_items(estimation: Option<&VolumeEstimationRow>) -> Vec<DetectedItemRow> {
    let Some(est) = estimation else {
        return vec![];
    };
    let Some(data) = &est.result_data else {
        return vec![];
    };

    // Try DepthSensorResult (has detected_items with dimensions)
    if let Ok(result) = serde_json::from_value::<DepthSensorResult>(data.clone()) {
        return result
            .detected_items
            .into_iter()
            .map(detected_item_to_row)
            .collect();
    }

    // Try VisionAnalysisResult (LLM, array of results)
    if let Ok(results) = serde_json::from_value::<Vec<VisionAnalysisResult>>(data.clone()) {
        return results
            .into_iter()
            .flat_map(|r| r.detected_items)
            .map(detected_item_to_row)
            .collect();
    }

    // Try single VisionAnalysisResult
    if let Ok(result) = serde_json::from_value::<VisionAnalysisResult>(data.clone()) {
        return result
            .detected_items
            .into_iter()
            .map(detected_item_to_row)
            .collect();
    }

    // Try raw Vec<DetectedItem> (handles both old vision and depth_sensor arrays)
    if let Ok(items) = serde_json::from_value::<Vec<DetectedItem>>(data.clone()) {
        return items.into_iter().map(detected_item_to_row).collect();
    }

    // Try parsed inventory items (from VolumeCalculator items_list text)
    if let Ok(items) = serde_json::from_value::<Vec<ParsedInventoryItem>>(data.clone()) {
        return items
            .into_iter()
            .map(|item| {
                let name = if item.quantity > 1 {
                    format!("{}x {}", item.quantity, item.name)
                } else {
                    item.name
                };
                DetectedItemRow {
                    name,
                    volume_m3: item.volume_m3, // already total volume for this line
                    dimensions: None,
                    confidence: 0.8, // form-submitted data has decent confidence
                    german_name: None,
                    re_value: None,
                    volume_source: None,
                    crop_s3_key: None,
                    bbox: None,
                    bbox_image_index: None,
                    source_image_urls: None,
                }
            })
            .collect();
    }

    vec![]
}

/// Map English detection labels to German Umzugsgutliste names.
/// Mirrors the RE_CATALOG in services/vision/app/models/schemas.py.
pub fn label_to_german(label: &str) -> Option<&'static str> {
    match label.to_lowercase().as_str() {
        // Seating
        "sofa" | "couch" => Some("Sofa, Couch, Liege"),
        "armchair" | "recliner" => Some("Sessel mit Armlehnen"),
        "chair" | "stool" => Some("Stuhl"),
        "bench" => Some("Eckbank"),
        "ottoman" => Some("Ottoman"),
        "bar stool" => Some("Stuhl mit Armlehnen"),
        "office chair" => Some("Bürostuhl"),
        // Tables
        "table" => Some("Tisch"),
        "desk" => Some("Schreibtisch"),
        "dining table" => Some("Esstisch"),
        "coffee table" => Some("Couchtisch"),
        "kitchen island" => Some("Winkelkombination"),
        // Beds
        "bed" => Some("Bett"),
        "mattress" => Some("Matratze"),
        "crib" => Some("Kinderbett"),
        "bunk bed" => Some("Etagenbett"),
        // Storage
        "wardrobe" | "closet" => Some("Schrank"),
        "dresser" | "chest of drawers" | "cabinet" => Some("Kommode"),
        "shelf" => Some("Regal"),
        "bookshelf" => Some("Bücherregal"),
        "cupboard" => Some("Wohnzimmerschrank"),
        "nightstand" => Some("Nachttisch"),
        "shoe rack" => Some("Schuhschrank"),
        "coat rack" => Some("Kleiderablage"),
        // Electronics
        "tv" | "television" => Some("Fernseher"),
        "monitor" => Some("Monitor"),
        "computer" => Some("Computer"),
        "laptop" => Some("Laptop"),
        "printer" => Some("Tischkopierer"),
        "speaker" | "stereo" => Some("Stereoanlage"),
        "lamp" => Some("Deckenlampe"),
        "floor lamp" => Some("Stehlampe"),
        "chandelier" => Some("Lüster"),
        // Appliances
        "refrigerator" | "fridge" => Some("Kühlschrank"),
        "freezer" => Some("Gefrierschrank"),
        "washing machine" => Some("Waschmaschine"),
        "dryer" => Some("Trockner"),
        "dishwasher" => Some("Geschirrspülmaschine"),
        "oven" | "stove" => Some("Herd"),
        "microwave" => Some("Mikrowelle"),
        "vacuum cleaner" => Some("Staubsauger"),
        "fan" => Some("Ventilator"),
        "heater" => Some("Heizgerät"),
        // Boxes
        "box" | "carton" | "moving box" => Some("Umzugskarton"),
        "basket" => Some("Korb"),
        "storage container" => Some("Umzugskarton groß"),
        // Children
        "stroller" => Some("Kinderwagen"),
        // Luggage
        "suitcase" => Some("Koffer"),
        "bag" => Some("Tasche"),
        // Sports
        "bicycle" | "bike" => Some("Fahrrad"),
        "treadmill" => Some("Laufband"),
        "exercise equipment" => Some("Sportgerät"),
        // Instruments
        "piano" => Some("Klavier"),
        "keyboard" => Some("Keyboard"),
        "guitar" => Some("Gitarre"),
        // Misc
        "plant" => Some("Pflanze"),
        "painting" => Some("Bild"),
        "mirror" => Some("Spiegel"),
        "rug" | "carpet" => Some("Teppich"),
        "curtain" => Some("Vorhang"),
        "ironing board" => Some("Bügelbrett"),
        _ => None,
    }
}

fn detected_item_to_row(item: DetectedItem) -> DetectedItemRow {
    let german_name = item.german_name.or_else(|| label_to_german(&item.name).map(String::from));
    DetectedItemRow {
        name: item.name,
        volume_m3: item.volume_m3,
        dimensions: item.dimensions.map(|d| {
            format!("{:.1} × {:.1} × {:.1} m", d.length_m, d.width_m, d.height_m)
        }),
        confidence: item.confidence,
        german_name,
        re_value: item.re_value,
        volume_source: item.volume_source,
        crop_s3_key: item.crop_s3_key,
        bbox: item.bbox,
        bbox_image_index: item.bbox_image_index,
        source_image_urls: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vol_est(result_data: serde_json::Value) -> VolumeEstimationRow {
        VolumeEstimationRow {
            result_data: Some(result_data),
            source_data: None,
            total_volume_m3: Some(10.0),
            method: "depth_sensor".to_string(),
        }
    }

    #[test]
    fn parse_depth_sensor_result_data() {
        let json = serde_json::json!({
            "detected_items": [
                {
                    "name": "Sofa",
                    "volume_m3": 1.2,
                    "confidence": 0.85,
                    "dimensions": {"length_m": 2.0, "width_m": 0.9, "height_m": 0.8},
                    "category": "seating"
                }
            ],
            "total_volume_m3": 1.2,
            "confidence_score": 0.85,
            "processing_time_ms": 5000
        });
        let est = make_vol_est(json);
        let items = parse_detected_items(Some(&est));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Sofa");
        assert!((items[0].volume_m3 - 1.2).abs() < 0.001);
        assert!((items[0].confidence - 0.85).abs() < 0.001);
        assert!(items[0].dimensions.is_some());
    }

    #[test]
    fn parse_vision_llm_result_data() {
        let json = serde_json::json!({
            "detected_items": [
                {"name": "Tisch", "estimated_volume_m3": 0.5, "confidence": 0.7}
            ],
            "total_volume_m3": 0.5,
            "confidence_score": 0.7,
            "room_type": "living_room"
        });
        let est = make_vol_est(json);
        let items = parse_detected_items(Some(&est));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Tisch");
        assert!((items[0].volume_m3 - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_vision_llm_array_result_data() {
        let json = serde_json::json!([
            {
                "detected_items": [
                    {"name": "Schrank", "estimated_volume_m3": 2.0, "confidence": 0.9}
                ],
                "total_volume_m3": 2.0,
                "confidence_score": 0.9
            }
        ]);
        let est = make_vol_est(json);
        let items = parse_detected_items(Some(&est));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Schrank");
    }

    #[test]
    fn parse_empty_result_data() {
        let est = VolumeEstimationRow {
            result_data: None,
            source_data: None,
            total_volume_m3: None,
            method: "vision".to_string(),
        };
        let items = parse_detected_items(Some(&est));
        assert!(items.is_empty());
    }

    #[test]
    fn parse_no_estimation() {
        let items = parse_detected_items(None);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_inventory_items() {
        let json = serde_json::json!([
            {"name": "Sofa", "quantity": 2, "volume_m3": 1.6}
        ]);
        let est = make_vol_est(json);
        let items = parse_detected_items(Some(&est));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "2x Sofa");
        assert!((items[0].volume_m3 - 1.6).abs() < 0.001);
    }

    #[test]
    fn depth_sensor_item_german_name_lookup() {
        let json = serde_json::json!({
            "detected_items": [
                {"name": "sofa", "volume_m3": 1.0, "confidence": 0.9}
            ],
            "total_volume_m3": 1.0,
            "confidence_score": 0.9,
            "processing_time_ms": 3000
        });
        let est = make_vol_est(json);
        let items = parse_detected_items(Some(&est));
        assert_eq!(items[0].german_name.as_deref(), Some("Sofa, Couch, Liege"));
    }
}
