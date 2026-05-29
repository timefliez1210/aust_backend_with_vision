//! Offer pipeline — auto-generates offers once a quote has enough data.
//!
//! Checks readiness (volume > 0), auto-calculates distance if missing,
//! then calls `build_offer` and forwards the PDF to Telegram.

use crate::repositories::{address_repo, inquiry_repo, offer_repo};
use crate::services::offer_builder::build_offer;
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
    // Check if an offer already exists for this quote.
    // NOTE: A UNIQUE partial index (`offers_inquiry_active_unique`) on offers(inquiry_id)
    //       WHERE status NOT IN ('rejected','cancelled') prevents duplicate active offers
    //       even under concurrent access. The check below is an optimization to avoid
    //       unnecessary work; the DB constraint is the true guard.
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
    if q.distance_km.unwrap_or(0.0) == 0.0
        && let (Some(origin_id), Some(dest_id)) = (q.origin_address_id, q.destination_address_id) {
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
                if let Some(stop_id) = q.stop_address_id
                    && let Some(stop) = address_repo::fetch_street_city(&state.db, stop_id).await.ok().flatten() {
                        route_addresses.push(fmt_addr(&stop));
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

    info!("Auto-generating offer for quote {inquiry_id}");

    match build_offer(&state.db, &*state.storage, &state.config, inquiry_id, Some(30)).await {
        Ok(generated) => {
            let offer_id = generated.offer.id;
            let customer_id = generated.offer.inquiry_id; // used below for event payload
            info!(
                "Offer {} generated for quote {inquiry_id} (€{:.2})",
                offer_id,
                generated.offer.price_cents as f64 / 100.0
            );

            // Emit offer.drafted domain event (non-fatal).
            {
                let emitter = state.events.clone();
                let payload = serde_json::json!({
                    "offer_id": offer_id,
                    "inquiry_id": inquiry_id,
                    // customer_id is not directly available here; callers can look it up via inquiry
                });
                let _ = customer_id; // suppress unused warning
                let aggregate = format!("offer:{offer_id}");
                tokio::spawn(async move {
                    if let Err(e) = emitter.emit("offer.drafted", &aggregate, payload).await {
                        tracing::warn!("Failed to emit offer.drafted event: {e}");
                    }
                });
            }

            // Feature flag: when agent_owns_approval=true, skip the legacy Telegram post.
            // The offer.drafted event was already emitted above; the agent's
            // handle_offer_drafted handler will post the approval message.
            // Rollback: `UPDATE settings SET value = 'false' WHERE key = 'agent_owns_approval';`
            let agent_owns = crate::repositories::settings_repo::agent_owns_approval(&state.db).await;

            // Store the approval routing decision on the offer row at draft time (B4).
            // The event consumer reads `offers.approval_owner` instead of re-reading the flag,
            // so flipping the flag mid-flight cannot produce double-posts or lost-posts.
            let approval_owner_str = if agent_owns { "agent" } else { "legacy" };
            if let Err(e) = sqlx::query(
                "UPDATE offers SET approval_owner = $1 WHERE id = $2"
            )
            .bind(approval_owner_str)
            .bind(offer_id)
            .execute(&state.db)
            .await
            {
                tracing::warn!("Failed to set approval_owner on offer {offer_id}: {e}");
            }

            if agent_owns {
                info!("Skipping legacy Telegram approval post — agent_owns_approval=true. Event consumer will handle it.");
            } else {
                send_offer_to_telegram(&state.config.telegram, &generated).await;
            }
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
