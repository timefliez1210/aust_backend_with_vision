//! Offer orchestrator — auto-generates offers when quotes become ready,
//! sends PDF to Telegram for approval, and emails on approval.
//! Supports edit loop: Alex can press ✏️, type adjustment instructions,
//! and get a regenerated offer.

use crate::routes::offers::{build_offer, build_offer_with_overrides, GeneratedOffer, OfferOverrides};
use crate::{services, AppState};
use aust_core::config::TelegramConfig;
use aust_core::models::{MovingInquiry, Services};
use aust_distance_calculator::{RouteCalculator, RouteRequest};
use aust_llm_providers::{LlmMessage, LlmProvider};
use reqwest::{
    multipart::{Form, Part},
    Client,
};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Format a single address string with floor and elevator info for Telegram display.
///
/// **Caller**: `send_offer_to_telegram`
/// **Why**: Telegram caption must show the admin exactly where helpers are going and what
/// access conditions they face (stairs vs elevator), so they can sanity-check the offer.
///
/// # Parameters
/// - `address` — full address string, e.g. `"Musterstr. 1, 31135 Hildesheim"`
/// - `floor` — floor descriptor, e.g. `"3. OG"` or `""` (empty = omit)
/// - `elevator` — `Some(true)` = Aufzug, `Some(false)` = kein Aufzug, `None` = unknown (omit)
///
/// # Returns
/// If floor/elevator info is available: `"Musterstr. 1, 31135 Hildesheim (3. OG, kein Aufzug)"`.
/// If neither is known: the address string unchanged.
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

/// Check whether a quote has enough data to auto-generate an offer, and do so if ready.
///
/// **Caller**: `handle_complete_inquiry` (immediately after quote + volume estimation are stored),
/// and any endpoint that completes a volume estimation (e.g. `estimates::post_inventory`).
/// **Why**: Offers should be generated and forwarded to Telegram without any manual trigger from
/// Alex. The function is idempotent — it exits early if an offer already exists.
///
/// # Readiness criteria
/// - Quote must have `estimated_volume_m3 > 0`.
/// - Distance is not required; if missing and both addresses are present, this function
///   automatically runs the route calculator and writes `distance_km` to the quote first.
///
/// # Parameters
/// - `state` — shared application state (DB, storage, config)
/// - `inquiry_id` — the quote to check and potentially generate an offer for
///
/// # Returns
/// Nothing. Errors are logged and, if critical, forwarded to the admin via Telegram.
pub async fn try_auto_generate_offer(state: Arc<AppState>, inquiry_id: Uuid) {
    // Check if an offer already exists for this quote
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM offers WHERE inquiry_id = $1 LIMIT 1")
            .bind(inquiry_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    if existing.is_some() {
        info!("Offer already exists for quote {inquiry_id}, skipping auto-generation");
        return;
    }

    // Check that the quote has a volume estimate (minimum requirement); also fetch distance/addresses
    #[derive(sqlx::FromRow)]
    struct QuoteReadiness {
        estimated_volume_m3: Option<f64>,
        distance_km: Option<f64>,
        origin_address_id: Option<Uuid>,
        destination_address_id: Option<Uuid>,
        stop_address_id: Option<Uuid>,
    }
    let readiness: Option<QuoteReadiness> = sqlx::query_as(
        "SELECT estimated_volume_m3, distance_km, origin_address_id, destination_address_id, stop_address_id FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let q = match readiness {
        Some(r) if r.estimated_volume_m3.unwrap_or(0.0) > 0.0 => r,
        _ => {
            info!("Quote {inquiry_id} not ready for offer (no volume estimate)");
            return;
        }
    };

    // Auto-calculate distance if both addresses are present and distance is still 0/missing
    if q.distance_km.unwrap_or(0.0) == 0.0 {
        if let (Some(origin_id), Some(dest_id)) = (q.origin_address_id, q.destination_address_id) {
            #[derive(sqlx::FromRow)]
            struct AddrStr { street: String, city: String, postal_code: Option<String> }
            let fetch_addr = |id: Uuid| {
                let db = state.db.clone();
                async move {
                    sqlx::query_as::<_, AddrStr>("SELECT street, city, postal_code FROM addresses WHERE id = $1")
                        .bind(id)
                        .fetch_optional(&db)
                        .await
                        .ok()
                        .flatten()
                }
            };
            let fmt_addr = |a: &AddrStr| format!(
                "{}, {}{}",
                a.street,
                a.postal_code.as_deref().map(|p| format!("{p} ")).unwrap_or_default(),
                a.city
            );

            if let (Some(origin), Some(dest)) = (fetch_addr(origin_id).await, fetch_addr(dest_id).await) {
                let mut route_addresses = vec![fmt_addr(&origin)];
                if let Some(stop_id) = q.stop_address_id {
                    if let Some(stop) = fetch_addr(stop_id).await {
                        route_addresses.push(fmt_addr(&stop));
                    }
                }
                route_addresses.push(fmt_addr(&dest));

                let calculator = RouteCalculator::new(state.config.maps.api_key.clone());
                match calculator.calculate(&RouteRequest { addresses: route_addresses }).await {
                    Ok(result) => {
                        info!("Distance calculated for quote {inquiry_id}: {:.1} km", result.total_distance_km);
                        let _ = sqlx::query("UPDATE inquiries SET distance_km = $1, updated_at = NOW() WHERE id = $2")
                            .bind(result.total_distance_km)
                            .bind(inquiry_id)
                            .execute(&state.db)
                            .await;
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
            error!("Auto-offer generation failed for quote {inquiry_id}: {e}");
            notify_telegram_error(
                &state.config.telegram,
                &format!("Angebotserstellung fehlgeschlagen für Quote {inquiry_id}: {e}"),
            )
            .await;
        }
    }
}

/// Send the generated offer PDF to the admin Telegram chat with inline action buttons.
///
/// **Caller**: `try_auto_generate_offer` (initial offer) and `handle_offer_edit` (regenerated
/// offer after Alex's edits).
/// **Why**: The Telegram message is the primary review UI for Alex. It shows all relevant
/// move details and pricing so he can approve, request edits, or discard the offer.
///
/// The caption is formatted in Telegram Markdown and includes:
/// - Customer name, email, phone
/// - Origin/destination addresses with floor + elevator info (via `format_address_line`)
/// - Preferred date, volume (m³), item count, distance (km)
/// - Selected additional services
/// - Brutto and netto prices, persons × hours × rate breakdown
/// - Free-text customer message (if any)
/// - Validity date
///
/// Inline keyboard: `✅ Senden` / `✏️ Bearbeiten` / `❌ Verwerfen`, each carrying
/// `offer_approve:<id>`, `offer_edit:<id>`, or `offer_deny:<id>` as callback data.
///
/// # Parameters
/// - `config` — Telegram bot config (token + admin chat ID)
/// - `generated` — offer data including the rendered PDF bytes and offer summary
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

/// Send a plain-text error notification to the admin Telegram chat.
///
/// **Caller**: `try_auto_generate_offer` when offer generation fails.
/// **Why**: The admin (Alex) has no other way to learn about silent background failures.
/// Sending a message to Telegram ensures errors surface immediately rather than being
/// silently swallowed.
///
/// The message is prefixed with `⚠️` so it is visually distinct from normal offer messages.
///
/// # Parameters
/// - `config` — Telegram bot config (token + admin chat ID)
/// - `message` — German-language error description to display
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
                    let inquiry_id: Option<(Uuid,)> =
                        sqlx::query_as("SELECT inquiry_id FROM offers WHERE id = $1")
                            .bind(offer_id)
                            .fetch_optional(&state.db)
                            .await
                            .unwrap_or(None);

                    if let Some((qid,)) = inquiry_id {
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

    let customer_id: Uuid = match sqlx::query_as::<_, (Uuid,)>(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
        ON CONFLICT (email) DO UPDATE SET
            name       = COALESCE(EXCLUDED.name,       customers.name),
            salutation = COALESCE(EXCLUDED.salutation, customers.salutation),
            first_name = COALESCE(EXCLUDED.first_name, customers.first_name),
            last_name  = COALESCE(EXCLUDED.last_name,  customers.last_name),
            phone      = COALESCE(EXCLUDED.phone,      customers.phone),
            updated_at = $8
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&inquiry.email)
    .bind(&inquiry.name)
    .bind(&inquiry.salutation)
    .bind(&inq_first_name)
    .bind(&inq_last_name)
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

    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO inquiries (id, customer_id, origin_address_id, destination_address_id, stop_address_id,
                           status, estimated_volume_m3, distance_km, preferred_date, notes, services, source, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .bind(origin_id)
    .bind(dest_id)
    .bind(stop_id)
    .bind("estimated")
    .bind(volume_m3)
    .bind(distance_km)
    .bind(preferred_date_ts)
    .bind(&notes)
    .bind(&services_json)
    .bind(&source)
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

    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO volume_estimations (id, inquiry_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(estimation_id)
    .bind(inquiry_id)
    .bind("manual")
    .bind(source_data)
    .bind(result_data)
    .bind(volume_m3)
    .bind(0.5f64) // lower confidence for rough estimate
    .bind(now)
    .execute(&state.db)
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
    let _ = sqlx::query(
        r#"
        UPDATE email_threads SET inquiry_id = $1
        WHERE id = (
            SELECT id FROM email_threads
            WHERE customer_id = $2 AND inquiry_id IS NULL
            ORDER BY created_at DESC LIMIT 1
        )
        "#,
    )
    .bind(inquiry_id)
    .bind(customer_id)
    .execute(&state.db)
    .await;

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

/// Parse Alex's free-text edit instructions and regenerate the offer with the requested overrides.
///
/// **Caller**: `run_offer_event_handler` on `ApprovalDecision::OfferEditText`, after Alex
/// has entered edit mode by pressing ✏️ and typed his adjustment.
/// **Why**: Alex reviews offers in Telegram and may want to adjust price, headcount, hours, or
/// volume before sending to the customer. Rather than opening a backend admin panel, he types
/// natural language German instructions and the system re-generates the PDF in-place.
///
/// # Flow
/// 1. Fetch current offer details for LLM context (`fetch_current_offer_summary`).
/// 2. Call `llm_parse_edit_instructions` to extract numeric overrides; on failure fall
///    back to regex-based `parse_edit_instructions`.
/// 3. If `volume_m3` was overridden, write it back to `quotes.estimated_volume_m3`.
/// 4. Rebuild the offer with the parsed overrides, preserving the existing offer ID and
///    offer number (`existing_offer_id = Some(old_offer_id)`).
/// 5. Send the new PDF to Telegram via `send_offer_to_telegram`.
///
/// # Parameters
/// - `state` — shared application state
/// - `client` — HTTP client for Telegram API calls
/// - `bot_token` — Telegram bot token
/// - `chat_id` — admin Telegram chat ID
/// - `old_offer_id` — the offer being replaced (its ID is reused in the regenerated offer)
/// - `inquiry_id` — the quote this offer belongs to
/// - `instructions` — raw German free-text from Alex, e.g. `"800 Euro, 4 Helfer"`
///
/// # Errors
/// Generation errors are sent back to Alex as a German error message via Telegram.
async fn handle_offer_edit(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    old_offer_id: Uuid,
    inquiry_id: Uuid,
    instructions: &str,
) {
    // Fetch current offer details for LLM context
    let current_offer = fetch_current_offer_summary(&state.db, old_offer_id, inquiry_id).await;

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
        let _ = sqlx::query("UPDATE inquiries SET estimated_volume_m3 = $1 WHERE id = $2")
            .bind(volume)
            .bind(inquiry_id)
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
        fahrt_flat_total: None,  // Telegram edits don't touch Fahrkostenpauschale; stored override is preserved via COALESCE
    };

    match build_offer_with_overrides(&state.db, &*state.storage, &state.config, inquiry_id, Some(30), &offer_overrides).await {
        Ok(generated) => {
            info!(
                "Offer {} regenerated for quote {inquiry_id} (€{:.2})",
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

/// Fetch the current offer's price and the quote's volume/distance to build an `OfferSummary`.
///
/// **Caller**: `handle_offer_edit`, before calling `llm_parse_edit_instructions`.
/// **Why**: The LLM prompt includes the current offer state so Alex can say things like
/// "reduce by 50 euros" and the LLM can compute the new absolute price rather than needing
/// the full context to be re-fetched inside the LLM layer.
///
/// Persons and hours are reverse-engineered from volume using the same heuristic the pricing
/// engine uses: `persons = ceil(volume / 10)`, `hours = ceil(volume / (persons × 2))`.
/// This gives approximate values sufficient for LLM context — the authoritative recalculation
/// happens in `build_offer_with_overrides`.
///
/// # Parameters
/// - `db` — database connection pool
/// - `offer_id` — offer whose `price_cents` is fetched
/// - `inquiry_id` — quote whose `estimated_volume_m3` and `distance_km` are fetched
///
/// # Returns
/// An `OfferSummary` with sensible defaults (`0` price, `25.0` m³) if the rows are missing.
async fn fetch_current_offer_summary(db: &PgPool, offer_id: Uuid, inquiry_id: Uuid) -> OfferSummary {
    // Get offer price
    let price: Option<(i64,)> = sqlx::query_as("SELECT price_cents FROM offers WHERE id = $1")
        .bind(offer_id)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

    // Get quote details
    let quote: Option<(Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT estimated_volume_m3, distance_km FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
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

/// Use the configured LLM to parse Alex's natural language edit instructions into numeric overrides.
///
/// **Caller**: `handle_offer_edit`.
/// **Why**: Regex parsing (`parse_edit_instructions`) handles straightforward phrases but
/// fails on paraphrases like "mach das Angebot billiger" or "nehmt einen Helfer weg". The LLM
/// understands context and can compute derived values (e.g. back-calculate netto from brutto).
///
/// If the LLM call or JSON parsing fails, `handle_offer_edit` falls back to `parse_edit_instructions`.
///
/// # Math
/// Alex always thinks and speaks in **brutto** prices (incl. 19% VAT).
/// A bare number like `"800"` or `"800 Euro"` is always treated as brutto:
///
/// `price_cents_netto = round(brutto / 1.19 × 100)`
///
/// Only the explicit keyword `"netto"` bypasses the conversion:
///
/// `price_cents_netto = round(netto × 100)`
///
/// # Parameters
/// - `llm` — LLM provider instance (Claude, OpenAI, or Ollama)
/// - `instructions` — Alex's raw German free-text, e.g. `"mach auf 800 Euro, 4 Helfer"`
/// - `current` — current offer summary embedded in the system prompt for context
///
/// # Returns
/// `Ok(EditOverrides)` with only the fields that were explicitly mentioned set.
/// Fields not mentioned remain `None` so they are not overridden in `build_offer_with_overrides`.
///
/// # Errors
/// Returns `Err(String)` if the LLM call fails or the response cannot be parsed as JSON.
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

/// Regex-based fallback parser for Alex's German edit instructions.
///
/// **Caller**: `handle_offer_edit`, used when `llm_parse_edit_instructions` fails.
/// **Why**: Provides a deterministic, offline fallback that handles the most common
/// instruction patterns without requiring a live LLM.
///
/// The text is split on `,`, `.`, `;`, and `\n` and each segment is checked for keyword
/// patterns. Only the first matching number per segment is extracted.
///
/// # Supported patterns
/// - Price: `"Preis auf 800 Euro"` / `"800€"` / `"Preis: 500"` / `"350 brutto"`
/// - Persons: `"4 Helfer"` / `"Helfer: 4"` / `"3 Mann"`
/// - Hours: `"6 Stunden"` / `"Stunden: 6"`
/// - Rate: `"Stundensatz 35"` / `"Rate: 35"`
/// - Volume: `"15 m³"` / `"Volumen 15"` / `"15 Kubikmeter"`
///
/// # Math
/// Alex always thinks in **brutto** prices. Unless `"netto"` is present, every extracted
/// price number is treated as brutto and converted to netto cents:
///
/// `price_cents_netto = floor(brutto / 1.19 × 100)`
///
/// If both `"netto"` and `"brutto"` appear in the same segment, `"netto"` takes precedence
/// (the `netto` check executes first in the condition chain).
///
/// # Parameters
/// - `text` — free-text German instruction from Alex
///
/// # Returns
/// `EditOverrides` with only the detected fields set; undetected fields remain `None`.
fn parse_edit_instructions(text: &str) -> EditOverrides {
    let mut overrides = EditOverrides::default();
    let text_lower = text.to_lowercase();

    // Extract all numbers with their surrounding context
    for segment in text_lower.split([',', '.', ';', '\n']) {
        let segment = segment.trim();

        // Price: "preis auf 800", "800 euro", "800€", "preis: 800", "350 brutto"
        // Bare prices ("800 euro", "800€", "preis 800") are always treated as brutto — consistent with LLM path.
        // Only explicit "netto" keeps the value as-is.
        if segment.contains("preis") || segment.contains('€') || segment.contains("euro") || segment.contains("brutto") || segment.contains("netto") {
            if let Some(num) = extract_number(segment) {
                let cents = if segment.contains("netto") {
                    (num * 100.0) as i64
                } else {
                    // Default: treat as brutto → convert to netto
                    ((num / 1.19) * 100.0) as i64
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

/// Extract the first numeric value (integer or decimal) from a string.
///
/// **Caller**: `parse_edit_instructions`, once per keyword-matched segment.
/// **Why**: Price and quantity values are embedded in natural language and must be
/// extracted without splitting the string into tokens, since the number can appear
/// anywhere relative to the keyword.
///
/// Scanning stops at the first non-numeric, non-decimal character encountered after
/// at least one digit has been seen. Both `.` and `,` are treated as decimal separators
/// (German locale uses commas), though only the first separator is meaningful.
///
/// # Parameters
/// - `s` — the string segment to scan
///
/// # Returns
/// `Some(f64)` for the first number found, or `None` if the string contains no digits.
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

/// Create an editable email draft in the admin dashboard when an offer is approved in Telegram.
///
/// **Caller**: `run_offer_event_handler` on `ApprovalDecision::OfferApprove`.
/// **Why**: Alex presses ✅ in Telegram after reviewing the PDF. Instead of auto-sending the
/// offer email to the customer, we create a draft in the email thread so Alex can review and
/// edit the cover letter in the admin dashboard before explicitly sending it with the PDF.
///
/// # Flow
/// 1. Look up `customer.email` and `inquiry_id` from the approved offer.
/// 2. Find or create an email thread for the inquiry.
/// 3. Insert a draft `email_message` (status='draft') with a cover letter template.
/// 4. Notify Alex via Telegram that the draft is ready in the dashboard.
///
/// The draft is sent (with PDF attached) via `POST /api/v1/admin/emails/messages/{id}/send`
/// from the admin frontend once Alex is satisfied with the cover letter.
///
/// # Parameters
/// - `state` — shared application state (DB, email config)
/// - `client` — HTTP client for Telegram API calls
/// - `bot_token` — Telegram bot token
/// - `chat_id` — admin Telegram chat ID
/// - `offer_id` — the offer that was approved
async fn handle_offer_approval(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    info!("Offer {offer_id} approved — creating email draft for admin to review and send");

    let row: Option<(String, Uuid)> = sqlx::query_as(
        r#"
        SELECT c.email, o.inquiry_id
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        JOIN customers c ON q.customer_id = c.id
        WHERE o.id = $1
        "#,
    )
    .bind(offer_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let Some((customer_email, inquiry_id)) = row else {
        send_telegram_message(
            client,
            bot_token,
            chat_id,
            "Fehler: Angebot nicht gefunden.",
        )
        .await;
        return;
    };

    // Find or create an email thread for this inquiry so the draft is visible in the dashboard.
    let thread_id = find_or_create_offer_thread(state, inquiry_id, &customer_email).await;

    let Some(thread_id) = thread_id else {
        send_telegram_message(
            client,
            bot_token,
            chat_id,
            &format!("Fehler: E-Mail-Thread für {customer_email} konnte nicht erstellt werden."),
        )
        .await;
        return;
    };

    let body = "Sehr geehrte/r [Name],\n\n\
        anbei erhalten Sie Ihr persönliches Umzugsangebot.\n\n\
        Bei Rückfragen stehen wir Ihnen gerne unter 05121 – 7558379 zur Verfügung.\n\n\
        Mit freundlichen Grüßen,\n\
        Ihr AUST Umzüge Team";

    let _ = sqlx::query(
        r#"
        INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, 'draft', NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(thread_id)
    .bind(&state.config.email.from_address)
    .bind(&customer_email)
    .bind("Ihr Umzugsangebot — AUST Umzüge")
    .bind(body)
    .execute(&state.db)
    .await;

    send_telegram_message(
        client,
        bot_token,
        chat_id,
        &format!(
            "✅ Angebot freigegeben! Entwurf für {customer_email} im Dashboard bereit — bitte prüfen und senden."
        ),
    )
    .await;
}

/// Find an existing email thread for an inquiry, or create a new one linked to it.
///
/// **Caller**: `handle_offer_approval` — needs a thread to store the outbound offer email draft.
/// **Why**: The offer email draft must live in a thread so it appears in the admin email view
/// and can be sent via the frontend. This covers three cases: thread already linked by `inquiry_id`,
/// thread linked by `customer_id`, or no thread yet (creates one).
///
/// # Parameters
/// - `state` — shared AppState (DB pool, no SMTP needed here)
/// - `inquiry_id` — the inquiry whose offer was approved
/// - `customer_email` — customer email address (used to look up customer_id for new threads)
///
/// # Returns
/// The thread UUID to insert the draft into, or `None` if creation failed.
async fn find_or_create_offer_thread(
    state: &AppState,
    inquiry_id: Uuid,
    customer_email: &str,
) -> Option<Uuid> {
    // 1. Thread already directly linked to this inquiry
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM email_threads WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some((tid,)) = existing {
        return Some(tid);
    }

    // 2. Thread linked by customer (not yet linked to this inquiry)
    let existing: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT et.id FROM email_threads et
        JOIN inquiries q ON et.customer_id = q.customer_id
        WHERE q.id = $1
        ORDER BY et.created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some((tid,)) = existing {
        // Link thread to inquiry while we're here
        let _ = sqlx::query(
            "UPDATE email_threads SET inquiry_id = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(inquiry_id)
        .bind(tid)
        .execute(&state.db)
        .await;
        return Some(tid);
    }

    // 3. No thread exists — create one (e.g. manually-created admin inquiry)
    let customer_id: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM customers WHERE email = $1")
            .bind(customer_email)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some((customer_id,)) = customer_id else {
        warn!("Cannot create email thread: customer not found for {customer_email}");
        return None;
    };

    let thread_id = Uuid::now_v7();
    match sqlx::query(
        "INSERT INTO email_threads (id, customer_id, inquiry_id, subject, created_at, updated_at) VALUES ($1, $2, $3, $4, NOW(), NOW())",
    )
    .bind(thread_id)
    .bind(customer_id)
    .bind(inquiry_id)
    .bind("Ihr Umzugsangebot — AUST Umzüge")
    .execute(&state.db)
    .await
    {
        Ok(_) => Some(thread_id),
        Err(e) => {
            error!("Failed to create email thread for offer draft: {e}");
            None
        }
    }
}

/// Mark a rejected offer and its parent quote as `'rejected'` in the database.
///
/// **Caller**: `run_offer_event_handler` on `ApprovalDecision::OfferDeny`.
/// **Why**: Alex presses ❌ in Telegram when the offer is wrong or not relevant (e.g. spam
/// inquiry). Marking both `offers.status` and `quotes.status` as rejected prevents the offer
/// from appearing in pending dashboards and allows future reporting on rejection rates.
///
/// # Parameters
/// - `state` — shared application state (DB)
/// - `client` — HTTP client for Telegram API calls
/// - `bot_token` — Telegram bot token
/// - `chat_id` — admin Telegram chat ID
/// - `offer_id` — the offer to reject
async fn handle_offer_denial(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    info!("Offer {offer_id} denied");

    // Fetch inquiry_id before updating
    let quote_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT inquiry_id FROM offers WHERE id = $1")
            .bind(offer_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let _ = sqlx::query("UPDATE offers SET status = 'rejected' WHERE id = $1")
        .bind(offer_id)
        .execute(&state.db)
        .await;

    // Also update quote status to rejected
    if let Some((inquiry_id,)) = quote_row {
        let now = chrono::Utc::now();
        let _ = sqlx::query("UPDATE inquiries SET status = 'rejected', updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(inquiry_id)
            .execute(&state.db)
            .await;
    }

    send_telegram_message(client, bot_token, chat_id, "❌ Angebot verworfen.").await;
}

/// Send the offer PDF to a customer via SMTP with a standard German cover letter.
///
/// **Caller**: `handle_offer_approval` (approval flow) and the admin API route
/// `POST /api/v1/offers/{id}/send` (manual re-send from the dashboard).
/// **Why**: Encapsulates SMTP send logic and the standard body text so that both callers
/// produce identical emails. The offer PDF is attached under the filename `Angebot-<id>.pdf`.
///
/// Uses `crate::services::email::{build_email_with_attachment, send_email}` internally.
///
/// # Parameters
/// - `state` — shared application state (email config: SMTP host/port, credentials, from address)
/// - `to` — customer email address
/// - `pdf_bytes` — raw PDF bytes to attach
/// - `offer_id` — used for the attachment filename and for logging
///
/// # Returns
/// `Ok(())` on successful SMTP delivery.
///
/// # Errors
/// Returns `Err(String)` if the email cannot be built (e.g. invalid address) or SMTP delivery fails.
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

/// Send the offer PDF to a customer via SMTP with a caller-supplied subject and body.
///
/// **Caller**: The admin API route `POST /api/v1/offers/{id}/send` when the request body
/// includes a custom `subject` and/or `body` (editable email draft feature).
/// **Why**: Alex sometimes wants to personalise the cover letter before sending, e.g. to
/// reference a phone conversation or add a special note. This variant accepts arbitrary
/// subject and body strings instead of the standard template used by `send_offer_email`.
///
/// # Parameters
/// - `state` — shared application state (email config)
/// - `to` — customer email address
/// - `pdf_bytes` — raw PDF bytes to attach
/// - `offer_id` — used for the attachment filename and for logging
/// - `subject` — custom email subject line
/// - `body` — custom plain-text email body
///
/// # Returns
/// `Ok(())` on successful SMTP delivery.
///
/// # Errors
/// Returns `Err(String)` if the email cannot be built or SMTP delivery fails.
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

/// Send a plain-text message to the admin Telegram chat.
///
/// **Caller**: Multiple handlers throughout the orchestrator — used for status confirmations
/// (e.g. "✅ Angebot gesendet"), edit-mode prompts, error reports, and cancellation notices.
/// **Why**: Centralises the Telegram `sendMessage` API call so all handlers share the same
/// error handling (log on failure, never panic).
///
/// # Parameters
/// - `client` — shared `reqwest::Client`
/// - `bot_token` — Telegram bot token
/// - `chat_id` — admin Telegram chat ID
/// - `text` — message text (plain text, no Markdown parsing)
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

    // ========== parse_edit_instructions ==========
    // These tests would have caught the brutto/netto bug: before the fix,
    // "800 Euro" and "800€" were treated as netto (×100), not brutto (÷1.19×100).

    fn brutto_to_netto_cents(brutto: f64) -> i64 {
        ((brutto / 1.19) * 100.0) as i64
    }

    #[test]
    fn edit_bare_euro_word_is_brutto() {
        // "800 Euro" — no qualifier → must be treated as brutto
        let o = parse_edit_instructions("800 Euro");
        assert_eq!(o.price_cents, Some(brutto_to_netto_cents(800.0)),
            "bare 'euro' must be brutto; before fix it returned 80000 (netto) instead of ~67226");
    }

    #[test]
    fn edit_euro_symbol_is_brutto() {
        let o = parse_edit_instructions("800€");
        assert_eq!(o.price_cents, Some(brutto_to_netto_cents(800.0)));
    }

    #[test]
    fn edit_preis_keyword_without_qualifier_is_brutto() {
        let o = parse_edit_instructions("Preis 500");
        assert_eq!(o.price_cents, Some(brutto_to_netto_cents(500.0)));
    }

    #[test]
    fn edit_explicit_brutto_converts() {
        let o = parse_edit_instructions("350 brutto");
        assert_eq!(o.price_cents, Some(brutto_to_netto_cents(350.0)));
    }

    #[test]
    fn edit_explicit_netto_keeps_value() {
        // "800 netto" → 80000 cents exactly (no division)
        let o = parse_edit_instructions("800 netto");
        assert_eq!(o.price_cents, Some(80000));
    }

    #[test]
    fn edit_persons_helfer() {
        let o = parse_edit_instructions("4 Helfer");
        assert_eq!(o.persons, Some(4));
        assert!(o.price_cents.is_none(), "persons keyword must not set price");
    }

    #[test]
    fn edit_persons_mann() {
        let o = parse_edit_instructions("3 Mann");
        assert_eq!(o.persons, Some(3));
    }

    #[test]
    fn edit_hours_stunden() {
        let o = parse_edit_instructions("6 Stunden");
        assert!((o.hours.unwrap_or(0.0) - 6.0).abs() < 0.001);
        assert!(o.price_cents.is_none());
    }

    #[test]
    fn edit_rate_stundensatz() {
        let o = parse_edit_instructions("Stundensatz 35");
        assert!((o.rate.unwrap_or(0.0) - 35.0).abs() < 0.001);
    }

    #[test]
    fn edit_volume_m3() {
        let o = parse_edit_instructions("15 m³");
        assert!((o.volume_m3.unwrap_or(0.0) - 15.0).abs() < 0.001);
    }

    #[test]
    fn edit_combined_all_fields() {
        let o = parse_edit_instructions("800 Euro, 4 Helfer, 6 Stunden");
        assert_eq!(o.price_cents, Some(brutto_to_netto_cents(800.0)));
        assert_eq!(o.persons, Some(4));
        assert!((o.hours.unwrap_or(0.0) - 6.0).abs() < 0.001);
    }

    #[test]
    fn edit_empty_input_all_none() {
        let o = parse_edit_instructions("");
        assert!(o.price_cents.is_none());
        assert!(o.persons.is_none());
        assert!(o.hours.is_none());
        assert!(o.rate.is_none());
        assert!(o.volume_m3.is_none());
    }

    #[test]
    fn edit_no_numbers_all_none() {
        let o = parse_edit_instructions("mach es ein bisschen schöner bitte");
        assert!(o.price_cents.is_none());
        assert!(o.persons.is_none());
    }

    #[test]
    fn edit_netto_and_brutto_in_same_segment_netto_wins() {
        // Pathological but must not panic; "netto" check comes first
        let o = parse_edit_instructions("800 brutto netto");
        // "netto" is present → netto path
        assert_eq!(o.price_cents, Some(80000));
    }
}
