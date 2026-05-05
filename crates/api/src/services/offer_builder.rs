use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;
use crate::repositories::{AddressRow, CustomerRow};
use crate::repositories::{address_repo, customer_repo, offer_repo};
use crate::types::resolve_billing_address_id;
use crate::types::InquiryRow;
use aust_core::config::Config;
use aust_core::models::{
    DepthSensorResult, DetectedItem, Inquiry, Offer, OfferStatus, PricingInput, Services,
    VisionAnalysisResult,
};
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_offer_xlsx, parse_floor, DetectedItemRow, OfferData,
    OfferLineItem, PricingEngine,
};
use aust_storage::StorageProvider;
use tracing::warn;

/// Minimal SQLx projection of `volume_estimations` used by `parse_detected_items` and
/// re-exported to `admin` and `quotes` modules for the same purpose.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct VolumeEstimationRow {
    pub result_data: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub source_data: Option<serde_json::Value>,
    #[allow(dead_code)]
    pub total_volume_m3: Option<f64>,
    #[allow(dead_code)]
    pub method: String,
}

/// Summary data for the Telegram caption — populated during offer generation.
///
/// Passed alongside the PDF bytes to the Telegram approval bot so Alex can see key
/// moving details without opening the PDF.
pub(crate) struct TelegramSummary {
    pub customer_phone: String,
    pub origin_address: String,
    pub origin_floor: String,
    pub origin_elevator: Option<bool>,
    pub dest_address: String,
    pub dest_floor: String,
    pub dest_elevator: Option<bool>,
    pub scheduled_date: String,
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
///
/// Returned by `build_offer_with_overrides`. The caller (API handler or orchestrator)
/// uses `pdf_bytes` to upload to Telegram or attach to an email without a second
/// round-trip to S3.
pub(crate) struct GeneratedOffer {
    pub offer: Offer,
    pub pdf_bytes: Vec<u8>,
    pub customer_email: Option<String>,
    pub customer_name: String,
    pub summary: TelegramSummary,
}

/// Optional overrides applied during offer generation.
///
/// Used by the Telegram edit flow: Alex types a natural-language instruction, the LLM
/// parses it into numeric fields here, and `build_offer_with_overrides` uses them
/// instead of PricingEngine defaults.
///
/// Also used by the admin dashboard's regenerate endpoint for manual price/person/hour
/// adjustments submitted as JSON.
#[derive(Default, Debug)]
pub(crate) struct OfferOverrides {
    pub price_cents: Option<i64>,
    pub persons: Option<u32>,
    pub hours: Option<f64>,
    pub rate: Option<f64>,
    /// Custom non-labor line items. When set, replaces `build_line_items()` output.
    pub line_items: Option<Vec<OfferLineItem>>,
    /// When set, UPDATE this offer in-place instead of INSERTing a new one.
    /// The offer_number and created_at are preserved; pricing/PDF/line_items are refreshed.
    pub existing_offer_id: Option<Uuid>,
    /// When set, use this flat amount (in €) for Fahrkostenpauschale instead of recalculating
    /// via ORS. Stored in `offers.fahrt_override_cents`; once set it is the law — all future
    /// regenerations carry it forward automatically (loaded from DB inside `build_offer_with_overrides`).
    pub fahrt_flat_total: Option<f64>,
    /// When true, clears any stored admin override and forces a fresh ORS recalculation.
    /// Only valid when the admin explicitly wants to reset the Fahrt back to the calculated value.
    pub fahrt_reset: bool,
}

/// Generate an offer with no manual overrides — delegates to `build_offer_with_overrides`.
///
/// **Caller**: `orchestrator::try_auto_generate_offer`, and any code path that needs a
/// fresh offer without manual adjustments.
/// **Why**: Convenience wrapper so callers do not need to construct a default
/// `OfferOverrides` struct.
///
/// # Parameters
/// - `db` — live PostgreSQL connection pool
/// - `storage` — S3-compatible storage for uploading the PDF
/// - `config` — application config (company depot address, rate per km, etc.)
/// - `inquiry_id` — the inquiry to generate an offer for
/// - `valid_days` — optional number of days until the offer expires
///
/// # Returns
/// `GeneratedOffer` containing the persisted `Offer` record, the raw PDF bytes, and the
/// `TelegramSummary` for the approval caption.
///
/// # Errors
/// Propagates all errors from `build_offer_with_overrides`.
pub(crate) async fn build_offer(
    db: &PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    inquiry_id: Uuid,
    valid_days: Option<i64>,
) -> Result<GeneratedOffer, ApiError> {
    build_offer_with_overrides(db, storage, config, inquiry_id, valid_days, &OfferOverrides::default()).await
}

/// Core offer generation pipeline with optional manual overrides.
///
/// **Caller**: `build_offer` (no overrides), `generate_offer` route handler (API),
/// `orchestrator::try_auto_generate_offer` (background), `admin::regenerate_offer` (dashboard).
/// **Why**: Central function for the entire inquiry-to-offer pipeline: fetches all required
/// data, computes pricing, builds XLSX via template, converts to PDF via LibreOffice,
/// uploads to S3, and inserts (or updates in-place) the offer DB record.
///
/// # Parameters
/// - `db` — live PostgreSQL connection pool
/// - `storage` — S3-compatible storage provider
/// - `config` — application config including depot address, km rate, JWT secret, etc.
/// - `inquiry_id` — the inquiry to generate an offer for; must have `estimated_volume_m3`
/// - `valid_days` — optional offer validity period; stored in `offers.valid_until`
/// - `overrides` — optional manual overrides for price, persons, hours, rate, or line items;
///   when `existing_offer_id` is set, the existing offer record is updated in-place
///   (preserving `offer_number` and `created_at`)
///
/// # Returns
/// `GeneratedOffer` with the persisted `Offer`, raw PDF/XLSX bytes, customer email, and
/// a `TelegramSummary` for the approval message caption.
///
/// # Errors
/// - 404 if inquiry or customer not found
/// - 400 if inquiry has no volume estimate
/// - 500 on XLSX generation, PDF conversion, S3 upload, or DB errors
///
/// # Math
/// Labor netto = `hours × persons × rate`
/// Actual netto = `sum(flat_total for Fahrkostenpauschale) + sum(qty × price for non-labor items) + labor_netto`
/// `rate = calculate_rate_override(price_override, rate_override, persons, hours, line_items)`
pub(crate) async fn build_offer_with_overrides(
    db: &PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    inquiry_id: Uuid,
    valid_days: Option<i64>,
    overrides: &OfferOverrides,
) -> Result<GeneratedOffer, ApiError> {
    // 1. Fetch inquiry
    let inquiry_row: InquiryRow = offer_repo::fetch_inquiry_for_offer(db, inquiry_id)
        .await
        .map_err(ApiError::Database)?
        .ok_or_else(|| ApiError::NotFound("Inquiry not found".into()))?;

    let inquiry = Inquiry::from(inquiry_row);

    let volume = inquiry
        .estimated_volume_m3
        .ok_or_else(|| ApiError::BadRequest("Inquiry has no volume estimate".into()))?;

    let distance = inquiry.distance_km.unwrap_or(0.0);

    // 2. Fetch customer
    let customer: CustomerRow = customer_repo::fetch_by_id(db, inquiry.customer_id).await?;

    // 3. Fetch addresses
    let origin: Option<AddressRow> = address_repo::fetch_optional(db, inquiry.origin_address_id).await?;
    let destination: Option<AddressRow> = address_repo::fetch_optional(db, inquiry.destination_address_id).await?;
    let stop_address: Option<AddressRow> = address_repo::fetch_optional(db, inquiry.stop_address_id).await?;

    // 4. Fetch latest volume estimation for detected items
    let repo_estimation = offer_repo::fetch_latest_estimation(db, inquiry_id)
        .await
        .map_err(ApiError::Database)?;
    let estimation: Option<VolumeEstimationRow> = repo_estimation.map(|e| VolumeEstimationRow {
        result_data: e.result_data,
        source_data: e.source_data,
        total_volume_m3: e.total_volume_m3,
        method: e.method,
    });

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

    let stop_floor = stop_address
        .as_ref()
        .and_then(|a| a.floor.as_deref())
        .map(parse_floor);

    let pricing_input = PricingInput {
        volume_m3: volume,
        distance_km: distance,
        scheduled_date: inquiry.scheduled_date,
        floor_origin: origin_floor,
        floor_destination: dest_floor,
        has_elevator_origin: origin.as_ref().and_then(|a| a.elevator),
        has_elevator_destination: destination.as_ref().and_then(|a| a.elevator),
        floor_stop: stop_floor,
        has_elevator_stop: stop_address.as_ref().and_then(|a| a.elevator),
    };

    let pricing_engine = PricingEngine::with_rate(config.company.rate_per_person_hour_cents, config.company.saturday_surcharge_cents);
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

    // 7. Build line items.
    //    Frontend sends a fully ordered list (line_items=Some). Backend trusts the order
    //    and only resolves special items in-place: labor (filled with persons/hours/rate),
    //    Fahrkostenpauschale (admin override or ORS), Nürnbergerversicherung (canonical
    //    coverage line). If insurance is absent from the list, it is omitted from the offer
    //    (= admin removed it). Custom items with quantity=0 are filtered out (skeleton
    //    placeholders the admin left blank).
    //
    //    When line_items is None (auto-gen / Telegram path): compose default order matching
    //    the admin UI's preferred sequence.
    let inquiry_services = inquiry.services.clone().unwrap_or_default();

    // Resolved fahrt value: Some(euros) if admin-set (or previously admin-set), None if ORS should calculate.
    let admin_fahrt_euros: Option<f64> = if overrides.fahrt_reset {
        None
    } else if let Some(v) = overrides.fahrt_flat_total {
        Some(v)
    } else if let Some(ref items) = overrides.line_items {
        items
            .iter()
            .find(|li| li.description == "Fahrkostenpauschale")
            .and_then(|li| {
                li.flat_total.or_else(|| {
                    let q = li.quantity * li.unit_price;
                    if q > 0.0 { Some(q) } else { None }
                })
            })
    } else if let Some(existing_id) = overrides.existing_offer_id {
        offer_repo::fetch_fahrt_override(db, existing_id)
            .await?
            .map(|c| c as f64 / 100.0)
    } else {
        None
    };

    let make_labor = |rate: f64| OfferLineItem {
        description: format!("{} Umzugshelfer", pricing_result.estimated_helpers),
        quantity: pricing_result.estimated_hours,
        unit_price: rate,
        is_labor: true,
        ..Default::default()
    };
    let make_insurance = || OfferLineItem {
        description: "Nürnbergerversicherung".to_string(),
        quantity: 1.0,
        unit_price: 0.0,
        flat_total: Some(0.0),
        remark: Some("Deckungssumme: 620,00 Euro / m³".to_string()),
        ..Default::default()
    };
    let resolved_fahrt_item = if let Some(total) = admin_fahrt_euros {
        OfferLineItem {
            description: "Fahrkostenpauschale".to_string(),
            quantity: 0.0,
            unit_price: 0.0,
            is_labor: false,
            flat_total: Some(total),
            remark: None,
        }
    } else {
        build_fahrt_item(config, origin.as_ref(), destination.as_ref(), stop_address.as_ref(), distance).await
    };

    let line_items: Vec<OfferLineItem> = if let Some(ref items) = overrides.line_items {
        // Admin sent an authoritative ordered list. Resolve special items in-place,
        // filter qty=0 placeholders, preserve order.
        let mut result = Vec::with_capacity(items.len());
        for li in items {
            let is_labor_item = li.is_labor || li.description.ends_with("Umzugshelfer");
            let is_fahrt = li.description == "Fahrkostenpauschale";
            let is_insurance = li.description == "Nürnbergerversicherung";

            if is_labor_item {
                // Unit price filled in after rate resolution below
                result.push(make_labor(0.0));
            } else if is_fahrt {
                result.push(resolved_fahrt_item.clone());
            } else if is_insurance {
                result.push(make_insurance());
            } else if li.quantity > 0.0 {
                result.push(li.clone());
            }
            // qty=0 custom items: dropped (skeleton placeholders)
        }
        result
    } else {
        // Auto-gen / Telegram path: labor first, then service items, then fahrt, insurance last.
        let service_prices = ServicePrices::from_config(config);
        let auto = build_line_items(&inquiry_services, &service_prices);
        let mut services_items: Vec<OfferLineItem> = Vec::new();
        let mut insurance: Option<OfferLineItem> = None;
        for item in auto {
            match item.description.as_str() {
                "Nürnbergerversicherung" => insurance = Some(item),
                _ => services_items.push(item),
            }
        }
        let mut composed: Vec<OfferLineItem> = Vec::new();
        composed.push(make_labor(0.0));
        composed.extend(services_items);
        composed.push(resolved_fahrt_item.clone());
        if let Some(i) = insurance { composed.push(i); }
        composed
    };

    let rate_override = calculate_rate_override(
        overrides.price_cents,
        overrides.rate,
        pricing_result.estimated_helpers,
        pricing_result.estimated_hours,
        &line_items,
    );

    // Fill labor unit_price now that rate is resolved.
    let line_items: Vec<OfferLineItem> = line_items
        .into_iter()
        .map(|mut li| {
            if li.is_labor {
                li.unit_price = rate_override;
            }
            li
        })
        .collect();

    // 8. Build OfferData
    let now = chrono::Utc::now();
    let today = now.date_naive();

    let customer_name = customer.display_name();
    let customer_salutation = customer.address_salutation();
    let greeting = customer.formal_greeting();

    let moving_date = inquiry
        .scheduled_date
        .map(|d| d.format("%d.%m.%Y").to_string())
        .unwrap_or_else(|| "nach Vereinbarung".to_string());

    let origin_street = origin.as_ref().map(|a| a.street.clone()).unwrap_or_default();
    let origin_city = origin
        .as_ref()
        .map(|a| format_city(a))
        .unwrap_or_default();
    let origin_floor_info = origin
        .as_ref()
        .and_then(|a| a.floor.as_deref())
        .map(format_floor_display)
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
        .and_then(|a| a.floor.as_deref())
        .map(format_floor_display)
        .unwrap_or_default();

    // Resolve billing address: explicit > customer default > destination (post-move) > origin (pre-move)
    let billing_addr_id = resolve_billing_address_id(
        inquiry.billing_address_id,
        customer.billing_address_id,
        inquiry.origin_address_id,
        inquiry.destination_address_id,
        inquiry.status.as_str(),
    );
    let billing_addr = address_repo::fetch_optional(db, billing_addr_id).await?;
    let billing_street = billing_addr
        .as_ref()
        .map(|a| {
            match a.house_number.as_deref() {
                Some(hn) if !hn.is_empty() => format!("{} {}", a.street, hn),
                _ => a.street.clone(),
            }
        })
        .unwrap_or_else(|| origin_street.clone());
    let billing_city = billing_addr
        .as_ref()
        .map(|a| format_city(a))
        .unwrap_or_else(|| origin_city.clone());

    // Get or generate offer ID and number (UPDATE-in-place when existing_offer_id is set)
    let (offer_id, offer_number) = if let Some(existing_id) = overrides.existing_offer_id {
        let offer_number = offer_repo::fetch_offer_number(db, existing_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("Offer {existing_id} not found")))?;
        (existing_id, offer_number)
    } else {
        let offer_number = offer_repo::next_offer_number(db, today).await
            .map_err(|e| ApiError::Database(e))?;
        (Uuid::now_v7(), offer_number)
    };

    // Build services display string from structured Services and use notes as customer message
    let services_str = format_services_display(&inquiry_services);
    let customer_message = inquiry.notes.clone().unwrap_or_default();

    let valid_until_date =
        valid_days.map(|days| (now + chrono::Duration::days(days)).date_naive());

    // line_items is already the full ordered list (labor / fahrt / insurance resolved in place).
    let all_items = line_items;

    let offer_data = OfferData {
        offer_number: offer_number.clone(),
        date: today,
        valid_until: valid_until_date,
        customer_salutation,
        customer_name: customer_name.clone(),
        customer_street: billing_street.clone(),
        customer_city: billing_city.clone(),
        customer_phone: customer.phone.clone().unwrap_or_default(),
        customer_email: customer.email.clone(),
        company_name: customer.company_name.clone(),
        attention_line: Some(customer.attention_line()).filter(|s| !s.is_empty()),
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
        line_items: all_items,
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

    // 10. Insert or update offer record
    let line_items_json = serde_json::to_value(&offer_data.line_items).ok();
    let rate_cents = (rate_override * 100.0).round() as i64;
    // Persist the fahrt override. Rules:
    //   - fahrt_reset=true  → None  (explicitly cleared by admin, ORS will recalculate next time too)
    //   - admin_fahrt_euros is Some → Some(cents)  (admin value, or previously stored admin value
    //                                               loaded from DB — both must be kept)
    //   - ORS-calculated (admin_fahrt_euros is None) → None  (calculated value, not a law)
    let fahrt_override_cents: Option<i32> = if overrides.fahrt_reset {
        None
    } else {
        admin_fahrt_euros.map(|euros| (euros * 100.0).round() as i32)
    };

    // Compute actual netto from line items (must match XLSX SUM(G31:G42))
    let actual_netto: f64 = offer_data.line_items.iter().map(|item| {
        if let Some(ft) = item.flat_total {
            ft
        } else if item.is_labor {
            item.quantity * item.unit_price * pricing_result.estimated_helpers as f64
        } else {
            item.quantity * item.unit_price
        }
    }).sum();
    let actual_netto_cents = (actual_netto * 100.0).round() as i64;

    let repo_row = if overrides.existing_offer_id.is_some() {
        offer_repo::update_returning(
            db, offer_id, actual_netto_cents, Some(&s3_key), OfferStatus::Draft.as_str(),
            pricing_result.estimated_helpers as i32, pricing_result.estimated_hours,
            rate_cents, &line_items_json, fahrt_override_cents,
        )
        .await
        .map_err(ApiError::Database)?
    } else {
        match offer_repo::insert_returning(
            db, offer_id, inquiry_id, actual_netto_cents, "EUR", valid_until_date,
            Some(&s3_key), OfferStatus::Draft.as_str(), now, &offer_number,
            pricing_result.estimated_helpers as i32, pricing_result.estimated_hours,
            rate_cents, &line_items_json, fahrt_override_cents,
        )
        .await
        {
            Ok(row) => row,
            Err(sqlx::Error::Database(ref e)) if e.constraint() == Some("offers_inquiry_active_unique") => {
                // M1 guard: concurrent offer generation beat us — the unique partial index
                // prevented a duplicate. Treat as idempotent success by fetching the existing offer.
                tracing::info!(inquiry_id = %inquiry_id, "Concurrent offer generation detected (unique constraint) — returning existing offer");
                let existing_id = offer_repo::fetch_active_id_for_inquiry(db, inquiry_id)
                    .await
                    .map_err(ApiError::Database)?
                    .ok_or_else(|| ApiError::Internal("Offer exists but could not be fetched".into()))?;
                offer_repo::update_returning(
                    db, existing_id, actual_netto_cents, Some(&s3_key), OfferStatus::Draft.as_str(),
                    pricing_result.estimated_helpers as i32, pricing_result.estimated_hours,
                    rate_cents, &line_items_json, fahrt_override_cents,
                )
                .await
                .map_err(ApiError::Database)?
            }
            Err(e) => return Err(ApiError::Database(e)),
        }
    };

    // Map repo row to Offer domain model
    let offer = {
        #[derive(Debug, sqlx::FromRow)]
        struct OfferRow {
            id: Uuid,
            inquiry_id: Uuid,
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
            #[allow(dead_code)]
            fahrt_override_cents: Option<i32>,
        }
        let row = OfferRow {
            id: repo_row.id, inquiry_id: repo_row.inquiry_id, price_cents: repo_row.price_cents,
            currency: repo_row.currency, valid_until: repo_row.valid_until,
            pdf_storage_key: repo_row.pdf_storage_key, status: repo_row.status,
            created_at: repo_row.created_at, sent_at: repo_row.sent_at,
            offer_number: repo_row.offer_number, persons: repo_row.persons,
            hours_estimated: repo_row.hours_estimated, rate_per_hour_cents: repo_row.rate_per_hour_cents,
            line_items_json: repo_row.line_items_json, fahrt_override_cents: repo_row.fahrt_override_cents,
        };
        let status: OfferStatus = row.status.parse().unwrap_or_default();
        Offer {
            id: row.id,
            inquiry_id: row.inquiry_id,
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
    };

    // Update inquiry status
    crate::repositories::inquiry_repo::update_status(db, inquiry_id, "offer_ready", now)
        .await
        .map_err(|e| ApiError::Database(e))?;

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
        scheduled_date: moving_date,
        volume_m3: volume,
        items_count: detected_items.len(),
        distance_km: distance,
        services: services_str,
        persons: pricing_result.estimated_helpers,
        hours: pricing_result.estimated_hours,
        rate: rate_override,
        netto_cents: actual_netto_cents,
        customer_message,
    };

    Ok(GeneratedOffer {
        offer,
        pdf_bytes,
        customer_email: customer.email,
        customer_name,
        summary,
    })
}

/// Format a `Services` struct into a human-readable German string for Telegram display.
///
/// **Caller**: `build_offer_with_overrides` — used to populate the `services` field in
/// `TelegramSummary`.
/// **Why**: The Telegram caption shows a summary of selected additional services so Alex
/// can verify the offer includes the correct extras.
///
/// # Parameters
/// - `services` — structured `Services` flags from the inquiry
///
/// # Returns
/// Maps a stored floor value to a German display label for the offer PDF.
///
/// The admin address editor stores numeric strings ("0", "1", ...) while the public
/// quote form stores full German labels ("Erdgeschoss", "3. Stock"). Both are handled
/// so the XLSX always shows a human-readable string.
fn format_floor_display(floor: &str) -> String {
    match floor.trim() {
        "0" => "Erdgeschoss".to_string(),
        "-1" => "Keller".to_string(),
        "1" => "1. OG".to_string(),
        "2" => "2. OG".to_string(),
        "3" => "3. OG".to_string(),
        "4" => "4. OG".to_string(),
        "5" => "5. OG".to_string(),
        other => other.to_string(), // pass-through for "Erdgeschoss", "3. Stock", etc.
    }
}

/// Comma-separated string of active services in German, e.g.
/// `"Verpackungsservice, Montage, Halteverbot Beladestelle"`. Empty string when no flags are set.
fn format_services_display(services: &Services) -> String {
    let mut parts = Vec::new();
    if services.packing {
        parts.push("Verpackungsservice".to_string());
    }
    if services.assembly {
        parts.push("Montage".to_string());
    }
    if services.disassembly {
        parts.push("Demontage".to_string());
    }
    if services.storage {
        parts.push("Einlagerung".to_string());
    }
    if services.disposal {
        parts.push("Entsorgung".to_string());
    }
    if services.parking_ban_origin {
        parts.push("Halteverbot Beladestelle".to_string());
    }
    if services.parking_ban_destination {
        parts.push("Halteverbot Entladestelle".to_string());
    }
    if services.transporter {
        parts.push("3,5t Transporter m. Koffer".to_string());
    }
    parts.join(", ")
}

/// Format a city string as "PLZ City" (or just "City" when postal code is absent).
///
/// **Caller**: `build_offer_with_overrides` for OfferData fields and address display strings.
pub(crate) fn format_city(addr: &AddressRow) -> String {
    format!(
        "{}{}",
        addr.postal_code
            .as_ref()
            .map(|p| format!("{p} "))
            .unwrap_or_default(),
        addr.city
    )
}


/// Detect the appropriate salutation and greeting line from a customer name.
///
/// **Caller**: `build_offer_with_overrides` (XLSX OfferData fields) and `greeting_for_name`.
/// **Why**: The offer template needs both the address-block salutation (e.g. "Herrn") and
/// a formal greeting line. Uses explicit "Herr"/"Frau" prefix first, then falls back to
/// a heuristic lookup of common German/Austrian female first names.
///
/// # Parameters
/// - `name` — raw customer name string
///
/// # Returns
/// `(salutation, greeting)` — e.g. `("Herrn", "Sehr geehrter Herr Müller,")` or
/// `("", "Sehr geehrte Damen und Herren,")` for single-word names
pub(crate) fn detect_salutation_and_greeting(name: &str) -> (String, String) {
    // If the name contains "Frau" or "Herr" prefix, use that directly
    let name_trimmed = name.trim();
    if name_trimmed.starts_with("Frau ") {
        let after = &name_trimmed[5..];
        return (
            "Frau".to_string(),
            format!("Sehr geehrte Frau {after},"),
        );
    }
    if name_trimmed.starts_with("Herr ") {
        let after = &name_trimmed[5..];
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

/// Build the Fahrkostenpauschale (flat travel cost) line item.
///
/// **Caller**: `build_offer_with_overrides` — always the first non-labor line item.
/// **Why**: Austrian moving companies charge a flat travel fee based on the full round-trip
/// distance from the company depot, including any intermediate stop. This function calls
/// OpenRouteService to calculate the exact route rather than doubling the stored one-way
/// `distance_km`.
///
/// # Parameters
/// - `config` — provides `company.depot_address` (ORS start/end point) and
///   `company.fahrt_rate_per_km` (EUR per km)
/// - `origin` — moving-out address; `None` triggers fallback
/// - `destination` — moving-in address; `None` triggers fallback
/// - `stop` — optional intermediate stop address (e.g. storage facility)
/// - `distance_km` — stored one-way distance used only for the ORS fallback
///
/// # Returns
/// An `OfferLineItem` with `flat_total` set (quantity and unit_price left at 0 because
/// the total is a lump sum, not quantity × price).
///
/// # Math
/// `flat_total = ORS_route_total_km × fahrt_rate_per_km`
/// ORS route: `depot → origin → [stop] → destination → depot`
/// Fallback: `flat_total = distance_km × 2.0 × fahrt_rate_per_km`
async fn build_fahrt_item(
    config: &Config,
    origin: Option<&AddressRow>,
    destination: Option<&AddressRow>,
    stop: Option<&AddressRow>,
    distance_km: f64,
) -> OfferLineItem {
    let depot = config.company.depot_address.clone();
    let rate = config.company.fahrt_rate_per_km;

    let format_addr = |a: &AddressRow| -> String {
        match &a.postal_code {
            Some(plz) => format!("{}, {} {}", a.street, plz, a.city),
            None => format!("{}, {}", a.street, a.city),
        }
    };

    let flat_total = if let (Some(orig), Some(dest)) = (origin, destination) {
        let mut route_addrs = vec![depot.clone(), format_addr(orig)];
        if let Some(s) = stop {
            route_addrs.push(format_addr(s));
        }
        route_addrs.push(format_addr(dest));
        route_addrs.push(depot.clone());

        let calculator = RouteCalculator::new(config.maps.api_key.clone());
        match calculator.calculate(&RouteRequest { addresses: route_addrs }).await {
            Ok(result) => result.total_distance_km * rate,
            Err(e) => {
                warn!("Fahrkostenpauschale route calculation failed ({e}), using fallback");
                distance_km * 2.0 * rate
            }
        }
    } else {
        // No addresses — use stored distance as fallback
        distance_km * 2.0 * rate
    };

    OfferLineItem {
        description: "Fahrkostenpauschale".to_string(),
        flat_total: Some(flat_total),
        ..Default::default()
    }
}

/// Configurable service line-item prices, loaded from `CompanyConfig`.
///
/// **Caller**: `build_line_items` — determines unit prices for assembly, parking ban, and packing.
/// **Why**: Avoids hardcoded pricing constants that require a redeploy to change.
pub(crate) struct ServicePrices {
    pub assembly_unit_price: f64,
    pub parking_ban_unit_price: f64,
    pub packing_unit_price: f64,
    pub transporter_unit_price: f64,
}

impl ServicePrices {
    /// Build from the application config.
    pub fn from_config(config: &Config) -> Self {
        Self {
            assembly_unit_price: config.company.assembly_price,
            parking_ban_unit_price: config.company.parking_ban_price,
            packing_unit_price: config.company.packing_price,
            transporter_unit_price: config.company.transporter_price,
        }
    }

    /// Default prices for tests (matches CompanyConfig defaults).
    #[allow(dead_code)]
    pub fn defaults() -> Self {
        Self {
            assembly_unit_price: 25.0,
            parking_ban_unit_price: 100.0,
            packing_unit_price: 30.0,
            transporter_unit_price: 60.0,
        }
    }
}

/// Derive the non-labor XLSX line items from structured `Services` flags.
///
/// **Caller**: `build_offer_with_overrides` — called only when `overrides.line_items` is
/// `None`; the result is appended after the labor item and the Fahrkostenpauschale.
/// **Why**: Services requested by the customer (parking bans, packing, assembly) are stored
/// as JSONB in `inquiries.services`. This function converts those boolean flags into
/// typed `OfferLineItem` values that map to specific rows in the XLSX template.
///
/// # Parameters
/// - `services` — structured `Services` flags from the inquiry
///
/// # Returns
/// `Vec<OfferLineItem>` in template row order:
/// - Demontage (row 31, €50) — if `services.disassembly`
/// - Montage (row 31, €50) — if `services.assembly`
/// - Halteverbotszone (row 32, €100/zone) — 1–2 zones depending on flags
/// - Umzugsmaterial (row 33, €30) — if `services.packing`
/// - 3,5t Transporter m. Koffer (row ??, €60) — if `services.transporter`
/// - Nürnbergerversicherung (always last, €0, `flat_total = 0.0`)
///
/// Does NOT include Fahrkostenpauschale (computed separately in `build_offer_with_overrides`)
/// or the labor item (prepended separately before this list is appended).
pub(crate) fn build_line_items(services: &Services, prices: &ServicePrices) -> Vec<OfferLineItem> {
    let mut items = Vec::new();

    // Demontage — if disassembly service requested
    if services.disassembly {
        items.push(OfferLineItem {
            description: "Demontage".to_string(),
            quantity: 1.0,
            unit_price: prices.assembly_unit_price,
            ..Default::default()
        });
    }

    // Montage — if assembly requested
    if services.assembly {
        items.push(OfferLineItem {
            description: "Montage".to_string(),
            quantity: 1.0,
            unit_price: prices.assembly_unit_price,
            ..Default::default()
        });
    }

    // Halteverbotszone — count parking ban locations
    let has_origin_ban = services.parking_ban_origin;
    let has_dest_ban = services.parking_ban_destination;

    let halteverbot_count = has_origin_ban as u32 + has_dest_ban as u32;

    if halteverbot_count > 0 {
        let remark = match (has_origin_ban, has_dest_ban) {
            (true, true) => Some("Beladestelle + Entladestelle".to_string()),
            (true, false) => Some("Beladestelle".to_string()),
            (false, true) => Some("Entladestelle".to_string()),
            _ => None,
        };
        items.push(OfferLineItem {
            description: "Halteverbotszone".to_string(),
            quantity: halteverbot_count as f64,
            unit_price: prices.parking_ban_unit_price,
            remark,
            ..Default::default()
        });
    }

    // Umzugsmaterial — if packing service requested
    if services.packing {
        items.push(OfferLineItem {
            description: "Umzugsmaterial".to_string(),
            quantity: 1.0,
            unit_price: prices.packing_unit_price,
            remark: Some(format!("Stretchfolie, Decken, Gurte Einzelpreis {} €", format!("{:.2}", prices.packing_unit_price).replace('.', ","))),
            ..Default::default()
        });
    }

    // 3,5t Transporter m. Koffer
    if services.transporter {
        items.push(OfferLineItem {
            description: "3,5t Transporter m. Koffer".to_string(),
            quantity: 1.0,
            unit_price: prices.transporter_unit_price,
            ..Default::default()
        });
    }

    // Nürnbergerversicherung — always last, €0
    items.push(OfferLineItem {
        description: "Nürnbergerversicherung".to_string(),
        quantity: 1.0,
        unit_price: 0.0,
        flat_total: Some(0.0),
        remark: Some("Deckungssumme: 620,00 Euro / m³".to_string()),
        ..Default::default()
    });

    items
}

/// Resolve the effective hourly rate, back-calculating from a target price when needed.
///
/// **Caller**: `build_offer_with_overrides` — called after pricing and line items are
/// finalized, just before building `OfferData`.
/// **Why**: Alex always thinks in brutto prices. When he overrides the total price in the
/// Telegram edit flow, the LLM converts that to a netto value (÷ 1.19) and puts it in
/// `price_cents_override`. This function back-calculates the per-hour rate such that the
/// labor line item alone bridges the gap between the non-labor subtotal and the target netto.
///
/// # Parameters
/// - `price_cents_override` — target total netto in cents; `None` means no price override
/// - `rate_override` — explicit hourly rate in EUR; takes precedence over price override
/// - `persons` — number of workers (clamped to ≥ 1 to avoid division by zero)
/// - `hours` — estimated working hours (clamped to ≥ 1.0)
/// - `line_items` — non-labor items used to calculate `other_items_netto`
///
/// # Returns
/// Effective hourly rate in EUR (not cents). Default is `30.0` when no override is given.
///
/// # Math
/// `other_items_netto = Σ flat_total || (qty × price)` for all non-labor items
/// `labor_netto = max(0, target_netto - other_items_netto)`
/// `rate = labor_netto / (persons × hours)`
pub(crate) fn calculate_rate_override(
    price_cents_override: Option<i64>,
    rate_override: Option<f64>,
    persons: u32,
    hours: f64,
    line_items: &[OfferLineItem],
) -> f64 {
    if let Some(r) = rate_override {
        r
    } else if let Some(price_cents) = price_cents_override {
        let persons = persons.max(1) as f64;
        let hours = hours.max(1.0);
        let target_netto = price_cents as f64 / 100.0;
        let other_items_netto: f64 = line_items
            .iter()
            .filter(|li| !li.is_labor)
            .map(|li| li.flat_total.unwrap_or(li.quantity * li.unit_price))
            .sum();
        let labor_netto = (target_netto - other_items_netto).max(0.0);
        labor_netto / (persons * hours)
    } else {
        30.0
    }
}

/// Inventory item parsed from the VolumeCalculator `items_list` text format.
///
/// Matches the JSON schema stored by `orchestrator::parse_items_list_text()` in
/// `volume_estimations.result_data` for `method = 'manual'` or `method = 'inventory'`.
#[derive(Debug, Clone, serde::Deserialize)]
struct ParsedInventoryItem {
    name: String,
    quantity: u32,
    volume_m3: f64,
}

/// Parse detected items from `volume_estimations.result_data` into a uniform `DetectedItemRow` list.
///
/// **Caller**: `build_offer_with_overrides` (for the XLSX items sheet), `quotes::get_quote`
/// and `admin::get_quote_detail` (for the frontend item cards), `admin::get_offer_detail`.
/// **Why**: The `result_data` column stores different JSON schemas depending on which
/// estimation method was used. This function tries each known schema in priority order
/// and returns the first successful parse.
///
/// # Parameters
/// - `estimation` — the `VolumeEstimationRow` row; returns empty vec when `None`
///
/// # Returns
/// Flat list of `DetectedItemRow` values. For inventory/manual items the `quantity` is
/// baked into the name (e.g. "2x Sofa") and `volume_m3` is the already-multiplied total.
///
/// Deserialization priority:
/// 1. `DepthSensorResult` (ML 3D pipeline result with `detected_items` + `dimensions`)
/// 2. `Vec<VisionAnalysisResult>` (LLM per-image array)
/// 3. Single `VisionAnalysisResult` (LLM single-image)
/// 4. `Vec<DetectedItem>` (raw item array)
/// 5. `Vec<ParsedInventoryItem>` (VolumeCalculator text format)
pub(crate) fn parse_detected_items(estimation: Option<&VolumeEstimationRow>) -> Vec<DetectedItemRow> {
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

/// Map an English vision-model detection label to its German Umzugsgutliste name.
///
/// **Caller**: `detected_item_to_row` — applied when `DetectedItem.german_name` is absent.
/// **Why**: Grounding DINO produces English class labels. The offer and the items sheet
/// must use German Umzugsgutliste (moving goods list) terminology. This lookup mirrors
/// the `RE_CATALOG` in `services/vision/app/models/schemas.py`.
///
/// # Parameters
/// - `label` — English detection label (case-insensitive)
///
/// # Returns
/// `Some(&str)` with the German Umzugsgutliste name, or `None` if unlisted.
pub(crate) fn label_to_german(label: &str) -> Option<&'static str> {
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

/// Convert a `DetectedItem` domain model into the flattened `DetectedItemRow` used in offer generation.
///
/// **Caller**: `parse_detected_items` — used for DepthSensorResult and VisionAnalysisResult paths.
/// **Why**: `DetectedItemRow` is the struct expected by `OfferData.detected_items` for the
/// XLSX items sheet. This conversion also fills in `german_name` via `label_to_german`
/// when the original item did not carry one, and stringifies the `dimensions` struct.
///
/// # Parameters
/// - `item` — raw `DetectedItem` from the ML or LLM pipeline
///
/// # Returns
/// A `DetectedItemRow` ready for XLSX rendering.
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

    // --- build_line_items tests ---

    #[test]
    fn always_has_versicherung_last() {
        let items = build_line_items(&Services::default(), &ServicePrices::defaults());
        assert!(!items.is_empty(), "should have at least Versicherung");
        let last = items.last().unwrap();
        assert_eq!(last.description, "Nürnbergerversicherung");
        assert_eq!(last.unit_price, 0.0);
        assert_eq!(last.flat_total, Some(0.0));
    }

    #[test]
    fn no_transporter_item_when_disabled() {
        let items = build_line_items(&Services { transporter: false, ..Default::default() }, &ServicePrices::defaults());
        assert!(!items.iter().any(|i| i.description.contains("Transporter")), "Transporter must not appear when disabled");
    }

    #[test]
    fn transporter_item_when_enabled() {
        let items = build_line_items(&Services { transporter: true, ..Default::default() }, &ServicePrices::defaults());
        assert!(items.iter().any(|i| i.description.contains("Transporter")), "Transporter should appear when enabled");
    }

    #[test]
    fn no_anfahrt_item() {
        let items = build_line_items(&Services::default(), &ServicePrices::defaults());
        assert!(!items.iter().any(|i| i.description.contains("Anfahrt")), "Anfahrt must not appear");
    }

    #[test]
    fn demontage_separate_from_montage() {
        let items = build_line_items(&Services { disassembly: true, ..Default::default() }, &ServicePrices::defaults());
        assert!(items.iter().any(|i| i.description == "Demontage"), "should have Demontage");
        assert!(!items.iter().any(|i| i.description == "Montage"), "should NOT have Montage");
    }

    #[test]
    fn montage_separate_from_demontage() {
        let items = build_line_items(&Services { assembly: true, ..Default::default() }, &ServicePrices::defaults());
        assert!(items.iter().any(|i| i.description == "Montage"), "should have Montage");
        assert!(!items.iter().any(|i| i.description == "Demontage"), "should NOT have Demontage");
    }

    #[test]
    fn both_services_both_items() {
        let items = build_line_items(&Services { assembly: true, disassembly: true, ..Default::default() }, &ServicePrices::defaults());
        assert!(items.iter().any(|i| i.description == "Demontage"), "should have Demontage");
        assert!(items.iter().any(|i| i.description == "Montage"), "should have Montage");
    }

    #[test]
    fn halteverbot_origin_only() {
        let items = build_line_items(&Services { parking_ban_origin: true, ..Default::default() }, &ServicePrices::defaults());
        let hv = items.iter().find(|i| i.description == "Halteverbotszone").expect("should have halteverbot");
        assert_eq!(hv.quantity, 1.0);
        assert_eq!(hv.remark.as_deref(), Some("Beladestelle"));
    }

    #[test]
    fn halteverbot_destination_only() {
        let items = build_line_items(&Services { parking_ban_destination: true, ..Default::default() }, &ServicePrices::defaults());
        let hv = items.iter().find(|i| i.description == "Halteverbotszone").expect("should have halteverbot");
        assert_eq!(hv.quantity, 1.0);
        assert_eq!(hv.remark.as_deref(), Some("Entladestelle"));
    }

    #[test]
    fn halteverbot_both() {
        let items = build_line_items(&Services { parking_ban_origin: true, parking_ban_destination: true, ..Default::default() }, &ServicePrices::defaults());
        let hv = items.iter().find(|i| i.description == "Halteverbotszone").expect("should have halteverbot");
        assert_eq!(hv.quantity, 2.0);
        assert_eq!(hv.remark.as_deref(), Some("Beladestelle + Entladestelle"));
    }

    #[test]
    fn umzugsmaterial_remark() {
        let items = build_line_items(&Services { packing: true, ..Default::default() }, &ServicePrices::defaults());
        let um = items.iter().find(|i| i.description == "Umzugsmaterial").expect("should have umzugsmaterial");
        assert_eq!(um.unit_price, 30.0);
        assert_eq!(um.remark.as_deref(), Some("Stretchfolie, Decken, Gurte Einzelpreis 30,00 €"));
    }

    #[test]
    fn packing_triggers_umzugsmaterial() {
        let items = build_line_items(&Services { packing: true, ..Default::default() }, &ServicePrices::defaults());
        assert!(items.iter().any(|i| i.description == "Umzugsmaterial"));
    }

    #[test]
    fn versicherung_zero_price() {
        let items = build_line_items(&Services::default(), &ServicePrices::defaults());
        let v = items.iter().find(|i| i.description == "Nürnbergerversicherung").unwrap();
        assert_eq!(v.quantity, 1.0);
        assert_eq!(v.unit_price, 0.0);
        assert_eq!(v.flat_total, Some(0.0));
    }

    // --- format_services_display tests ---

    #[test]
    fn display_services_empty() {
        let s = format_services_display(&Services::default());
        assert!(s.is_empty());
    }

    #[test]
    fn display_services_all() {
        let s = format_services_display(&Services {
            packing: true,
            assembly: true,
            disassembly: true,
            storage: true,
            disposal: true,
            parking_ban_origin: true,
            parking_ban_destination: true,
            transporter: true,
        });
        assert!(s.contains("Verpackungsservice"));
        assert!(s.contains("Montage"));
        assert!(s.contains("Demontage"));
        assert!(s.contains("Einlagerung"));
        assert!(s.contains("Entsorgung"));
        assert!(s.contains("Halteverbot Beladestelle"));
        assert!(s.contains("Halteverbot Entladestelle"));
    }

    #[test]
    fn display_services_partial() {
        let s = format_services_display(&Services {
            assembly: true,
            parking_ban_origin: true,
            ..Default::default()
        });
        assert!(s.contains("Montage"));
        assert!(s.contains("Halteverbot Beladestelle"));
        assert!(!s.contains("Demontage"));
    }

    // --- detect_salutation_and_greeting tests ---

    #[test]
    fn salutation_explicit_herr() {
        let (sal, greet) = detect_salutation_and_greeting("Herr Müller");
        assert_eq!(sal, "Herrn");
        assert_eq!(greet, "Sehr geehrter Herr Müller,");
    }

    #[test]
    fn salutation_explicit_frau() {
        let (sal, greet) = detect_salutation_and_greeting("Frau Schmidt");
        assert_eq!(sal, "Frau");
        assert_eq!(greet, "Sehr geehrte Frau Schmidt,");
    }

    #[test]
    fn salutation_female_first_name() {
        let (sal, greet) = detect_salutation_and_greeting("Anna Müller");
        assert_eq!(sal, "Frau");
        assert_eq!(greet, "Sehr geehrte Frau Müller,");
    }

    #[test]
    fn salutation_male_first_name() {
        let (sal, greet) = detect_salutation_and_greeting("Thomas Müller");
        assert_eq!(sal, "Herrn");
        assert_eq!(greet, "Sehr geehrter Herr Müller,");
    }

    #[test]
    fn salutation_single_name() {
        let (sal, greet) = detect_salutation_and_greeting("Müller");
        assert_eq!(sal, "");
        assert_eq!(greet, "Sehr geehrte Damen und Herren,");
    }

    #[test]
    fn salutation_unknown_first_name() {
        let (sal, greet) = detect_salutation_and_greeting("Xandr Müller");
        assert_eq!(sal, "Herrn");
        assert_eq!(greet, "Sehr geehrter Herr Müller,");
    }

    #[test]
    fn salutation_whitespace_handling() {
        let (sal, greet) = detect_salutation_and_greeting("  Frau Schmidt  ");
        assert_eq!(sal, "Frau");
        assert_eq!(greet, "Sehr geehrte Frau Schmidt,");
    }

    // --- label_to_german tests ---

    #[test]
    fn german_label_sofa() {
        assert_eq!(label_to_german("sofa"), Some("Sofa, Couch, Liege"));
    }

    #[test]
    fn german_label_case_insensitive() {
        assert_eq!(label_to_german("SOFA"), Some("Sofa, Couch, Liege"));
    }

    #[test]
    fn german_label_unknown() {
        assert_eq!(label_to_german("xyzabc"), None);
    }

    #[test]
    fn german_label_all_categories() {
        // One from each major category
        assert!(label_to_german("chair").is_some(), "seating");
        assert!(label_to_german("desk").is_some(), "tables");
        assert!(label_to_german("bed").is_some(), "beds");
        assert!(label_to_german("wardrobe").is_some(), "storage");
        assert!(label_to_german("tv").is_some(), "electronics");
        assert!(label_to_german("fridge").is_some(), "appliances");
        assert!(label_to_german("box").is_some(), "boxes");
        assert!(label_to_german("piano").is_some(), "instruments");
        assert!(label_to_german("plant").is_some(), "misc");
    }

    #[test]
    fn german_label_aliases() {
        // "couch" and "sofa" map to same
        assert_eq!(label_to_german("couch"), label_to_german("sofa"));
    }

    // --- format_city tests ---

    #[test]
    fn format_city_with_postal() {
        let addr = AddressRow {
            id: uuid::Uuid::nil(),
            street: "Musterstr. 1".to_string(),
            city: "Hildesheim".to_string(),
            postal_code: Some("31134".to_string()),
            floor: None,
            elevator: None,
            house_number: None,
            parking_ban: false,
        };
        assert_eq!(format_city(&addr), "31134 Hildesheim");
    }

    #[test]
    fn format_city_without_postal() {
        let addr = AddressRow {
            id: uuid::Uuid::nil(),
            street: "Musterstr. 1".to_string(),
            city: "Hildesheim".to_string(),
            postal_code: None,
            floor: None,
            elevator: None,
            house_number: None,
            parking_ban: false,
        };
        assert_eq!(format_city(&addr), "Hildesheim");
    }

    #[test]
    fn format_city_empty_postal() {
        let addr = AddressRow {
            id: uuid::Uuid::nil(),
            street: "Musterstr. 1".to_string(),
            city: "Hildesheim".to_string(),
            postal_code: Some("".to_string()),
            floor: None,
            elevator: None,
            house_number: None,
            parking_ban: false,
        };
        assert_eq!(format_city(&addr), " Hildesheim");
    }

    // --- proptests ---

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn build_line_items_never_panics(
            packing in proptest::bool::ANY,
            assembly in proptest::bool::ANY,
            disassembly in proptest::bool::ANY,
            storage in proptest::bool::ANY,
            disposal in proptest::bool::ANY,
            ban_origin in proptest::bool::ANY,
            ban_dest in proptest::bool::ANY,
            transporter in proptest::bool::ANY,
        ) {
            let services = Services {
                packing, assembly, disassembly, storage, disposal,
                parking_ban_origin: ban_origin,
                parking_ban_destination: ban_dest,
                transporter,
            };
            let items = build_line_items(&services, &ServicePrices::defaults());
            // Must always end with Nürnbergerversicherung
            assert!(!items.is_empty());
            let last = items.last().unwrap();
            assert_eq!(last.description, "Nürnbergerversicherung");
        }

        #[test]
        fn detect_salutation_never_panics(s in ".*") {
            let _ = detect_salutation_and_greeting(&s);
        }

        #[test]
        fn label_to_german_never_panics(s in ".*") {
            let _ = label_to_german(&s);
        }

        #[test]
        fn parse_detected_items_never_panics(val in proptest::arbitrary::any::<String>()) {
            // Create arbitrary JSON from the string (will usually fail to deserialize, but shouldn't panic)
            let json_val = serde_json::Value::String(val);
            let est = VolumeEstimationRow {
                result_data: Some(json_val),
                source_data: None,
                total_volume_m3: None,
                method: "test".to_string(),
            };
            let _ = parse_detected_items(Some(&est));
        }
    }

    // --- calculate_rate_override tests ---

    #[test]
    fn rate_no_override_returns_default() {
        let rate = calculate_rate_override(None, None, 2, 4.0, &[]);
        assert!((rate - 30.0).abs() < 0.001, "default rate should be 30");
    }

    #[test]
    fn rate_explicit_override_used_directly() {
        let rate = calculate_rate_override(None, Some(45.0), 2, 4.0, &[]);
        assert!((rate - 45.0).abs() < 0.001);
    }

    #[test]
    fn rate_explicit_wins_over_price() {
        // When both are set, explicit rate wins
        let rate = calculate_rate_override(Some(100_00), Some(50.0), 2, 4.0, &[]);
        assert!((rate - 50.0).abs() < 0.001);
    }

    #[test]
    fn rate_back_calculated_no_other_items() {
        // Target €400 netto, 2 persons × 4 hours, no other items → rate = 400/(2×4) = 50
        let rate = calculate_rate_override(Some(40_000), None, 2, 4.0, &[]);
        assert!((rate - 50.0).abs() < 0.001, "expected 50.0, got {rate}");
    }

    #[test]
    fn rate_back_calculated_with_other_items() {
        // Target €400 netto, other items = €60 (Halteverbot), labor = €340, 2 persons × 4hrs → rate = 340/8 = 42.5
        let items = vec![OfferLineItem {
            description: "Halteverbotszone".to_string(),
            quantity: 1.0,
            unit_price: 60.0,
            is_labor: false,
            ..Default::default()
        }];
        let rate = calculate_rate_override(Some(40_000), None, 2, 4.0, &items);
        assert!((rate - 42.5).abs() < 0.001, "expected 42.5, got {rate}");
    }

    #[test]
    fn flat_total_excluded_from_other_items() {
        // Fahrkostenpauschale with flat_total=60 should be included in other_items via flat_total
        let items = vec![OfferLineItem {
            description: "Fahrkostenpauschale".to_string(),
            flat_total: Some(60.0),
            ..Default::default()
        }];
        // Target €400 netto, other items = €60 flat, labor = €340, 2 persons × 4hrs → rate = 42.5
        let rate = calculate_rate_override(Some(40_000), None, 2, 4.0, &items);
        assert!((rate - 42.5).abs() < 0.001, "flat_total should be subtracted from labor budget");
    }

    #[test]
    fn versicherung_zero_no_effect_on_rate() {
        // Nürnbergerversicherung flat_total=0 should not change rate
        let items = vec![OfferLineItem {
            description: "Nürnbergerversicherung".to_string(),
            flat_total: Some(0.0),
            ..Default::default()
        }];
        let rate = calculate_rate_override(Some(40_000), None, 2, 4.0, &items);
        // No change: 40_000/100 / (2*4) = 50
        assert!((rate - 50.0).abs() < 0.001);
    }

    #[test]
    fn rate_back_calculated_saturates_at_zero() {
        // Target €50 netto, other items = €100 — labor cost can't be negative → rate = 0
        let items = vec![OfferLineItem {
            description: "Halteverbot".to_string(),
            quantity: 1.0,
            unit_price: 100.0,
            is_labor: false,
            ..Default::default()
        }];
        let rate = calculate_rate_override(Some(5_000), None, 2, 4.0, &items);
        assert!(rate >= 0.0, "rate must not be negative");
        assert!((rate - 0.0).abs() < 0.001);
    }

    #[test]
    fn rate_back_calculated_persons_zero_uses_one() {
        // persons=0 should use 1 to avoid division by zero
        let rate = calculate_rate_override(Some(40_000), None, 0, 4.0, &[]);
        assert!(rate.is_finite() && rate > 0.0);
    }

    #[test]
    fn rate_back_calculated_hours_zero_uses_one() {
        // hours=0 should use 1.0 to avoid division by zero
        let rate = calculate_rate_override(Some(40_000), None, 2, 0.0, &[]);
        assert!(rate.is_finite() && rate > 0.0);
    }

    proptest! {
        #[test]
        fn calculate_rate_override_never_panics(
            price in proptest::option::of(0i64..1_000_000i64),
            rate in proptest::option::of(0.0f64..500.0f64),
            persons in 0u32..10u32,
            hours in 0.0f64..24.0f64,
        ) {
            let result = calculate_rate_override(price, rate, persons, hours, &[]);
            assert!(result.is_finite());
            assert!(result >= 0.0);
        }
    }

    // --- M2: Configurable pricing tests ---

    #[test]
    fn configurable_service_prices_affect_line_items() {
        let custom_prices = ServicePrices {
            assembly_unit_price: 50.0,
            parking_ban_unit_price: 150.0,
            packing_unit_price: 40.0,
            transporter_unit_price: 80.0,
        };
        let services = Services {
            disassembly: true,
            assembly: true,
            parking_ban_origin: true,
            packing: true,
            ..Default::default()
        };
        let items = build_line_items(&services, &custom_prices);

        let demontage = items.iter().find(|i| i.description == "Demontage").expect("should have demontage");
        assert_eq!(demontage.unit_price, 50.0, "custom assembly price should apply to Demontage");

        let montage = items.iter().find(|i| i.description == "Montage").expect("should have montage");
        assert_eq!(montage.unit_price, 50.0, "custom assembly price should apply to Montage");

        let hv = items.iter().find(|i| i.description == "Halteverbotszone").expect("should have halteverbot");
        assert_eq!(hv.unit_price, 150.0, "custom parking ban price should apply");

        let um = items.iter().find(|i| i.description == "Umzugsmaterial").expect("should have umzugsmaterial");
        assert_eq!(um.unit_price, 40.0, "custom packing price should apply");
    }

    #[test]
    fn saturday_surcharge_configurable() {
        let engine = PricingEngine::with_rate(3000, 7000);
        let mut input = PricingInput {
            volume_m3: 10.0,
            distance_km: 0.0,
            scheduled_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()), // Saturday
            floor_origin: None,
            floor_destination: None,
            has_elevator_origin: None,
            has_elevator_destination: None,
            floor_stop: None,
            has_elevator_stop: None,
        };
        let result = engine.calculate(&input);
        assert_eq!(result.breakdown.date_adjustment_cents, 7_000, "custom Saturday surcharge should apply");

        // Sunday should still be 0 regardless
        input.scheduled_date = Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
        let result2 = engine.calculate(&input);
        assert_eq!(result2.breakdown.date_adjustment_cents, 0, "no surcharge on Sunday");
    }
}
