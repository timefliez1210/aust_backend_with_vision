use axum::{
    extract::{Path, Query, State},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, AppState};
use aust_core::models::{CreateQuote, Quote, QuoteStatus, UpdateQuote};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(create_quote).get(list_quotes))
        .route("/{id}", get(get_quote).patch(update_quote))
}

#[derive(Debug, Deserialize)]
struct ListQuotesQuery {
    status: Option<String>,
    customer_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
struct QuoteListResponse {
    quotes: Vec<Quote>,
    total: i64,
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

async fn create_quote(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateQuote>,
) -> Result<Json<Quote>, ApiError> {
    let id = Uuid::now_v7();
    let now = chrono::Utc::now();

    let row: QuoteRow = sqlx::query_as(
        r#"
        INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id, status, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        "#
    )
    .bind(id)
    .bind(request.customer_id)
    .bind(request.origin_address_id)
    .bind(request.destination_address_id)
    .bind(QuoteStatus::Pending.as_str())
    .bind(request.preferred_date)
    .bind(&request.notes)
    .bind(now)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(Quote::from(row)))
}

async fn get_quote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Quote>, ApiError> {
    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes WHERE id = $1
        "#
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Quote {id} not found")))?;
    Ok(Json(Quote::from(row)))
}

async fn list_quotes(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListQuotesQuery>,
) -> Result<Json<QuoteListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);

    let rows: Vec<QuoteRow> = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        FROM quotes
        WHERE ($1::uuid IS NULL OR customer_id = $1)
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#
    )
    .bind(query.customer_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM quotes WHERE ($1::uuid IS NULL OR customer_id = $1)"
    )
    .bind(query.customer_id)
    .fetch_one(&state.db)
    .await?;

    let quotes: Vec<Quote> = rows.into_iter().map(Quote::from).collect();

    Ok(Json(QuoteListResponse { quotes, total: total.0 }))
}

async fn update_quote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateQuote>,
) -> Result<Json<Quote>, ApiError> {
    let now = chrono::Utc::now();

    let row: Option<QuoteRow> = sqlx::query_as(
        r#"
        UPDATE quotes SET
            origin_address_id = COALESCE($2, origin_address_id),
            destination_address_id = COALESCE($3, destination_address_id),
            status = COALESCE($4, status),
            estimated_volume_m3 = COALESCE($5, estimated_volume_m3),
            distance_km = COALESCE($6, distance_km),
            preferred_date = COALESCE($7, preferred_date),
            notes = COALESCE($8, notes),
            updated_at = $9
        WHERE id = $1
        RETURNING id, customer_id, origin_address_id, destination_address_id, status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at
        "#
    )
    .bind(id)
    .bind(request.origin_address_id)
    .bind(request.destination_address_id)
    .bind(request.status.map(|s| s.as_str()))
    .bind(request.estimated_volume_m3)
    .bind(request.distance_km)
    .bind(request.preferred_date)
    .bind(&request.notes)
    .bind(now)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or_else(|| ApiError::NotFound(format!("Quote {id} not found")))?;
    Ok(Json(Quote::from(row)))
}
