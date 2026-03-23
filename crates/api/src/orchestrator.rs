//! Offer orchestrator — event loop that dispatches Telegram approval/edit events and
//! handles the email-to-inquiry pipeline (`handle_complete_inquiry`).
//!
//! Heavy lifting is delegated to focused service modules:
//! - `services::offer_pipeline`   — auto-generation + distance calculation
//! - `services::telegram_service` — PDF sending, approval/edit/denial handlers
//! - `services::email_dispatch`   — SMTP sending + email thread management

use crate::repositories::{address_repo, customer_repo, estimation_repo, inquiry_repo, offer_repo};
use crate::services;
use crate::services::telegram_service::{
    handle_offer_approval, handle_offer_denial, handle_offer_edit, send_telegram_message,
};
use crate::AppState;
use aust_core::models::{MovingInquiry, Services};
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use reqwest::Client;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

pub use crate::services::email_dispatch::{send_offer_email, send_offer_email_custom};
pub use crate::services::offer_pipeline::try_auto_generate_offer;

use aust_email_agent::ApprovalDecision;

/// State for the offer currently being edited.
struct EditingOffer {
    offer_id: Uuid,
    inquiry_id: Uuid,
}

/// Long-running background task that processes offer lifecycle events from the Telegram poller.
///
/// **Caller**: `main.rs`, spawned as a Tokio task at startup alongside the email agent.
/// **Why**: The email agent's Telegram poller runs in a separate task and forwards button
/// callbacks and free-text messages via an unbounded mpsc channel. This task owns the
/// event loop that routes each event to the appropriate handler.
///
/// # Event dispatch
///
/// | Event | Action |
/// |---|---|
/// | `InquiryComplete(inquiry)` | `handle_complete_inquiry` — full DB pipeline + offer generation |
/// | `OfferApprove(id)` | `handle_offer_approval` — email PDF to customer |
/// | `OfferEdit(id)` | Enter edit mode, prompt Alex for instructions |
/// | `OfferEditText(text)` | `handle_offer_edit` — LLM parse + regenerate offer |
/// | `OfferDeny(id)` | `handle_offer_denial` — mark offer rejected |
///
/// Edit-mode state is tracked locally in `editing: Option<EditingOffer>`. Free-text messages
/// are only consumed when edit mode is active; otherwise they are silently ignored.
/// Typing "Abbrechen" or "Cancel" exits edit mode without regenerating.
///
/// # Parameters
/// - `state` — shared application state
/// - `rx` — receiving end of the mpsc channel from the email agent's Telegram poller
pub async fn run_offer_event_handler(
    state: Arc<AppState>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<ApprovalDecision>,
) {
    let client = Client::new();
    let bot_token = &state.config.telegram.bot_token;
    let chat_id = state.config.telegram.admin_chat_id;
    let mut editing: Option<EditingOffer> = None;

    info!("Offer event handler started");

    while let Some(event) = rx.recv().await {
        match event {
            ApprovalDecision::OfferApprove(id_str) => {
                if let Ok(offer_id) = Uuid::parse_str(&id_str) {
                    editing = None;
                    handle_offer_approval(&state, &client, bot_token, chat_id, offer_id).await;
                }
            }
            ApprovalDecision::OfferEdit(id_str) => {
                if let Ok(offer_id) = Uuid::parse_str(&id_str) {
                    let inquiry_id = offer_repo::fetch_inquiry_id(&state.db, offer_id)
                        .await
                        .unwrap_or(None);

                    if let Some(qid) = inquiry_id {
                        editing = Some(EditingOffer {
                            offer_id,
                            inquiry_id: qid,
                        });
                        send_telegram_message(
                            &client,
                            bot_token,
                            chat_id,
                            "✏️ Was soll am Angebot geändert werden?\n\n\
                             Beispiele:\n\
                             • \"Preis auf 800 Euro\"\n\
                             • \"4 Helfer, 6 Stunden\"\n\
                             • \"Stundensatz 35\"\n\
                             • \"Abbrechen\"",
                        )
                        .await;
                    }
                }
            }
            ApprovalDecision::OfferDeny(id_str) => {
                if let Ok(offer_id) = Uuid::parse_str(&id_str) {
                    editing = None;
                    handle_offer_denial(&state, &client, bot_token, chat_id, offer_id).await;
                }
            }
            ApprovalDecision::InquiryComplete(inquiry) => {
                handle_complete_inquiry(&state, &client, bot_token, chat_id, inquiry).await;
            }
            ApprovalDecision::OfferEditText(text) => {
                let Some(edit_state) = editing.take() else {
                    continue; // not editing an offer, ignore
                };

                let text_lower = text.trim().to_lowercase();
                if text_lower == "abbrechen" || text_lower == "cancel" {
                    send_telegram_message(
                        &client,
                        bot_token,
                        chat_id,
                        "✏️ Bearbeitung abgebrochen.",
                    )
                    .await;
                    continue;
                }

                send_telegram_message(
                    &client,
                    bot_token,
                    chat_id,
                    "⏳ Angebot wird neu erstellt...",
                )
                .await;

                handle_offer_edit(
                    &state,
                    &client,
                    bot_token,
                    chat_id,
                    edit_state.offer_id,
                    edit_state.inquiry_id,
                    &text,
                )
                .await;
            }
            _ => {} // ignore non-offer events
        }
    }
}

/// Handle a fully-parsed moving inquiry: create all DB records and kick off offer generation.
///
/// **Caller**: `run_offer_event_handler` on `ApprovalDecision::InquiryComplete`.
/// **Why**: This is the entry point for the email-to-offer pipeline. A single `MovingInquiry`
/// carries everything the system needs to create the customer record, addresses, quote, volume
/// estimation, and first offer in a single atomic sequence.
///
/// # Pipeline steps
/// 1. Upsert customer by email (name/phone updated if previously unknown).
/// 2. Parse and insert origin address (street/city/postal from free-text).
/// 3. Parse and insert destination address.
/// 4. (Optional) Parse and insert intermediate stop address.
/// 5. Auto-calculate route distance if both addresses are present.
/// 6. Determine volume — use `inquiry.volume_m3` if provided, otherwise apply
///    room-count heuristics from `inquiry.notes` (default 25 m³).
/// 7. Build `Services` struct via `build_services` and insert inquiry
///    with status `"estimated"`.
/// 8. Insert a `volume_estimations` row (method `"manual"`) carrying the parsed items list.
/// 9. Link the most-recent open email thread to the new quote.
/// 10. Delegate to `try_auto_generate_offer` → PDF → Telegram.
///
/// # Parameters
/// - `state` — shared application state
/// - `_client`, `_bot_token`, `_chat_id` — retained for symmetry with other handlers
///   (error notifications are delegated to `try_auto_generate_offer`)
/// - `inquiry` — fully-parsed inquiry from the email agent
///
/// # Errors
/// DB errors in steps 1 and 5 abort early (logged). Address and volume errors are
/// non-fatal and result in `NULL` fields on the quote.
async fn handle_complete_inquiry(
    state: &Arc<AppState>,
    _client: &Client,
    _bot_token: &str,
    _chat_id: i64,
    inquiry: MovingInquiry,
) {
    info!(
        "Processing complete inquiry {} from {}",
        inquiry.id, inquiry.email
    );

    let now = chrono::Utc::now();

    // 1. Create or find customer by email
    // Split "Vorname Nachname" into structured fields for the PDF greeting logic.
    let (inq_first_name, inq_last_name) = inquiry
        .name
        .as_deref()
        .map(|n| {
            let mut parts = n.splitn(2, ' ');
            let first = parts.next().unwrap_or("").to_string();
            let last = parts.next().unwrap_or("").to_string();
            (
                if first.is_empty() { None } else { Some(first) },
                if last.is_empty() { None } else { Some(last) },
            )
        })
        .unwrap_or((None, None));

    let customer_id: Uuid = match customer_repo::upsert(
        &state.db,
        &inquiry.email,
        inquiry.name.as_deref(),
        inquiry.salutation.as_deref(),
        inq_first_name.as_deref(),
        inq_last_name.as_deref(),
        inquiry.phone.as_deref(),
        now,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            error!("Failed to create customer: {e}");
            return;
        }
    };

    // 2. Create origin address (if we have departure address)
    let origin_id = if let Some(ref addr) = inquiry.departure_address {
        let (street, city, postal) = services::vision::parse_address(addr);
        let postal_opt = if postal.is_empty() { None } else { Some(postal.as_str()) };
        match address_repo::create(
            &state.db,
            &street,
            &city,
            postal_opt,
            inquiry.departure_floor.as_deref(),
            inquiry.departure_elevator,
        )
        .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                warn!("Failed to create origin address: {e}");
                None
            }
        }
    } else {
        None
    };

    // 3. Create destination address (if we have arrival address)
    let dest_id = if let Some(ref addr) = inquiry.arrival_address {
        let (street, city, postal) = services::vision::parse_address(addr);
        let postal_opt = if postal.is_empty() { None } else { Some(postal.as_str()) };
        match address_repo::create(
            &state.db,
            &street,
            &city,
            postal_opt,
            inquiry.arrival_floor.as_deref(),
            inquiry.arrival_elevator,
        )
        .await
        {
            Ok(id) => Some(id),
            Err(e) => {
                warn!("Failed to create destination address: {e}");
                None
            }
        }
    } else {
        None
    };

    // 3b. Create intermediate stop address (if any)
    let stop_id = if inquiry.has_intermediate_stop {
        if let Some(ref addr) = inquiry.intermediate_address {
            let (street, city, postal) = services::vision::parse_address(addr);
            let postal_opt = if postal.is_empty() { None } else { Some(postal.as_str()) };
            match address_repo::create(
                &state.db,
                &street,
                &city,
                postal_opt,
                inquiry.intermediate_floor.as_deref(),
                inquiry.intermediate_elevator,
            )
            .await
            {
                Ok(id) => Some(id),
                Err(e) => {
                    warn!("Failed to create stop address: {e}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // 3c. Calculate distance if both addresses exist (include intermediate stop in route)
    let distance_km = if let (Some(ref dep), Some(ref arr)) =
        (&inquiry.departure_address, &inquiry.arrival_address)
    {
        let calculator = RouteCalculator::new(state.config.maps.api_key.clone());
        let mut route_addresses = vec![dep.clone()];
        if inquiry.has_intermediate_stop {
            if let Some(ref stop_addr) = inquiry.intermediate_address {
                route_addresses.push(stop_addr.clone());
            }
        }
        route_addresses.push(arr.clone());
        match calculator
            .calculate(&RouteRequest {
                addresses: route_addresses,
            })
            .await
        {
            Ok(result) => {
                info!(
                    "Distance calculated: {:.1} km between {} and {}",
                    result.total_distance_km, dep, arr
                );
                Some(result.total_distance_km)
            }
            Err(e) => {
                warn!("Distance calculation failed (will generate offer without distance): {e}");
                None
            }
        }
    } else {
        None
    };

    // 4. Determine volume — use provided volume, or rough estimate from items/description
    let volume_m3 = inquiry.volume_m3.unwrap_or_else(|| {
        // Rough estimate: typical apartment sizes
        if let Some(ref notes) = inquiry.notes {
            let notes_lower = notes.to_lowercase();
            if notes_lower.contains("haus") || notes_lower.contains("einfamilienhaus") {
                50.0
            } else if notes_lower.contains("4-zimmer") || notes_lower.contains("4 zimmer") {
                40.0
            } else if notes_lower.contains("3-zimmer") || notes_lower.contains("3 zimmer") {
                30.0
            } else if notes_lower.contains("2-zimmer") || notes_lower.contains("2 zimmer") {
                20.0
            } else if notes_lower.contains("1-zimmer") || notes_lower.contains("1 zimmer") || notes_lower.contains("studio") {
                15.0
            } else {
                25.0 // default estimate
            }
        } else {
            25.0
        }
    });

    // 5. Create quote
    let inquiry_id = Uuid::now_v7();
    let preferred_date_ts = inquiry
        .preferred_date
        .map(|d| d.and_hms_opt(10, 0, 0).unwrap())
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    let inquiry_services = build_services(&inquiry);
    let services_json = serde_json::to_value(&inquiry_services).unwrap_or_default();
    // notes now contains ONLY the customer's free-text message
    let notes = inquiry.notes.clone();
    let source = serde_json::to_value(&inquiry.source)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "direct_email".to_string());

    if let Err(e) = inquiry_repo::create(
        &state.db,
        inquiry_id,
        customer_id,
        origin_id,
        dest_id,
        stop_id,
        "estimated",
        Some(volume_m3),
        distance_km,
        preferred_date_ts,
        notes.as_deref(),
        &services_json,
        &source,
        now,
    )
    .await
    {
        error!("Failed to create quote: {e}");
        return;
    }

    // 6. Create a volume estimation record (manual, from inquiry data)
    let estimation_id = Uuid::now_v7();
    let source_data = serde_json::json!({
        "source": "email_inquiry",
        "inquiry_id": inquiry.id.to_string(),
        "items_list": inquiry.items_list,
    });

    // Parse items_list text into structured result_data for the "Erfasste Gegenstände" sheet
    let result_data = inquiry
        .items_list
        .as_deref()
        .map(|text| {
            let items = parse_items_list_text(text);
            serde_json::to_value(&items).ok()
        })
        .flatten();

    if let Err(e) = estimation_repo::insert_no_return(
        &state.db,
        estimation_id,
        inquiry_id,
        "manual",
        &source_data,
        result_data.as_ref(),
        volume_m3,
        0.5f64, // lower confidence for rough estimate
        now,
    )
    .await
    {
        warn!("Failed to insert volume_estimations for quote {inquiry_id}: {e}");
    }

    info!(
        "Created quote {inquiry_id} for customer {} ({}) — {:.1} m³",
        inquiry.name.as_deref().unwrap_or("?"),
        inquiry.email,
        volume_m3
    );

    // Link email thread to quote (if thread exists for this customer)
    let _ = inquiry_repo::link_email_thread(&state.db, inquiry_id, customer_id).await;

    // 7. Generate offer → PDF → Telegram
    try_auto_generate_offer(Arc::clone(state), inquiry_id).await;
}

/// Parsed item from the VolumeCalculator items_list text.
/// Matches the format: "2x Sofa, Couch, Liege je Sitz (0.80 m³)"
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedInventoryItem {
    pub name: String,
    pub quantity: u32,
    pub volume_m3: f64,
}

/// Parse the VolumeCalculator `items_list` string into a structured list of inventory items.
///
/// **Caller**: `handle_complete_inquiry` when `inquiry.items_list` is present; the result is
/// stored as `result_data` on the `volume_estimations` row and later used by `XlsxGenerator`
/// to populate the "Erfasste Gegenstände" sheet in the offer XLSX.
/// **Why**: The customer-facing VolumeCalculator app produces a flat text representation of
/// the inventory. This parser normalises it so the data can be stored and rendered in the
/// offer template without any further string manipulation.
///
/// # Accepted formats
/// - Newline-separated: `"1x Bettumbau (0.30 m³)\n1x Nachttisch (0.20 m³)"`
/// - Comma-separated:   `"1x Bettumbau (0.30 m³), 1x Nachttisch (0.20 m³)"`
/// - Mixed: a line may itself contain multiple comma-separated items.
///
/// Per-item parsing:
/// - Quantity prefix: `"2x "` or `"2 x "` at the start of an item.
/// - Volume suffix: parenthesized notation `"(0.80 m³)"` or German decimal `"(0,80 m³)"`.
///   Accepted unit spellings: `m³` and `m3`.
///
/// # Parameters
/// - `text` — raw items_list string from `MovingInquiry.items_list`
///
/// # Returns
/// Zero or more `ParsedInventoryItem` values. Items with an empty name after stripping are
/// discarded. Items without a parseable volume get `volume_m3 = 0.0`.
pub fn parse_items_list_text(text: &str) -> Vec<ParsedInventoryItem> {
    let mut items = Vec::new();

    // Normalize: split on newlines first, then within each line split on ", Nx " boundaries
    // to handle comma-separated items
    let mut raw_items: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Split on ", Nx " pattern (comma followed by digit+x)
        // e.g., "1x Bett (0.30 m³), 1x Tisch (0.50 m³)" → ["1x Bett (0.30 m³)", "1x Tisch (0.50 m³)"]
        let mut remaining = line;
        loop {
            // Find ", Nx " boundary — look for ", " followed by digits and "x"
            let mut split_pos = None;
            if let Some(comma_pos) = remaining.find(", ") {
                let after_comma = &remaining[comma_pos + 2..];
                let digits: String = after_comma.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    let after_digits = &after_comma[digits.len()..];
                    if after_digits.starts_with('x') || after_digits.starts_with(" x") {
                        split_pos = Some(comma_pos);
                    }
                }
            }
            if let Some(pos) = split_pos {
                let item = remaining[..pos].trim();
                if !item.is_empty() {
                    raw_items.push(item.to_string());
                }
                remaining = remaining[pos + 2..].trim(); // skip ", "
            } else {
                if !remaining.is_empty() {
                    raw_items.push(remaining.to_string());
                }
                break;
            }
        }
    }

    for item_str in &raw_items {
        let mut quantity = 1u32;
        let mut name = item_str.to_string();
        let mut volume = 0.0f64;

        // Try to extract quantity: "2x " or "2 x " at the start
        let digits: String = item_str.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Ok(qty) = digits.parse::<u32>() {
                let after_digits = &item_str[digits.len()..];
                if after_digits.starts_with('x') || after_digits.starts_with(" x") {
                    quantity = qty;
                    name = after_digits
                        .strip_prefix('x')
                        .or_else(|| after_digits.strip_prefix(" x "))
                        .or_else(|| after_digits.strip_prefix(" x"))
                        .unwrap_or(after_digits)
                        .trim()
                        .to_string();
                }
            }
        }

        // Try to extract volume from parenthesized notation: "(0.80 m³)" or "(0,80 m³)"
        if let Some(paren_start) = name.rfind('(') {
            if let Some(paren_end) = name[paren_start..].find(')') {
                let inside = &name[paren_start + 1..paren_start + paren_end];
                let vol_str = inside
                    .replace("m³", "")
                    .replace("m3", "")
                    .replace(',', ".")
                    .trim()
                    .to_string();
                if let Ok(v) = vol_str.parse::<f64>() {
                    volume = v;
                    name = name[..paren_start].trim().to_string();
                }
            }
        }

        if !name.is_empty() {
            items.push(ParsedInventoryItem {
                name,
                quantity,
                volume_m3: volume,
            });
        }
    }

    items
}

/// Build a `Services` struct from a `MovingInquiry`.
///
/// **Caller**: `handle_complete_inquiry`, step 7.
/// **Why**: The line-item builder (`build_line_items` in `offers.rs`) uses the structured
/// `Services` flags to determine which surcharge rows to include in the XLSX offer
/// (parking bans, packing service, assembly/disassembly, etc.). Stored as JSONB in the
/// `inquiries.services` column.
///
/// # Parameters
/// - `inquiry` — the fully-parsed `MovingInquiry` whose boolean service flags are mapped
///
/// # Returns
/// A `Services` struct with each field set from the corresponding inquiry flag.
fn build_services(inquiry: &MovingInquiry) -> Services {
    Services {
        packing: inquiry.service_packing,
        assembly: inquiry.service_assembly,
        disassembly: inquiry.service_disassembly,
        storage: inquiry.service_storage,
        disposal: inquiry.service_disposal,
        parking_ban_origin: inquiry.departure_parking_ban.unwrap_or(false),
        parking_ban_destination: inquiry.arrival_parking_ban.unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_item_with_volume() {
        let items = parse_items_list_text("1x Sofa (0.80 m³)");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Sofa");
        assert_eq!(items[0].quantity, 1);
        assert!((items[0].volume_m3 - 0.80).abs() < 0.001);
    }

    #[test]
    fn parse_multiple_items_newline() {
        let items = parse_items_list_text("1x Sofa (0.80 m³)\n1x Tisch (0.50 m³)");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Sofa");
        assert_eq!(items[1].name, "Tisch");
    }

    #[test]
    fn parse_comma_separated() {
        let items = parse_items_list_text("1x Bett (0.30 m³), 1x Tisch (0.50 m³)");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Bett");
        assert_eq!(items[1].name, "Tisch");
    }

    #[test]
    fn parse_quantity_greater_than_one() {
        let items = parse_items_list_text("3x Stuhl (0.20 m³)");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].quantity, 3);
        assert_eq!(items[0].name, "Stuhl");
    }

    #[test]
    fn parse_comma_decimal() {
        let items = parse_items_list_text("1x Sofa (0,80 m³)");
        assert_eq!(items.len(), 1);
        assert!((items[0].volume_m3 - 0.80).abs() < 0.001, "German comma decimal");
    }

    #[test]
    fn parse_m3_variants() {
        let items1 = parse_items_list_text("1x Sofa (0.80 m³)");
        let items2 = parse_items_list_text("1x Sofa (0.80 m3)");
        assert!((items1[0].volume_m3 - items2[0].volume_m3).abs() < 0.001);
    }

    #[test]
    fn parse_no_volume() {
        let items = parse_items_list_text("1x Sofa");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Sofa");
        assert!((items[0].volume_m3 - 0.0).abs() < 0.001);
    }

    #[test]
    fn parse_empty_input() {
        let items = parse_items_list_text("");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_mixed_format() {
        let items = parse_items_list_text("1x Sofa (0.80 m³)\n2x Stuhl (0.20 m³), 1x Tisch (0.50 m³)");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn parse_space_x_separator() {
        let items = parse_items_list_text("2 x Sofa (0.80 m³)");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].quantity, 2);
    }

    // Proptests
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn parse_items_never_panics(s in ".*") {
            let _ = parse_items_list_text(&s);
        }

        #[test]
        fn parse_items_structured_fuzz(
            s in "[0-9]{0,3}x? ?[a-zA-Z ]{0,30}( ?\\(?[0-9.,]+ ?m[³3]?\\)?)?"
        ) {
            let _ = parse_items_list_text(&s);
        }
    }

    // ---------------------------------------------------------------
    // build_services tests
    // ---------------------------------------------------------------

    fn minimal_inquiry() -> MovingInquiry {
        MovingInquiry {
            id: uuid::Uuid::nil(),
            email: "test@example.com".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn services_empty_inquiry() {
        let inquiry = minimal_inquiry();
        let services = build_services(&inquiry);
        assert_eq!(services, Services::default());
    }

    #[test]
    fn services_parking_ban_origin() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_parking_ban = Some(true);
        let services = build_services(&inquiry);
        assert!(services.parking_ban_origin);
        assert!(!services.parking_ban_destination);
    }

    #[test]
    fn services_parking_ban_destination() {
        let mut inquiry = minimal_inquiry();
        inquiry.arrival_parking_ban = Some(true);
        let services = build_services(&inquiry);
        assert!(!services.parking_ban_origin);
        assert!(services.parking_ban_destination);
    }

    #[test]
    fn services_parking_ban_none_defaults_false() {
        let inquiry = minimal_inquiry();
        let services = build_services(&inquiry);
        assert!(!services.parking_ban_origin);
        assert!(!services.parking_ban_destination);
    }

    #[test]
    fn services_all_flags() {
        let mut inquiry = minimal_inquiry();
        inquiry.service_packing = true;
        inquiry.service_assembly = true;
        inquiry.service_disassembly = true;
        inquiry.service_storage = true;
        inquiry.service_disposal = true;
        let services = build_services(&inquiry);
        assert!(services.packing);
        assert!(services.assembly);
        assert!(services.disassembly);
        assert!(services.storage);
        assert!(services.disposal);
    }

    #[test]
    fn services_partial_flags() {
        let mut inquiry = minimal_inquiry();
        inquiry.service_packing = true;
        inquiry.service_disassembly = true;
        let services = build_services(&inquiry);
        assert!(services.packing);
        assert!(!services.assembly);
        assert!(services.disassembly);
        assert!(!services.storage);
        assert!(!services.disposal);
    }

    #[test]
    fn services_full_inquiry() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_parking_ban = Some(true);
        inquiry.arrival_parking_ban = Some(true);
        inquiry.service_packing = true;
        inquiry.service_assembly = true;
        inquiry.service_disassembly = true;
        inquiry.service_storage = true;
        inquiry.service_disposal = true;
        let services = build_services(&inquiry);

        assert!(services.packing);
        assert!(services.assembly);
        assert!(services.disassembly);
        assert!(services.storage);
        assert!(services.disposal);
        assert!(services.parking_ban_origin);
        assert!(services.parking_ban_destination);
    }

    #[test]
    fn services_serializes_to_json() {
        let mut inquiry = minimal_inquiry();
        inquiry.service_packing = true;
        inquiry.departure_parking_ban = Some(true);
        let services = build_services(&inquiry);
        let json = serde_json::to_value(&services).unwrap();
        assert_eq!(json["packing"], true);
        assert_eq!(json["parking_ban_origin"], true);
        assert_eq!(json["assembly"], false);
    }

    #[test]
    fn services_notes_not_included() {
        // Notes should NOT affect services — notes is now purely free-text
        let mut inquiry = minimal_inquiry();
        inquiry.notes = Some("Klavier im 1. OG".to_string());
        let services = build_services(&inquiry);
        assert_eq!(services, Services::default());
    }
}
