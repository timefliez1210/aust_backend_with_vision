//! Single source of truth for building `InquiryResponse` and `InquiryListItem`
//! from the database, replacing duplicate implementations in quotes.rs, admin.rs,
//! and customer.rs.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::models::{
    AddressSnapshot, CustomerSnapshot, EmployeeAssignmentSnapshot, EstimationSnapshot,
    InquiryListItem, InquiryResponse, InquiryStatus, ItemSnapshot,
    LineItemSnapshot, OfferSnapshot, Services,
};

use crate::repositories::{
    address_repo, customer_repo, estimation_repo, inquiry_repo, offer_repo,
};
use crate::services::offer_builder::{parse_detected_items, VolumeEstimationRow};
use crate::types::resolve_billing_address_id;
use crate::ApiError;

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
    let row = inquiry_repo::fetch_by_id(pool, inquiry_id).await?;

    let status: InquiryStatus = row.status.parse().unwrap_or_default();
    let services: Services = serde_json::from_value(row.services).unwrap_or_default();

    // 2. Fetch customer
    let customer = customer_repo::fetch_by_id(pool, row.customer_id).await?;

    // 3. Fetch addresses
    let origin_address = fetch_address(pool, row.origin_address_id).await?;
    let destination_address = fetch_address(pool, row.destination_address_id).await?;
    let stop_address = fetch_address(pool, row.stop_address_id).await?;

    // 3b. Fetch recipient (if different from customer) and billing address
    let recipient = if let Some(rid) = row.recipient_id {
        if rid == row.customer_id {
            // recipient = same as customer, just include the customer snapshot later
            None // will be filled from customer below
        } else {
            customer_repo::fetch_by_id(pool, rid).await.ok().map(|c| CustomerSnapshot {
                id: c.id,
                name: c.name,
                salutation: c.salutation,
                first_name: c.first_name,
                last_name: c.last_name,
                email: c.email,
                phone: c.phone,
                customer_type: c.customer_type,
                company_name: c.company_name,
            })
        }
    } else {
        None
    };
    let billing_address = fetch_address(pool, row.inquiry_billing_address_id).await?;

    // Compute effective billing address (the one that lands on KVA/invoice)
    let effective_billing_address_id = resolve_billing_address_id(
        row.inquiry_billing_address_id,
        customer.billing_address_id,
        row.origin_address_id,
        row.destination_address_id,
        row.status.as_str(),
    );
    let effective_billing_address = fetch_address(pool, effective_billing_address_id).await?;

    // 4. Fetch latest completed estimation + items
    let est = estimation_repo::fetch_completed_for_inquiry(pool, inquiry_id).await?;

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
                let is_moveable = raw
                    .and_then(|r| r.get("is_moveable")?.as_bool())
                    .unwrap_or(true);
                let packs_into_boxes = raw
                    .and_then(|r| r.get("packs_into_boxes")?.as_bool())
                    .unwrap_or(false);

                let item_name = d.german_name.clone().unwrap_or_else(|| d.name.clone());
                let (parsed_name, quantity, per_item_volume) =
                    parse_quantity_prefix(&item_name, d.volume_m3);

                ItemSnapshot {
                    name: parsed_name,
                    volume_m3: per_item_volume,
                    quantity,
                    confidence: d.confidence,
                    category,
                    dimensions,
                    crop_url,
                    crop_s3_key: d.crop_s3_key.clone(),
                    source_image_url,
                    bbox: d.bbox.clone(),
                    bbox_image_index: d.bbox_image_index.map(|i| i as i32),
                    seen_in_images,
                    is_moveable,
                    packs_into_boxes,
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

    // 4b. Fetch ALL estimations (processing, failed, completed) for the full gallery + status UI.
    let all_estimation_rows = estimation_repo::fetch_all_for_inquiry(pool, inquiry_id).await?;
    let estimations: Vec<EstimationSnapshot> = all_estimation_rows
        .iter()
        .map(|e| {
            let s3_keys = extract_s3_keys(e.source_data.as_ref());
            let source_images: Vec<String> = s3_keys
                .iter()
                .map(|k| format!("/api/v1/estimates/images/{k}"))
                .collect();
            let source_video = e
                .source_data
                .as_ref()
                .and_then(|sd| sd.get("video_s3_key")?.as_str())
                .map(|k| format!("/api/v1/estimates/images/{k}"));
            let item_count = e
                .result_data
                .as_ref()
                .and_then(|rd| rd.as_array())
                .map(|arr| arr.len() as i64)
                .unwrap_or(0);
            EstimationSnapshot {
                id: e.id,
                method: e.method.clone(),
                status: e.status.clone(),
                total_volume_m3: e.total_volume_m3,
                confidence_score: e.confidence_score,
                item_count,
                source_images,
                source_video,
                created_at: e.created_at,
            }
        })
        .collect();

    // 5. Fetch latest active offer
    let offer_row = offer_repo::fetch_active_for_builder(pool, inquiry_id).await?;
    let offer = offer_row.map(|r| build_offer_snapshot(&r, inquiry_id));

    // 6. Fetch employee assignments
    let emp_rows = inquiry_repo::fetch_employee_assignments_snapshot(pool, inquiry_id).await?;
    let employees: Vec<EmployeeAssignmentSnapshot> = emp_rows
        .into_iter()
        .map(|r| EmployeeAssignmentSnapshot {
            employee_id: r.employee_id,
            first_name: r.first_name,
            last_name: r.last_name,
            clock_in: r.clock_in,
            clock_out: r.clock_out,
            start_time: r.start_time,
            end_time: r.end_time,
            break_minutes: r.break_minutes,
            actual_hours: r.actual_hours,
            employee_clock_in: r.employee_clock_in,
            employee_clock_out: r.employee_clock_out,
            employee_actual_hours: r.employee_actual_hours,
            notes: r.notes,
            job_date: r.job_date,
            transport_mode: r.transport_mode,
            travel_costs_cents: r.travel_costs_cents,
            accommodation_cents: r.accommodation_cents,
            misc_costs_cents: r.misc_costs_cents,
            meal_deduction: r.meal_deduction,
        })
        .collect();

    let end_date = row.end_date;
    let is_multi_day = end_date.map_or(false, |ed| row.scheduled_date.map_or(false, |sd| ed > sd));

    // 7. Extract customer message from notes
    let customer_message = extract_customer_message(row.notes.as_deref());

    // 8. Assemble response
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
        // preferred_date retired — scheduled_date is now the single date field
        scheduled_date: row.scheduled_date,
        start_time: row.start_time,
        end_time: row.end_time,
        notes: row.notes,
        customer_message,
        created_at: row.created_at,
        updated_at: row.updated_at,
        offer_sent_at: row.offer_sent_at,
        accepted_at: row.accepted_at,
        service_type: row.service_type,
        submission_mode: row.submission_mode,
        recipient,
        billing_address,
        effective_billing_address,
        customer: Some(CustomerSnapshot {
            id: customer.id,
            name: customer.name,
            salutation: customer.salutation,
            first_name: customer.first_name,
            last_name: customer.last_name,
            email: customer.email,
            phone: customer.phone,
            customer_type: customer.customer_type,
            company_name: customer.company_name,
        }),
        origin_address,
        destination_address,
        stop_address,
        estimation,
        estimations,
        items,
        offer,
        employees,
        end_date,
        is_multi_day,
        has_pauschale: row.has_pauschale,
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

    let items = inquiry_repo::list_items(
        pool,
        status,
        search_pattern.as_deref(),
        has_offer,
        limit,
        offset,
    )
    .await?;

    let total = inquiry_repo::count_items(
        pool,
        status,
        search_pattern.as_deref(),
        has_offer,
    )
    .await?;

    let result = items
        .into_iter()
        .map(|r| InquiryListItem {
            id: r.id,
            customer_name: r.customer_name,
            customer_email: r.customer_email,
            salutation: r.customer_salutation,
            service_type: r.service_type,
            customer_type: r.customer_type,
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

/// If an item name begins with a quantity prefix like `"4x "` or `"2x "`,
/// extract the quantity, strip the prefix, and return the per-item volume.
/// Otherwise return the original name with quantity = 1.
///
/// **Why**: Vision and inventory pipelines bake the quantity into the name
/// (e.g. `"4x Einzelbett komplett"`) while `volume_m3` is the *total* volume.
/// The frontend shows per-item volume and a separate ANZAHL column, so we
/// need to split the data before sending it downstream.
fn parse_quantity_prefix(name: &str, total_volume_m3: f64) -> (String, i64, f64) {
    let trimmed = name.trim();
    if let Some(pos) = trimmed.find('x') {
        let before = trimmed[..pos].trim();
        let after = trimmed[pos + 1..].trim();
        if let Ok(n) = before.parse::<i64>() {
            if n > 1 && !after.is_empty() {
                let per_item = total_volume_m3 / n as f64;
                return (after.to_string(), n, per_item);
            }
        }
    }
    (name.to_string(), 1, total_volume_m3)
}

/// Fetch an address row and convert to snapshot, if the ID is present.
async fn fetch_address(
    pool: &PgPool,
    address_id: Option<Uuid>,
) -> Result<Option<AddressSnapshot>, ApiError> {
    let Some(id) = address_id else {
        return Ok(None);
    };
    let row = address_repo::fetch_full(pool, id).await?;

    Ok(row.map(|a| AddressSnapshot {
        id: a.id,
        street: a.street,
        house_number: a.house_number,
        city: a.city,
        postal_code: a.postal_code.unwrap_or_default(),
        country: if a.country.is_empty() {
            "Deutschland".to_string()
        } else {
            a.country
        },
        floor: a.floor,
        elevator: a.elevator,
        needs_parking_ban: Some(a.parking_ban),
        parking_ban: a.parking_ban,
        latitude: a.latitude,
        longitude: a.longitude,
    }))
}

/// Build an OfferSnapshot from a DB row, parsing line items.
fn build_offer_snapshot(r: &offer_repo::OfferBuilderRow, inquiry_id: Uuid) -> OfferSnapshot {
    let persons = r.persons.unwrap_or(2);
    let netto = r.price_cents;
    let brutto = (netto as f64 * 1.19).round() as i64;

    let line_items: Vec<LineItemSnapshot> = r
        .line_items_json
        .as_ref()
        .and_then(|json| serde_json::from_value::<Vec<serde_json::Value>>(json.clone()).ok())
        .map(|items| items.iter().map(|item| map_line_item(item, persons)).collect())
        .unwrap_or_default();

    let pdf_url = r
        .pdf_storage_key
        .as_ref()
        .map(|_| format!("/api/v1/inquiries/{inquiry_id}/pdf"));

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
            DateTime::<Utc>::from_naive_utc_and_offset(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quantity_prefix_extracts_and_divides() {
        let (name, qty, vol) = parse_quantity_prefix("4x Einzelbett komplett", 4.0);
        assert_eq!(name, "Einzelbett komplett");
        assert_eq!(qty, 4);
        assert!((vol - 1.0).abs() < 0.001);
    }

    #[test]
    fn parse_quantity_prefix_no_prefix() {
        let (name, qty, vol) = parse_quantity_prefix("Sofa", 2.5);
        assert_eq!(name, "Sofa");
        assert_eq!(qty, 1);
        assert!((vol - 2.5).abs() < 0.001);
    }

    #[test]
    fn parse_quantity_prefix_single_x() {
        // "1x Sofa" is treated as no meaningful prefix >1, so it is returned unchanged
        let (name, qty, vol) = parse_quantity_prefix("1x Sofa", 2.0);
        assert_eq!(name, "1x Sofa");
        assert_eq!(qty, 1);
        assert!((vol - 2.0).abs() < 0.001);
    }

    #[test]
    fn parse_quantity_prefix_with_decimal_volume() {
        let (name, qty, vol) = parse_quantity_prefix("3x Schrank zerlegbar", 2.4);
        assert_eq!(name, "Schrank zerlegbar");
        assert_eq!(qty, 3);
        assert!((vol - 0.8).abs() < 0.001);
    }
}
