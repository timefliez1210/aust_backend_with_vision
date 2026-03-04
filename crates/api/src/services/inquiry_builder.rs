//! Single source of truth for building `InquiryResponse` and `InquiryListItem`
//! from the database, replacing duplicate implementations in quotes.rs, admin.rs,
//! and customer.rs.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use aust_core::models::{
    AddressSnapshot, CustomerSnapshot, EstimationSnapshot, InquiryListItem,
    InquiryResponse, InquiryStatus, ItemSnapshot, LineItemSnapshot, OfferSnapshot,
    Services,
};

use crate::routes::offers::{parse_detected_items, VolumeEstimationRow};
use crate::ApiError;

// ---------------------------------------------------------------------------
// Internal DB row types
// ---------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct InquiryDbRow {
    id: Uuid,
    customer_id: Uuid,
    origin_address_id: Option<Uuid>,
    destination_address_id: Option<Uuid>,
    #[sqlx(default)]
    stop_address_id: Option<Uuid>,
    status: String,
    estimated_volume_m3: Option<f64>,
    distance_km: Option<f64>,
    preferred_date: Option<DateTime<Utc>>,
    notes: Option<String>,
    #[sqlx(default)]
    services: serde_json::Value,
    #[sqlx(default)]
    source: String,
    #[sqlx(default)]
    offer_sent_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    accepted_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct CustomerDbRow {
    id: Uuid,
    email: String,
    name: Option<String>,
    phone: Option<String>,
}

#[derive(Debug, FromRow)]
struct AddressDbRow {
    id: Uuid,
    street: String,
    city: String,
    postal_code: Option<String>,
    #[sqlx(default)]
    country: String,
    floor: Option<String>,
    elevator: Option<bool>,
    #[sqlx(default)]
    latitude: Option<f64>,
    #[sqlx(default)]
    longitude: Option<f64>,
}

#[derive(Debug, FromRow)]
struct EstimationDbRow {
    id: Uuid,
    method: String,
    status: String,
    total_volume_m3: Option<f64>,
    #[sqlx(default)]
    confidence_score: Option<f64>,
    result_data: Option<serde_json::Value>,
    source_data: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct OfferDbRow {
    id: Uuid,
    #[sqlx(default)]
    offer_number: Option<String>,
    price_cents: i64,
    status: String,
    persons: Option<i32>,
    hours_estimated: Option<f64>,
    rate_per_hour_cents: Option<i64>,
    line_items_json: Option<serde_json::Value>,
    pdf_storage_key: Option<String>,
    #[sqlx(default)]
    valid_until: Option<chrono::NaiveDate>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct ListItemDbRow {
    id: Uuid,
    customer_name: Option<String>,
    customer_email: String,
    origin_city: Option<String>,
    destination_city: Option<String>,
    volume_m3: Option<f64>,
    distance_km: Option<f64>,
    status: String,
    has_offer: bool,
    offer_status: Option<String>,
    created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a complete `InquiryResponse` from the database.
///
/// **Caller**: `GET /api/v1/inquiries/{id}`, `GET /api/v1/customer/inquiries/{id}`,
///             admin detail views
/// **Why**: Single source of truth for inquiry detail, replaces 3 duplicate
///          implementations.
///
/// # Parameters
/// - `pool` -- PostgreSQL connection pool
/// - `inquiry_id` -- ID of the inquiry to fetch
///
/// # Returns
/// Fully populated `InquiryResponse` with customer, addresses, estimation,
/// items, and latest active offer.
///
/// # Errors
/// - 404 if the inquiry or its customer is not found
/// - 500 on DB failures
pub async fn build_inquiry_response(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<InquiryResponse, ApiError> {
    // 1. Fetch inquiry row
    let row: InquiryDbRow = sqlx::query_as(
        r#"
        SELECT id, customer_id, origin_address_id, destination_address_id, stop_address_id,
               status, estimated_volume_m3, distance_km, preferred_date, notes,
               services, source, offer_sent_at, accepted_at, created_at, updated_at
        FROM inquiries WHERE id = $1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("Inquiry {inquiry_id} not found")))?;

    let status: InquiryStatus = row.status.parse().unwrap_or_default();
    let services: Services = serde_json::from_value(row.services).unwrap_or_default();

    // 2. Fetch customer
    let customer: CustomerDbRow = sqlx::query_as(
        "SELECT id, email, name, phone FROM customers WHERE id = $1",
    )
    .bind(row.customer_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound("Customer not found".into()))?;

    // 3. Fetch addresses
    let origin_address = fetch_address(pool, row.origin_address_id).await?;
    let destination_address = fetch_address(pool, row.destination_address_id).await?;
    let stop_address = fetch_address(pool, row.stop_address_id).await?;

    // 4. Fetch latest completed estimation + items
    let est: Option<EstimationDbRow> = sqlx::query_as(
        r#"
        SELECT id, method, status, total_volume_m3, confidence_score,
               result_data, source_data, created_at
        FROM volume_estimations
        WHERE inquiry_id = $1 AND status = 'completed'
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    let (estimation, items) = if let Some(ref e) = est {
        let source_s3_keys = extract_s3_keys(e.source_data.as_ref());
        let source_images: Vec<String> = source_s3_keys
            .iter()
            .map(|k| format!("/api/v1/estimates/images/{k}"))
            .collect();

        let source_video = e
            .source_data
            .as_ref()
            .and_then(|sd| sd.get("video_s3_key")?.as_str())
            .map(|k| format!("/api/v1/estimates/images/{k}"));

        // Parse detected items
        let vol_est = VolumeEstimationRow {
            result_data: e.result_data.clone(),
            source_data: e.source_data.clone(),
            total_volume_m3: e.total_volume_m3,
            method: e.method.clone(),
        };
        let detected = parse_detected_items(Some(&vol_est));
        let raw_items: Vec<serde_json::Value> = e
            .result_data
            .as_ref()
            .and_then(|rd| serde_json::from_value::<Vec<serde_json::Value>>(rd.clone()).ok())
            .unwrap_or_default();

        let items: Vec<ItemSnapshot> = detected
            .iter()
            .enumerate()
            .map(|(idx, d)| {
                let crop_url = d
                    .crop_s3_key
                    .as_ref()
                    .map(|k| format!("/api/v1/estimates/images/{k}"));
                let source_image_url = d
                    .bbox_image_index
                    .and_then(|i| source_s3_keys.get(i))
                    .map(|k| format!("/api/v1/estimates/images/{k}"));
                let raw = raw_items.get(idx);
                let seen_in_images = raw.and_then(|r| {
                    r.get("seen_in_images")?
                        .as_array()
                        .map(|arr| arr.iter().filter_map(|v| v.as_i64().map(|n| n as i32)).collect())
                });
                let category = raw.and_then(|r| r.get("category")?.as_str().map(String::from));
                let dimensions = raw.and_then(|r| r.get("dimensions").cloned());

                ItemSnapshot {
                    name: d.german_name.clone().unwrap_or_else(|| d.name.clone()),
                    volume_m3: d.volume_m3,
                    quantity: 1,
                    confidence: d.confidence,
                    category,
                    dimensions,
                    crop_url,
                    source_image_url,
                    bbox: d.bbox.clone(),
                    bbox_image_index: d.bbox_image_index.map(|i| i as i32),
                    seen_in_images,
                }
            })
            .collect();

        let item_count = items.len() as i64;

        let estimation = EstimationSnapshot {
            id: e.id,
            method: e.method.clone(),
            status: e.status.clone(),
            total_volume_m3: e.total_volume_m3,
            confidence_score: e.confidence_score,
            item_count,
            source_images,
            source_video,
            created_at: e.created_at,
        };

        (Some(estimation), items)
    } else {
        (None, Vec::new())
    };

    // 5. Fetch latest active offer
    let offer_row: Option<OfferDbRow> = sqlx::query_as(
        r#"
        SELECT id, offer_number, price_cents, status, persons, hours_estimated,
               rate_per_hour_cents, line_items_json, pdf_storage_key, valid_until,
               created_at
        FROM offers
        WHERE inquiry_id = $1 AND status NOT IN ('rejected', 'cancelled')
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await?;

    let offer = offer_row.map(|r| build_offer_snapshot(&r));

    // 6. Extract customer message from notes
    let customer_message = extract_customer_message(row.notes.as_deref());

    // 7. Assemble response
    Ok(InquiryResponse {
        id: row.id,
        status,
        source: if row.source.is_empty() {
            "direct_email".to_string()
        } else {
            row.source
        },
        services,
        volume_m3: row.estimated_volume_m3,
        distance_km: row.distance_km,
        preferred_date: row.preferred_date.map(|d| d.format("%Y-%m-%d").to_string()),
        notes: row.notes,
        customer_message,
        created_at: row.created_at,
        updated_at: row.updated_at,
        offer_sent_at: row.offer_sent_at,
        accepted_at: row.accepted_at,
        customer: Some(CustomerSnapshot {
            id: customer.id,
            name: customer.name,
            email: customer.email,
            phone: customer.phone,
        }),
        origin_address,
        destination_address,
        stop_address,
        estimation,
        items,
        offer,
    })
}

/// Build a paginated list of `InquiryListItem`s with optional filtering.
///
/// **Caller**: `GET /api/v1/inquiries`, admin dashboard list views
/// **Why**: Canonical list query with search, status filter, and offer filter.
///
/// # Parameters
/// - `pool` -- PostgreSQL connection pool
/// - `status` -- optional status filter
/// - `search` -- optional ILIKE search on customer name/email
/// - `has_offer` -- optional filter: true = must have active offer, false = must not
/// - `limit` -- max items per page (capped at 100)
/// - `offset` -- pagination offset
///
/// # Returns
/// `(items, total_count)` tuple for paginated list responses.
///
/// # Errors
/// - 500 on DB failures
pub async fn build_inquiry_list(
    pool: &PgPool,
    status: Option<&str>,
    search: Option<&str>,
    has_offer: Option<bool>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<InquiryListItem>, i64), ApiError> {
    let limit = limit.min(100);
    let search_pattern = search.map(|s| format!("%{s}%"));

    let items: Vec<ListItemDbRow> = sqlx::query_as(
        r#"
        SELECT
            i.id,
            c.name AS customer_name,
            c.email AS customer_email,
            oa.city AS origin_city,
            da.city AS destination_city,
            i.estimated_volume_m3 AS volume_m3,
            i.distance_km,
            i.status,
            EXISTS (
                SELECT 1 FROM offers
                WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
            ) AS has_offer,
            (
                SELECT o2.status FROM offers o2
                WHERE o2.inquiry_id = i.id AND o2.status NOT IN ('rejected', 'cancelled')
                ORDER BY o2.created_at DESC LIMIT 1
            ) AS offer_status,
            i.created_at
        FROM inquiries i
        LEFT JOIN customers c ON i.customer_id = c.id
        LEFT JOIN addresses oa ON i.origin_address_id = oa.id
        LEFT JOIN addresses da ON i.destination_address_id = da.id
        WHERE ($1::text IS NULL OR i.status = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.email ILIKE $2)
          AND ($3::bool IS NULL OR
               (CASE WHEN $3 THEN EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) ELSE NOT EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) END))
        ORDER BY i.created_at DESC
        LIMIT $4 OFFSET $5
        "#,
    )
    .bind(status)
    .bind(&search_pattern)
    .bind(has_offer)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM inquiries i
        LEFT JOIN customers c ON i.customer_id = c.id
        WHERE ($1::text IS NULL OR i.status = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.email ILIKE $2)
          AND ($3::bool IS NULL OR
               (CASE WHEN $3 THEN EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) ELSE NOT EXISTS (
                   SELECT 1 FROM offers WHERE inquiry_id = i.id AND status NOT IN ('rejected', 'cancelled')
               ) END))
        "#,
    )
    .bind(status)
    .bind(&search_pattern)
    .bind(has_offer)
    .fetch_one(pool)
    .await?;

    let result = items
        .into_iter()
        .map(|r| InquiryListItem {
            id: r.id,
            customer_name: r.customer_name,
            customer_email: r.customer_email,
            origin_city: r.origin_city,
            destination_city: r.destination_city,
            volume_m3: r.volume_m3,
            distance_km: r.distance_km,
            status: r.status.parse().unwrap_or_default(),
            has_offer: r.has_offer,
            offer_status: r.offer_status,
            created_at: r.created_at,
        })
        .collect();

    Ok((result, total))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fetch an address row and convert to snapshot, if the ID is present.
async fn fetch_address(
    pool: &PgPool,
    address_id: Option<Uuid>,
) -> Result<Option<AddressSnapshot>, ApiError> {
    let Some(id) = address_id else {
        return Ok(None);
    };
    let row: Option<AddressDbRow> = sqlx::query_as(
        r#"
        SELECT id, street, city, postal_code, country, floor, elevator, latitude, longitude
        FROM addresses WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|a| AddressSnapshot {
        id: a.id,
        street: a.street,
        city: a.city,
        postal_code: a.postal_code.unwrap_or_default(),
        country: if a.country.is_empty() {
            "Österreich".to_string()
        } else {
            a.country
        },
        floor: a.floor,
        elevator: a.elevator,
        needs_parking_ban: None,
        latitude: a.latitude,
        longitude: a.longitude,
    }))
}

/// Build an OfferSnapshot from a DB row, parsing line items.
fn build_offer_snapshot(r: &OfferDbRow) -> OfferSnapshot {
    let persons = r.persons.unwrap_or(2);
    let netto = r.price_cents;
    let brutto = (netto as f64 * 1.19).round() as i64;

    let line_items: Vec<LineItemSnapshot> = r
        .line_items_json
        .as_ref()
        .and_then(|json| serde_json::from_value::<Vec<serde_json::Value>>(json.clone()).ok())
        .map(|items| items.iter().map(|item| map_line_item(item, persons)).collect())
        .unwrap_or_default();

    let pdf_url = r.pdf_storage_key.as_ref().map(|_| {
        format!("/api/v1/inquiries/{}/pdf", "placeholder") // caller can override
    });

    OfferSnapshot {
        id: r.id,
        offer_number: r.offer_number.clone(),
        status: r.status.clone(),
        persons,
        hours: r.hours_estimated.unwrap_or(0.0),
        rate_cents: r.rate_per_hour_cents.unwrap_or(3000),
        total_netto_cents: netto,
        total_brutto_cents: brutto,
        line_items,
        pdf_url,
        valid_until: r.valid_until.map(|d| {
            chrono::DateTime::<Utc>::from_naive_utc_and_offset(
                d.and_hms_opt(23, 59, 59).unwrap_or_default(),
                Utc,
            )
        }),
        created_at: r.created_at,
    }
}

/// Map a single line_items_json entry to a LineItemSnapshot.
fn map_line_item(item: &serde_json::Value, persons: i32) -> LineItemSnapshot {
    let label = item
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("Sonstiges")
        .to_string();
    let remark = item.get("remark").and_then(|r| r.as_str()).map(String::from);
    let is_labor = item.get("is_labor").and_then(|b| b.as_bool()).unwrap_or(false);
    let quantity = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(1.0);
    let unit_price = item.get("unit_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
    let unit_price_cents = (unit_price * 100.0).round() as i64;
    let flat_total = item.get("flat_total").and_then(|v| v.as_f64());
    let is_flat_total = flat_total.is_some();
    let total_cents = if let Some(ft) = flat_total {
        (ft * 100.0).round() as i64
    } else if is_labor {
        (quantity * unit_price * persons as f64 * 100.0).round() as i64
    } else {
        (quantity * unit_price * 100.0).round() as i64
    };

    LineItemSnapshot {
        label,
        remark,
        quantity,
        unit_price_cents,
        total_cents,
        is_labor,
        is_flat_total,
    }
}

/// Extract S3 keys from source_data JSONB.
fn extract_s3_keys(source_data: Option<&serde_json::Value>) -> Vec<String> {
    source_data
        .and_then(|sd| {
            sd.get("s3_keys")?
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        })
        .unwrap_or_default()
}

/// Extract free-text customer remarks from notes, stripping known service keywords.
///
/// **Caller**: `build_inquiry_response`
/// **Why**: `inquiries.notes` mixes service flags with actual customer remarks.
fn extract_customer_message(notes: Option<&str>) -> Option<String> {
    let notes = notes?;
    let known = [
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

    let parts: Vec<&str> = notes
        .split(", ")
        .filter(|part| {
            let lower = part.trim().to_lowercase();
            !known.iter().any(|s| lower == *s)
                && !known_prefixes.iter().any(|p| lower.starts_with(p))
        })
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}
