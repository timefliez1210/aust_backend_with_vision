//! Offer orchestrator — auto-generates offers when quotes become ready,
//! sends PDF to Telegram for approval, and emails on approval.
//! Supports edit loop: Alex can press ✏️, type adjustment instructions,
//! and get a regenerated offer.

use crate::routes::offers::{build_offer, build_offer_with_overrides, GeneratedOffer, OfferOverrides};
use crate::{services, AppState};
use aust_core::config::TelegramConfig;
use aust_core::models::MovingInquiry;
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use aust_llm_providers::{LlmMessage, LlmProvider};
use reqwest::{
    multipart::{Form, Part},
    Client,
};
use aust_calendar::NewBooking;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Format an address with floor and elevator info for Telegram display.
/// e.g. "Musterstr. 1, 31135 Hildesheim (3. OG, kein Aufzug)"
fn format_address_line(address: &str, floor: &str, elevator: Option<bool>) -> String {
    let mut parts = Vec::new();
    if !floor.is_empty() {
        parts.push(floor.to_string());
    }
    match elevator {
        Some(true) => parts.push("Aufzug".to_string()),
        Some(false) => parts.push("kein Aufzug".to_string()),
        None => {}
    }
    if parts.is_empty() {
        address.to_string()
    } else {
        format!("{} ({})", address, parts.join(", "))
    }
}

/// Check if a quote has enough data to auto-generate an offer, and do so.
/// Called after volume estimation or distance calculation completes.
pub async fn try_auto_generate_offer(state: Arc<AppState>, quote_id: Uuid) {
    // Check if an offer already exists for this quote
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM offers WHERE quote_id = $1 LIMIT 1")
            .bind(quote_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    if existing.is_some() {
        info!("Offer already exists for quote {quote_id}, skipping auto-generation");
        return;
    }

    // Check that the quote has a volume estimate (minimum requirement)
    let has_volume: Option<(Option<f64>,)> =
        sqlx::query_as("SELECT estimated_volume_m3 FROM quotes WHERE id = $1")
            .bind(quote_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    match has_volume {
        Some((Some(vol),)) if vol > 0.0 => {}
        _ => {
            info!("Quote {quote_id} not ready for offer (no volume estimate)");
            return;
        }
    }

    info!("Auto-generating offer for quote {quote_id}");

    match build_offer(&state.db, &*state.storage, &state.config, quote_id, Some(30)).await {
        Ok(generated) => {
            info!(
                "Offer {} generated for quote {quote_id} (€{:.2})",
                generated.offer.id,
                generated.offer.price_cents as f64 / 100.0
            );

            send_offer_to_telegram(&state.config.telegram, &generated).await;

            // Auto-create tentative booking if quote has a preferred_date
            auto_create_booking(&state, quote_id).await;
        }
        Err(e) => {
            error!("Auto-offer generation failed for quote {quote_id}: {e}");
            notify_telegram_error(
                &state.config.telegram,
                &format!("Angebotserstellung fehlgeschlagen für Quote {quote_id}: {e}"),
            )
            .await;
        }
    }
}

/// Auto-create a tentative booking when an offer is generated, if the quote has a preferred_date.
/// Uses force_create_booking (bypasses capacity) — conflicts are intentional and shown in the dashboard.
/// Failures are logged but never block offer generation.
async fn auto_create_booking(state: &AppState, quote_id: Uuid) {
    // Fetch preferred_date from quote
    let row: Option<(Option<chrono::DateTime<chrono::Utc>>,)> =
        sqlx::query_as("SELECT preferred_date FROM quotes WHERE id = $1")
            .bind(quote_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let booking_date = match row {
        Some((Some(dt),)) => dt.date_naive(),
        _ => {
            info!("Quote {quote_id} has no preferred_date, skipping auto-booking");
            return;
        }
    };

    // Check if an active booking already exists for this quote (the unique index would catch this too)
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM calendar_bookings WHERE quote_id = $1 AND status != 'cancelled' LIMIT 1",
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if existing.is_some() {
        info!("Active booking already exists for quote {quote_id}, skipping");
        return;
    }

    // Fetch customer and address info for the booking
    let info_row: Option<(Option<String>, Option<String>, Option<f64>, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT c.name, c.email, q.estimated_volume_m3, q.distance_km
        FROM quotes q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.id = $1
        "#,
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let (customer_name, customer_email, volume_m3, distance_km) =
        info_row.unwrap_or((None, None, None, None));

    // Fetch addresses
    let addr_row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            (SELECT street || ', ' || city FROM addresses WHERE id = q.origin_address_id),
            (SELECT street || ', ' || city FROM addresses WHERE id = q.destination_address_id)
        FROM quotes q WHERE q.id = $1
        "#,
    )
    .bind(quote_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let (departure, arrival) = addr_row.unwrap_or((None, None));

    let new_booking = NewBooking {
        booking_date,
        quote_id: Some(quote_id),
        customer_name,
        customer_email,
        departure_address: departure,
        arrival_address: arrival,
        volume_m3,
        distance_km,
        description: None,
        status: "tentative".to_string(),
    };

    match state.calendar.force_create_booking(new_booking).await {
        Ok(booking) => {
            info!(
                "Auto-created tentative booking {} for quote {quote_id} on {}",
                booking.id, booking_date
            );
        }
        Err(e) => {
            warn!("Failed to auto-create booking for quote {quote_id}: {e}");
        }
    }
}

/// Send the generated offer PDF to Telegram with approve/edit/deny buttons.
async fn send_offer_to_telegram(config: &TelegramConfig, generated: &GeneratedOffer) {
    let client = Client::new();
    let api_url = format!(
        "https://api.telegram.org/bot{}/sendDocument",
        config.bot_token
    );

    let offer = &generated.offer;
    let s = &generated.summary;
    let netto = s.netto_cents as f64 / 100.0;
    let brutto = netto * 1.19;

    // Build address lines with floor + elevator info
    let origin_line = format_address_line(&s.origin_address, &s.origin_floor, s.origin_elevator);
    let dest_line = format_address_line(&s.dest_address, &s.dest_floor, s.dest_elevator);

    let mut caption = format!(
        "📋 *Neues Angebot erstellt*\n\n\
         *Kunde:* {}\n\
         *E-Mail:* `{}`",
        generated.customer_name,
        generated.customer_email,
    );
    if !s.customer_phone.is_empty() {
        caption.push_str(&format!("\n*Tel:* {}", s.customer_phone));
    }

    if !s.origin_address.is_empty() {
        caption.push_str(&format!("\n\n*Auszug:* {origin_line}"));
    }
    if !s.dest_address.is_empty() {
        caption.push_str(&format!("\n*Einzug:* {dest_line}"));
    }
    caption.push_str(&format!("\n*Wunschtermin:* {}", s.preferred_date));

    caption.push_str(&format!("\n\n*Volumen:* {:.1} m³", s.volume_m3));
    if s.items_count > 0 {
        caption.push_str(&format!(" ({} Gegenstände)", s.items_count));
    }
    if s.distance_km > 0.0 {
        caption.push_str(&format!("\n*Entfernung:* {:.0} km", s.distance_km));
    }

    if !s.services.is_empty() {
        caption.push_str(&format!("\n\n*Leistungen:* {}", s.services));
    }

    caption.push_str(&format!(
        "\n\n*Preis:* {:.2} € brutto ({:.2} € netto)",
        brutto, netto
    ));
    caption.push_str(&format!(
        "\n{} Helfer × {:.0} Std × {:.2} €/Std",
        s.persons, s.hours, s.rate
    ));

    if !s.customer_message.is_empty() {
        caption.push_str(&format!("\n\n_Kundennachricht:_\n\"{}\"", s.customer_message));
    }

    caption.push_str(&format!(
        "\n\n*Gültig bis:* {}",
        offer
            .valid_until
            .map(|d| d.format("%d.%m.%Y").to_string())
            .unwrap_or_else(|| "Unbegrenzt".to_string()),
    ));

    let inline_keyboard = serde_json::json!({
        "inline_keyboard": [[
            {
                "text": "✅ Senden",
                "callback_data": format!("offer_approve:{}", offer.id)
            },
            {
                "text": "✏️ Bearbeiten",
                "callback_data": format!("offer_edit:{}", offer.id)
            },
            {
                "text": "❌ Verwerfen",
                "callback_data": format!("offer_deny:{}", offer.id)
            }
        ]]
    });

    let pdf_part = Part::bytes(generated.pdf_bytes.clone())
        .file_name(format!("Angebot-{}.pdf", offer.id))
        .mime_str("application/pdf")
        .unwrap();

    let form = Form::new()
        .text("chat_id", config.admin_chat_id.to_string())
        .text("caption", caption)
        .text("parse_mode", "Markdown")
        .text("reply_markup", inline_keyboard.to_string())
        .part("document", pdf_part);

    match client.post(&api_url).multipart(form).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                info!("Offer {} sent to Telegram for approval", offer.id);
            } else {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!("Telegram sendDocument failed ({status}): {body}");
            }
        }
        Err(e) => {
            error!("Failed to send offer to Telegram: {e}");
        }
    }
}

/// Send a simple error notification to the admin via Telegram.
async fn notify_telegram_error(config: &TelegramConfig, message: &str) {
    let client = Client::new();
    let api_url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        config.bot_token
    );

    let payload = serde_json::json!({
        "chat_id": config.admin_chat_id,
        "text": format!("⚠️ {message}"),
    });

    if let Err(e) = client.post(&api_url).json(&payload).send().await {
        error!("Failed to send Telegram error notification: {e}");
    }
}

// --- Offer event handler (receives events from email agent's Telegram poller) ---

use aust_email_agent::ApprovalDecision;

/// State for the offer currently being edited.
struct EditingOffer {
    offer_id: Uuid,
    quote_id: Uuid,
}

/// Background task that handles offer approval/edit/deny events forwarded from the
/// email agent's Telegram polling loop via an mpsc channel.
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
                    let quote_id: Option<(Uuid,)> =
                        sqlx::query_as("SELECT quote_id FROM offers WHERE id = $1")
                            .bind(offer_id)
                            .fetch_optional(&state.db)
                            .await
                            .unwrap_or(None);

                    if let Some((qid,)) = quote_id {
                        editing = Some(EditingOffer {
                            offer_id,
                            quote_id: qid,
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
                    edit_state.quote_id,
                    &text,
                )
                .await;
            }
            _ => {} // ignore non-offer events
        }
    }
}

/// Handle a complete inquiry: create customer + addresses + quote in DB,
/// set volume estimate, and trigger offer generation → PDF → Telegram.
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
    let customer_id: Uuid = match sqlx::query_as::<_, (Uuid,)>(
        r#"
        INSERT INTO customers (id, email, name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        ON CONFLICT (email) DO UPDATE SET
            name = COALESCE(EXCLUDED.name, customers.name),
            phone = COALESCE(EXCLUDED.phone, customers.phone),
            updated_at = $5
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&inquiry.email)
    .bind(&inquiry.name)
    .bind(&inquiry.phone)
    .bind(now)
    .fetch_one(&state.db)
    .await
    {
        Ok((id,)) => id,
        Err(e) => {
            error!("Failed to create customer: {e}");
            return;
        }
    };

    // 2. Create origin address (if we have departure address)
    let origin_id = if let Some(ref addr) = inquiry.departure_address {
        let (street, city, postal) = services::vision::parse_address(addr);
        match sqlx::query_as::<_, (Uuid,)>(
            "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(&street)
        .bind(&city)
        .bind(&postal)
        .bind(&inquiry.departure_floor)
        .bind(inquiry.departure_elevator)
        .fetch_one(&state.db)
        .await
        {
            Ok((id,)) => Some(id),
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
        match sqlx::query_as::<_, (Uuid,)>(
            "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(&street)
        .bind(&city)
        .bind(&postal)
        .bind(&inquiry.arrival_floor)
        .bind(inquiry.arrival_elevator)
        .fetch_one(&state.db)
        .await
        {
            Ok((id,)) => Some(id),
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
            match sqlx::query_as::<_, (Uuid,)>(
                "INSERT INTO addresses (id, street, city, postal_code, floor, elevator) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(Uuid::now_v7())
            .bind(&street)
            .bind(&city)
            .bind(&postal)
            .bind(&inquiry.intermediate_floor)
            .bind(inquiry.intermediate_elevator)
            .fetch_one(&state.db)
            .await
            {
                Ok((id,)) => Some(id),
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
    let quote_id = Uuid::now_v7();
    let preferred_date_ts = inquiry
        .preferred_date
        .map(|d| d.and_hms_opt(10, 0, 0).unwrap())
        .map(|dt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc));

    let notes = build_quote_notes(&inquiry);

    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO quotes (id, customer_id, origin_address_id, destination_address_id, stop_address_id,
                           status, estimated_volume_m3, distance_km, preferred_date, notes, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)
        "#,
    )
    .bind(quote_id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(stop_id)
    .bind("volume_estimated")
    .bind(volume_m3)
    .bind(distance_km)
    .bind(preferred_date_ts)
    .bind(&notes)
    .bind(now)
    .execute(&state.db)
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

    let _ = sqlx::query(
        r#"
        INSERT INTO volume_estimations (id, quote_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(estimation_id)
    .bind(quote_id)
    .bind("manual")
    .bind(source_data)
    .bind(result_data)
    .bind(volume_m3)
    .bind(0.5f64) // lower confidence for rough estimate
    .bind(now)
    .execute(&state.db)
    .await;

    info!(
        "Created quote {quote_id} for customer {} ({}) — {:.1} m³",
        inquiry.name.as_deref().unwrap_or("?"),
        inquiry.email,
        volume_m3
    );

    // Link email thread to quote (if thread exists for this customer)
    let _ = sqlx::query(
        r#"
        UPDATE email_threads SET quote_id = $1
        WHERE id = (
            SELECT id FROM email_threads
            WHERE customer_id = $2 AND quote_id IS NULL
            ORDER BY created_at DESC LIMIT 1
        )
        "#,
    )
    .bind(quote_id)
    .bind(customer_id)
    .execute(&state.db)
    .await;

    // 7. Generate offer → PDF → Telegram
    try_auto_generate_offer(Arc::clone(state), quote_id).await;
}

/// Parsed item from the VolumeCalculator items_list text.
/// Matches the format: "2x Sofa, Couch, Liege je Sitz (0.80 m³)"
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedInventoryItem {
    pub name: String,
    pub quantity: u32,
    pub volume_m3: f64,
}

/// Parse VolumeCalculator items_list text into structured items.
/// Handles both newline-separated and comma-separated formats:
/// - "1x Bettumbau (0.30 m³)\n1x Nachttisch (0.20 m³)"
/// - "1x Bettumbau (0.30 m³), 1x Nachttisch (0.20 m³)"
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

/// Build notes for the quote from inquiry data.
fn build_quote_notes(inquiry: &MovingInquiry) -> String {
    let mut parts = Vec::new();

    if let Some(ref floor) = inquiry.departure_floor {
        parts.push(format!("Auszug: {floor}"));
    }
    if let Some(ref floor) = inquiry.arrival_floor {
        parts.push(format!("Einzug: {floor}"));
    }
    if inquiry.departure_parking_ban == Some(true) {
        parts.push("Halteverbot Auszug".to_string());
    }
    if inquiry.arrival_parking_ban == Some(true) {
        parts.push("Halteverbot Einzug".to_string());
    }
    if inquiry.intermediate_parking_ban == Some(true) {
        parts.push("Halteverbot Zwischenstopp".to_string());
    }
    if inquiry.service_packing {
        parts.push("Verpackungsservice".to_string());
    }
    if inquiry.service_assembly {
        parts.push("Montage".to_string());
    }
    if inquiry.service_disassembly {
        parts.push("Demontage".to_string());
    }
    if inquiry.service_storage {
        parts.push("Einlagerung".to_string());
    }
    if inquiry.service_disposal {
        parts.push("Entsorgung".to_string());
    }
    if let Some(ref notes) = inquiry.notes {
        parts.push(notes.clone());
    }

    parts.join(", ")
}

/// Apply edit instructions and regenerate the offer.
async fn handle_offer_edit(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    old_offer_id: Uuid,
    quote_id: Uuid,
    instructions: &str,
) {
    // Fetch current offer details for LLM context
    let current_offer = fetch_current_offer_summary(&state.db, old_offer_id, quote_id).await;

    // Use LLM to parse natural language edit instructions
    let overrides = match llm_parse_edit_instructions(
        &*state.llm,
        instructions,
        &current_offer,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            warn!("LLM edit parsing failed, falling back to regex: {e}");
            parse_edit_instructions(instructions)
        }
    };

    // Apply numeric overrides directly to the quote if needed
    if let Some(volume) = overrides.volume_m3 {
        let _ = sqlx::query("UPDATE quotes SET estimated_volume_m3 = $1 WHERE id = $2")
            .bind(volume)
            .bind(quote_id)
            .execute(&state.db)
            .await;
    }

    // Regenerate with overrides baked into the xlsx/PDF, preserving the existing offer ID and number
    let offer_overrides = OfferOverrides {
        price_cents: overrides.price_cents,
        persons: overrides.persons,
        hours: overrides.hours,
        rate: overrides.rate,
        line_items: None,
        existing_offer_id: Some(old_offer_id),
    };

    match build_offer_with_overrides(&state.db, &*state.storage, &state.config, quote_id, Some(30), &offer_overrides).await {
        Ok(generated) => {
            info!(
                "Offer {} regenerated for quote {quote_id} (€{:.2})",
                generated.offer.id,
                generated.offer.price_cents as f64 / 100.0
            );

            send_offer_to_telegram(&state.config.telegram, &generated).await;
        }
        Err(e) => {
            error!("Failed to regenerate offer: {e}");
            send_telegram_message(
                client,
                bot_token,
                chat_id,
                &format!("Fehler bei Neuerstellung: {e}"),
            )
            .await;
        }
    }
}

/// Summary of the current offer for LLM context.
struct OfferSummary {
    price_cents: i64,
    persons: u32,
    hours: f64,
    volume_m3: f64,
    distance_km: f64,
}

impl std::fmt::Display for OfferSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let netto = self.price_cents as f64 / 100.0;
        let brutto = netto * 1.19;
        write!(
            f,
            "Aktuelles Angebot: {:.2}€ netto / {:.2}€ brutto, {} Helfer, {:.1} Stunden, {:.1} m³, {:.0} km",
            netto, brutto, self.persons, self.hours, self.volume_m3, self.distance_km
        )
    }
}

/// Fetch current offer details for LLM context.
async fn fetch_current_offer_summary(db: &PgPool, offer_id: Uuid, quote_id: Uuid) -> OfferSummary {
    // Get offer price
    let price: Option<(i64,)> = sqlx::query_as("SELECT price_cents FROM offers WHERE id = $1")
        .bind(offer_id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

    // Get quote details
    let quote: Option<(Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT estimated_volume_m3, distance_km FROM quotes WHERE id = $1",
    )
    .bind(quote_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let price_cents = price.map(|(p,)| p).unwrap_or(0);
    let (volume, distance) = quote.unwrap_or((None, None));

    // Estimate persons/hours from price (reverse of pricing engine)
    // price_cents = persons * hours * rate_per_person_hour (3000 = €30)
    let volume_m3 = volume.unwrap_or(25.0);
    let persons = 2u32.max((volume_m3 / 10.0).ceil() as u32);
    let throughput = persons as f64 * 2.0; // volume_per_person_hour
    let hours = (volume_m3 / throughput).ceil().max(1.0);

    OfferSummary {
        price_cents,
        persons,
        hours,
        volume_m3,
        distance_km: distance.unwrap_or(0.0),
    }
}

/// Use LLM to parse Alex's natural language edit instructions into numeric overrides.
///
/// Alex can say things like:
/// - "mach das Angebot auf 350 Euro" or "350 brutto" or just "350"
/// - "4 Helfer, 6 Stunden"
/// - "Stundensatz 35"
///
/// Default behavior: a bare price like "350" or "350 Euro" is treated as brutto.
/// Alex always thinks in brutto prices.
async fn llm_parse_edit_instructions(
    llm: &dyn LlmProvider,
    instructions: &str,
    current: &OfferSummary,
) -> Result<EditOverrides, String> {
    let system_prompt = format!(
        r#"Du bist ein Assistent, der Anweisungen zur Angebotsänderung versteht.

{current}

Analysiere die Anweisung und extrahiere die gewünschten Änderungen als JSON.
Wichtige Regeln:
- Wenn ein Preis OHNE Qualifier genannt wird (z.B. "350", "350 Euro", "mach auf 350"), ist das IMMER brutto (inkl. 19% USt).
- Nur wenn explizit "netto" gesagt wird, ist es netto.
- Brutto zu Netto: netto = brutto / 1.19
- Stundensatz wird aus dem Preis berechnet: stundensatz = netto / (helfer × stunden)

Antworte NUR mit einem JSON-Objekt. Felder die nicht geändert werden: weglassen.
Mögliche Felder:
- "price_cents_netto": Nettopreis in Cent (integer)
- "persons": Anzahl Helfer (integer)
- "hours": Stunden (float)
- "rate": Stundensatz pro Helfer (float)
- "volume_m3": Volumen in m³ (float)

Beispiele:
- "350 Euro" → {{"price_cents_netto": 29412}}  (350/1.19*100)
- "350 brutto" → {{"price_cents_netto": 29412}}
- "350 netto" → {{"price_cents_netto": 35000}}
- "4 Helfer" → {{"persons": 4}}
- "mach das Angebot auf 800" → {{"price_cents_netto": 67227}}  (800/1.19*100)
- "Stundensatz 35" → {{"rate": 35.0}}"#
    );

    let messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(instructions.to_string()),
    ];

    let response = llm.complete(&messages).await.map_err(|e| e.to_string())?;

    // Extract JSON from response (may have markdown code fences)
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim())
        .strip_suffix("```")
        .unwrap_or(response.trim())
        .trim();

    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {e}"))?;

    let mut overrides = EditOverrides::default();

    if let Some(price) = parsed.get("price_cents_netto").and_then(|v| v.as_i64()) {
        overrides.price_cents = Some(price);
    }
    if let Some(persons) = parsed.get("persons").and_then(|v| v.as_u64()) {
        overrides.persons = Some(persons as u32);
    }
    if let Some(hours) = parsed.get("hours").and_then(|v| v.as_f64()) {
        overrides.hours = Some(hours);
    }
    if let Some(rate) = parsed.get("rate").and_then(|v| v.as_f64()) {
        overrides.rate = Some(rate);
    }
    if let Some(vol) = parsed.get("volume_m3").and_then(|v| v.as_f64()) {
        overrides.volume_m3 = Some(vol);
    }

    info!(
        "LLM parsed edit instructions: price={:?}, persons={:?}, hours={:?}, rate={:?}",
        overrides.price_cents, overrides.persons, overrides.hours, overrides.rate
    );

    Ok(overrides)
}

/// Parsed overrides from admin's free-text edit instructions.
#[derive(Default)]
struct EditOverrides {
    price_cents: Option<i64>,
    persons: Option<u32>,
    hours: Option<f64>,
    rate: Option<f64>,
    volume_m3: Option<f64>,
}

/// Parse simple German edit instructions into numeric overrides.
/// Supports patterns like:
/// - "Preis auf 800 Euro" / "Preis: 800" / "800€"
/// - "4 Helfer" / "Helfer: 4"
/// - "6 Stunden" / "Stunden: 6"
/// - "Stundensatz 35" / "Rate: 35"
/// - "Volumen 15" / "15 m³"
fn parse_edit_instructions(text: &str) -> EditOverrides {
    let mut overrides = EditOverrides::default();
    let text_lower = text.to_lowercase();

    // Extract all numbers with their surrounding context
    for segment in text_lower.split([',', '.', ';', '\n']) {
        let segment = segment.trim();

        // Price: "preis auf 800", "800 euro", "800€", "preis: 800", "350 brutto"
        if segment.contains("preis") || segment.contains('€') || segment.contains("euro") || segment.contains("brutto") || segment.contains("netto") {
            if let Some(num) = extract_number(segment) {
                let cents = if segment.contains("brutto") {
                    // Convert brutto to netto (remove 19% USt)
                    ((num / 1.19) * 100.0) as i64
                } else {
                    (num * 100.0) as i64
                };
                overrides.price_cents = Some(cents);
            }
        }

        // Persons: "4 helfer", "helfer: 4", "4 mann"
        if segment.contains("helfer") || segment.contains("mann") || segment.contains("person") {
            if let Some(num) = extract_number(segment) {
                overrides.persons = Some(num as u32);
            }
        }

        // Hours: "6 stunden", "stunden: 6"
        if segment.contains("stunde") {
            if let Some(num) = extract_number(segment) {
                if !segment.contains("satz") && !segment.contains("rate") {
                    overrides.hours = Some(num);
                }
            }
        }

        // Rate: "stundensatz 35", "rate: 35"
        if segment.contains("stundensatz") || segment.contains("rate") {
            if let Some(num) = extract_number(segment) {
                overrides.rate = Some(num);
            }
        }

        // Volume: "volumen 15", "15 m³", "15 kubikmeter"
        if segment.contains("volumen") || segment.contains("m³") || segment.contains("kubik") {
            if let Some(num) = extract_number(segment) {
                overrides.volume_m3 = Some(num);
            }
        }
    }

    overrides
}

/// Extract the first number from a string.
fn extract_number(s: &str) -> Option<f64> {
    let mut num_str = String::new();
    let mut found_digit = false;

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_str.push(ch);
            found_digit = true;
        } else if (ch == '.' || ch == ',') && found_digit {
            num_str.push('.');
        } else if found_digit {
            break;
        }
    }

    if num_str.is_empty() {
        return None;
    }

    num_str.parse::<f64>().ok()
}

async fn handle_offer_approval(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    info!("Offer {offer_id} approved, sending to customer");

    let row: Option<(String, Option<String>, Uuid)> = sqlx::query_as(
        r#"
        SELECT c.email, o.pdf_storage_key, o.quote_id
        FROM offers o
        JOIN quotes q ON o.quote_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(offer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let Some((customer_email, Some(storage_key), quote_id)) = row else {
        send_telegram_message(
            client,
            bot_token,
            chat_id,
            "Fehler: Angebot oder PDF nicht gefunden.",
        )
        .await;
        return;
    };

    let pdf_bytes = match state.storage.download(&storage_key).await {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("Failed to download offer PDF: {e}");
            send_telegram_message(
                client,
                bot_token,
                chat_id,
                &format!("Fehler beim PDF-Download: {e}"),
            )
            .await;
            return;
        }
    };

    match send_offer_email(state, &customer_email, &pdf_bytes, offer_id).await {
        Ok(()) => {
            let now = chrono::Utc::now();
            let _ = sqlx::query("UPDATE offers SET status = 'sent', sent_at = $1 WHERE id = $2")
                .bind(now)
                .bind(offer_id)
                .execute(&state.db)
                .await;

            // Also update quote status to offer_sent
            let _ = sqlx::query("UPDATE quotes SET status = 'offer_sent', updated_at = $1 WHERE id = $2")
                .bind(now)
                .bind(quote_id)
                .execute(&state.db)
                .await;

            // Store offer email in thread (if a thread exists for this quote's customer)
            let thread_row: Option<(Uuid,)> = sqlx::query_as(
                r#"
                SELECT et.id FROM email_threads et
                JOIN quotes q ON et.customer_id = q.customer_id
                WHERE q.id = $1
                ORDER BY et.created_at DESC LIMIT 1
                "#,
            )
            .bind(quote_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some((thread_id,)) = thread_row {
                let body = "Sehr geehrte Damen und Herren,\n\n\
                    anbei erhalten Sie unser Angebot für Ihren Umzug.\n\n\
                    Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
                    Mit freundlichen Grüßen,\nIhr Umzugsteam";

                let _ = sqlx::query(
                    r#"
                    INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, created_at)
                    VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, NOW())
                    "#,
                )
                .bind(Uuid::now_v7())
                .bind(thread_id)
                .bind(&state.config.email.from_address)
                .bind(&customer_email)
                .bind("Ihr Umzugsangebot")
                .bind(body)
                .execute(&state.db)
                .await;
            }

            send_telegram_message(
                client,
                bot_token,
                chat_id,
                &format!("✅ Angebot an {customer_email} gesendet!"),
            )
            .await;
        }
        Err(e) => {
            error!("Failed to send offer email: {e}");
            send_telegram_message(
                client,
                bot_token,
                chat_id,
                &format!("Fehler beim E-Mail-Versand: {e}"),
            )
            .await;
        }
    }
}

async fn handle_offer_denial(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    info!("Offer {offer_id} denied");

    // Fetch quote_id before updating
    let quote_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT quote_id FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let _ = sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(offer_id)
        .execute(&state.db)
        .await;

    // Also update quote status to rejected
    if let Some((quote_id,)) = quote_row {
        let now = chrono::Utc::now();
        let _ = sqlx::query("UPDATE quotes SET status = 'rejected', updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(quote_id)
            .execute(&state.db)
            .await;
    }

    send_telegram_message(client, bot_token, chat_id, "❌ Angebot verworfen.").await;
}

/// Send an email with the offer PDF attached.
pub async fn send_offer_email(
    state: &AppState,
    to: &str,
    pdf_bytes: &[u8],
    offer_id: Uuid,
) -> Result<(), String> {
    use crate::services::email::{build_email_with_attachment, send_email};

    let email_config = &state.config.email;

    let body_text = "Sehr geehrte Damen und Herren,\n\n\
        anbei erhalten Sie unser Angebot für Ihren Umzug.\n\n\
        Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
        Mit freundlichen Grüßen,\n\
        Ihr Umzugsteam";

    let message = build_email_with_attachment(
        &email_config.from_address,
        &email_config.from_name,
        to,
        "Ihr Umzugsangebot",
        body_text,
        pdf_bytes,
        &format!("Angebot-{offer_id}.pdf"),
        "application/pdf",
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
    .map_err(|e| e.to_string())?;

    info!("Offer email sent to {to}");
    Ok(())
}

pub async fn send_offer_email_custom(
    state: &AppState,
    to: &str,
    pdf_bytes: &[u8],
    offer_id: Uuid,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_email_with_attachment, send_email};

    let email_config = &state.config.email;

    let message = build_email_with_attachment(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
        pdf_bytes,
        &format!("Angebot-{offer_id}.pdf"),
        "application/pdf",
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
    .map_err(|e| e.to_string())?;

    info!("Offer email sent to {to} (custom)");
    Ok(())
}

async fn send_telegram_message(client: &Client, bot_token: &str, chat_id: i64, text: &str) {
    let api_url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    if let Err(e) = client.post(&api_url).json(&payload).send().await {
        error!("Failed to send Telegram message: {e}");
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
    // build_quote_notes tests
    // ---------------------------------------------------------------

    fn minimal_inquiry() -> MovingInquiry {
        MovingInquiry {
            id: uuid::Uuid::nil(),
            email: "test@example.com".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn notes_empty_inquiry() {
        let inquiry = minimal_inquiry();
        let notes = build_quote_notes(&inquiry);
        assert_eq!(notes, "");
    }

    #[test]
    fn notes_departure_floor_only() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_floor = Some("3. Stock".to_string());
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Auszug: 3. Stock"), "notes = {notes}");
    }

    #[test]
    fn notes_arrival_floor_only() {
        let mut inquiry = minimal_inquiry();
        inquiry.arrival_floor = Some("EG".to_string());
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Einzug: EG"), "notes = {notes}");
    }

    #[test]
    fn notes_both_floors() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_floor = Some("2. Stock".to_string());
        inquiry.arrival_floor = Some("EG".to_string());
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Auszug: 2. Stock"), "notes = {notes}");
        assert!(notes.contains("Einzug: EG"), "notes = {notes}");
        // Should be comma-separated
        assert!(notes.contains(", "), "parts should be comma-separated: {notes}");
    }

    #[test]
    fn notes_halteverbot_auszug() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_parking_ban = Some(true);
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Halteverbot Auszug"), "notes = {notes}");
    }

    #[test]
    fn notes_halteverbot_einzug() {
        let mut inquiry = minimal_inquiry();
        inquiry.arrival_parking_ban = Some(true);
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Halteverbot Einzug"), "notes = {notes}");
    }

    #[test]
    fn notes_halteverbot_zwischenstopp() {
        let mut inquiry = minimal_inquiry();
        inquiry.intermediate_parking_ban = Some(true);
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Halteverbot Zwischenstopp"), "notes = {notes}");
    }

    #[test]
    fn notes_all_services() {
        let mut inquiry = minimal_inquiry();
        inquiry.service_packing = true;
        inquiry.service_assembly = true;
        inquiry.service_disassembly = true;
        inquiry.service_storage = true;
        inquiry.service_disposal = true;
        let notes = build_quote_notes(&inquiry);
        assert!(notes.contains("Verpackungsservice"), "notes = {notes}");
        assert!(notes.contains("Montage"), "notes = {notes}");
        assert!(notes.contains("Demontage"), "notes = {notes}");
        assert!(notes.contains("Einlagerung"), "notes = {notes}");
        assert!(notes.contains("Entsorgung"), "notes = {notes}");
    }

    #[test]
    fn notes_passthrough_notes_field() {
        let mut inquiry = minimal_inquiry();
        inquiry.notes = Some("Bitte vorsichtig mit dem Klavier".to_string());
        let notes = build_quote_notes(&inquiry);
        assert!(
            notes.contains("Bitte vorsichtig mit dem Klavier"),
            "notes = {notes}"
        );
    }

    #[test]
    fn notes_full_inquiry() {
        let mut inquiry = minimal_inquiry();
        inquiry.departure_floor = Some("1. Stock".to_string());
        inquiry.arrival_floor = Some("3. Stock".to_string());
        inquiry.departure_parking_ban = Some(true);
        inquiry.arrival_parking_ban = Some(true);
        inquiry.intermediate_parking_ban = Some(true);
        inquiry.service_packing = true;
        inquiry.service_assembly = true;
        inquiry.service_disassembly = true;
        inquiry.service_storage = true;
        inquiry.service_disposal = true;
        inquiry.notes = Some("Klavier im 1. OG".to_string());
        let notes = build_quote_notes(&inquiry);

        assert!(notes.contains("Auszug: 1. Stock"), "notes = {notes}");
        assert!(notes.contains("Einzug: 3. Stock"), "notes = {notes}");
        assert!(notes.contains("Halteverbot Auszug"), "notes = {notes}");
        assert!(notes.contains("Halteverbot Einzug"), "notes = {notes}");
        assert!(notes.contains("Halteverbot Zwischenstopp"), "notes = {notes}");
        assert!(notes.contains("Verpackungsservice"), "notes = {notes}");
        assert!(notes.contains("Montage"), "notes = {notes}");
        assert!(notes.contains("Demontage"), "notes = {notes}");
        assert!(notes.contains("Einlagerung"), "notes = {notes}");
        assert!(notes.contains("Entsorgung"), "notes = {notes}");
        assert!(notes.contains("Klavier im 1. OG"), "notes = {notes}");
    }
}
