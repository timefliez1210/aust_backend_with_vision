//! Offer pipeline — auto-generates offers once a quote has enough data.
//!
//! Checks readiness (volume > 0), auto-calculates distance if missing,
//! then calls `build_offer` and forwards the PDF to Telegram.

use crate::repositories::{address_repo, inquiry_repo, offer_repo};
use crate::routes::offers::build_offer;
use crate::services::telegram_service::{notify_telegram_error, send_offer_to_telegram};
use crate::AppState;
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Check whether a quote has enough data to auto-generate an offer, and do so if ready.
///
/// **Caller**: `handle_complete_inquiry` (immediately after quote + volume estimation are stored),
/// and any endpoint that completes a volume estimation (e.g. `estimates::post_inventory`).
/// **Why**: Offers should be generated and forwarded to Telegram without any manual trigger from
/// Alex. The function is idempotent — it exits early if an offer already exists.
///
/// # Readiness criteria
/// - Inquiry must have `estimated_volume_m3 > 0`.
/// - Distance is not required; if missing and both addresses are present, this function
///   automatically runs the route calculator and writes `distance_km` to the inquiry first.
///
/// # Parameters
/// - `state` — shared application state (DB, storage, config)
/// - `inquiry_id` — the inquiry to check and potentially generate an offer for
///
/// # Returns
/// Nothing. Errors are logged and, if critical, forwarded to the admin via Telegram.
pub async fn try_auto_generate_offer(state: Arc<AppState>, inquiry_id: Uuid) {
    // Check if an offer already exists for this quote
    let already_exists = offer_repo::any_exists_for_inquiry(&state.db, inquiry_id)
        .await
        .unwrap_or(false);

    if already_exists {
        info!("Offer already exists for quote {inquiry_id}, skipping auto-generation");
        return;
    }

    // Check that the quote has a volume estimate (minimum requirement); also fetch distance/addresses
    let readiness = inquiry_repo::fetch_readiness(&state.db, inquiry_id)
        .await
        .unwrap_or(None);

    let q = match readiness {
        Some(r) if r.estimated_volume_m3.unwrap_or(0.0) > 0.0 => r,
        _ => {
            info!("Inquiry {inquiry_id} not ready for offer (no volume estimate)");
            return;
        }
    };

    // Auto-calculate distance if both addresses are present and distance is still 0/missing
    if q.distance_km.unwrap_or(0.0) == 0.0 {
        if let (Some(origin_id), Some(dest_id)) = (q.origin_address_id, q.destination_address_id) {
            let fmt_addr = |a: &address_repo::AddressStrRow| format!(
                "{}, {}{}",
                a.street,
                a.postal_code.as_deref().map(|p| format!("{p} ")).unwrap_or_default(),
                a.city
            );

            let origin = address_repo::fetch_street_city(&state.db, origin_id).await.ok().flatten();
            let dest = address_repo::fetch_street_city(&state.db, dest_id).await.ok().flatten();

            if let (Some(origin), Some(dest)) = (origin, dest) {
                let mut route_addresses = vec![fmt_addr(&origin)];
                if let Some(stop_id) = q.stop_address_id {
                    if let Some(stop) = address_repo::fetch_street_city(&state.db, stop_id).await.ok().flatten() {
                        route_addresses.push(fmt_addr(&stop));
                    }
                }
                route_addresses.push(fmt_addr(&dest));

                let calculator = RouteCalculator::new(state.config.maps.api_key.clone());
                match calculator.calculate(&RouteRequest { addresses: route_addresses }).await {
                    Ok(result) => {
                        info!("Distance calculated for quote {inquiry_id}: {:.1} km", result.total_distance_km);
                        let _ = inquiry_repo::update_distance(&state.db, inquiry_id, result.total_distance_km).await;
                    }
                    Err(e) => warn!("Distance calculation for quote {inquiry_id} failed: {e}"),
                }
            }
        }
    }

    info!("Auto-generating offer for quote {inquiry_id}");

    match build_offer(&state.db, &*state.storage, &state.config, inquiry_id, Some(30)).await {
        Ok(generated) => {
            info!(
                "Offer {} generated for quote {inquiry_id} (€{:.2})",
                generated.offer.id,
                generated.offer.price_cents as f64 / 100.0
            );

            send_offer_to_telegram(&state.config.telegram, &generated).await;
        }
        Err(e) => {
            error!("Auto-offer generation failed for inquiry {inquiry_id}: {e}");
            notify_telegram_error(
                &state.config.telegram,
                &format!("Angebotserstellung fehlgeschlagen für Anfrage {inquiry_id}: {e}"),
            )
            .await;
        }
    }
}
