use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, AppState};
use aust_core::models::{Offer, OfferStatus, PricingInput, Quote, QuoteStatus};
use aust_offer_generator::{PdfGenerator, PricingEngine};

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
}

#[derive(Debug, FromRow)]
struct QuoteRow {
    id: Uuid,
    customer_id: Uuid,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
    status: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    preferred_date: Option<chrono::DateTime<chrono::Utc>>,
    notes: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<QuoteRow> for Quote {
    fn from(row: QuoteRow) -> Self {
        let status = match row.status.as_str() {
            "pending" => QuoteStatus::Pending,
            "info_requested" => QuoteStatus::InfoRequested,
            "volume_estimated" => QuoteStatus::VolumeEstimated,
            "offer_generated" => QuoteStatus::OfferGenerated,
            "offer_sent" => QuoteStatus::OfferSent,
            "accepted" => QuoteStatus::Accepted,
            "rejected" => QuoteStatus::Rejected,
            "expired" => QuoteStatus::Expired,
            _ => QuoteStatus::Pending,
        };

        Quote {
            id: row.id,
            customer_id: row.customer_id,
            origin_address_id: row.origin_address_id,
            destination_address_id: row.destination_address_id,
            status,
            estimated_volume_m3: row.estimated_volume_m3,
            distance_km: row.distance_km,
            preferred_date: row.preferred_date,
            notes: row.notes,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
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
        }
    }
}

async fn generate_offer(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateOfferRequest>,
) -> Result<Json<Offer>, ApiError> {
    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes WHERE id = $1
        "#
    )
    .bind(request.quote_id)
    .fetch_optional(&state.db)
    .await?;

    let quote = Quote::from(row.ok_or_else(|| ApiError::NotFound("Quote not found".into()))?);

    let volume = quote
        .estimated_volume_m3
        .ok_or_else(|| ApiError::BadRequest("Quote has no volume estimate".into()))?;

    let distance = quote
        .distance_km
        .ok_or_else(|| ApiError::BadRequest("Quote has no distance calculated".into()))?;

    let pricing_input = PricingInput {
        volume_m3: volume,
        distance_km: distance,
        preferred_date: quote.preferred_date,
        floor_origin: None,
        floor_destination: None,
        has_elevator_origin: None,
        has_elevator_destination: None,
    };

    let pricing_engine = PricingEngine::new();
    let pricing_result = pricing_engine.calculate(&pricing_input);

    let valid_until = request.valid_days.map(|days| {
        (chrono::Utc::now() + chrono::Duration::days(days))
            .date_naive()
    });

    let id = Uuid::now_v7();
    let now = chrono::Utc::now();

    let row: OfferRow = sqlx::query_as(
        r#"
        INSERT INTO offers (id, quote_id, price_cents, currency, valid_until, status, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at
        "#
    )
    .bind(id)
    .bind(request.quote_id)
    .bind(pricing_result.total_price_cents)
    .bind("EUR")
    .bind(valid_until)
    .bind(OfferStatus::Draft.as_str())
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // Update quote status
    sqlx::query(
        "UPDATE quotes SET status = $1, updated_at = $2 WHERE id = $3"
    )
    .bind("offer_generated")
    .bind(now)
    .bind(request.quote_id)
    .execute(&state.db)
    .await?;

    Ok(Json(Offer::from(row)))
}

async fn get_offer(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Offer>, ApiError> {
    let row: Option<OfferRow> = sqlx::query_as(
        r#"
        SELECT id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at
        FROM offers WHERE id = $1
        "#
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
        SELECT id, quote_id, price_cents, currency, valid_until, pdf_storage_key, status, created_at, sent_at
        FROM offers WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let offer = Offer::from(row.ok_or_else(|| ApiError::NotFound(format!("Offer {id} not found")))?);

    let content = format!(
        "Angebot #{}\nPreis: {:.2} EUR\nGültig bis: {}",
        offer.id,
        offer.price_cents as f64 / 100.0,
        offer
            .valid_until
            .map(|d| d.to_string())
            .unwrap_or_else(|| "Unbegrenzt".to_string())
    );

    let pdf_generator = PdfGenerator::new();
    let pdf_bytes = pdf_generator
        .generate(&content)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"offer.pdf\"",
            ),
        ],
        pdf_bytes,
    ))
}
