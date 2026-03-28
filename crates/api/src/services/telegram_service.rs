//! Telegram service — send PDF offers to the admin, handle approval/edit/denial,
//! and parse natural-language edit instructions via LLM or regex fallback.

use crate::repositories::{inquiry_repo, offer_repo};
use crate::services::offer_builder::{build_offer_with_overrides, GeneratedOffer, OfferOverrides};
use crate::AppState;
use aust_core::config::TelegramConfig;
use aust_llm_providers::{LlmMessage, LlmProvider};
use reqwest::{
    multipart::{Form, Part},
    Client,
};
use sqlx::PgPool;
use tracing::{error, info, warn};
use uuid::Uuid;

// ── Telegram helpers ──────────────────────────────────────────────────────────

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
pub(crate) fn format_address_line(address: &str, floor: &str, elevator: Option<bool>) -> String {
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
pub(crate) async fn send_offer_to_telegram(config: &TelegramConfig, generated: &GeneratedOffer) {
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

    let tg_last_name = generated.customer_name.split_whitespace().last().unwrap_or("Angebot");
    let tg_offer_num = offer.offer_number.as_deref().unwrap_or("");
    let tg_filename = crate::repositories::offer_repo::build_offer_filename(tg_offer_num, tg_last_name, "pdf");
    let pdf_part = Part::bytes(generated.pdf_bytes.clone())
        .file_name(tg_filename)
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
pub(crate) async fn notify_telegram_error(config: &TelegramConfig, message: &str) {
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
pub(crate) async fn send_telegram_message(client: &Client, bot_token: &str, chat_id: i64, text: &str) {
    let api_url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    if let Err(e) = client.post(&api_url).json(&payload).send().await {
        error!("Failed to send Telegram message: {e}");
    }
}

// ── Approval / denial handlers ────────────────────────────────────────────────

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
pub(crate) async fn handle_offer_approval(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    use crate::repositories::email_repo;

    info!("Offer {offer_id} approved — creating email draft for admin to review and send");

    let row = offer_repo::fetch_approval_context(&state.db, offer_id)
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
    let thread_id = crate::services::email_dispatch::find_or_create_offer_thread(state, inquiry_id, &customer_email).await;

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

    let _ = email_repo::insert_message(
        &state.db,
        Uuid::now_v7(),
        thread_id,
        "outbound",
        &state.config.email.from_address,
        &customer_email,
        "Ihr Umzugsangebot — AUST Umzüge",
        body,
        false,
        "draft",
    )
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
pub(crate) async fn handle_offer_denial(
    state: &AppState,
    client: &Client,
    bot_token: &str,
    chat_id: i64,
    offer_id: Uuid,
) {
    info!("Offer {offer_id} denied");

    // Fetch inquiry_id before updating
    let inq_id = offer_repo::fetch_inquiry_id(&state.db, offer_id)
        .await
        .unwrap_or(None);

    let _ = offer_repo::reject(&state.db, offer_id).await;

    // Also update quote status to rejected
    if let Some(inquiry_id) = inq_id {
        let now = chrono::Utc::now();
        let _ = inquiry_repo::update_status(&state.db, inquiry_id, "rejected", now).await;
    }

    send_telegram_message(client, bot_token, chat_id, "❌ Angebot verworfen.").await;
}

// ── Edit flow ─────────────────────────────────────────────────────────────────

/// Parsed overrides from admin's free-text edit instructions.
#[derive(Default)]
pub(crate) struct EditOverrides {
    pub price_cents: Option<i64>,
    pub persons: Option<u32>,
    pub hours: Option<f64>,
    pub rate: Option<f64>,
    pub volume_m3: Option<f64>,
}

/// Summary of the current offer for LLM context.
pub(crate) struct OfferSummary {
    pub price_cents: i64,
    pub persons: u32,
    pub hours: f64,
    pub volume_m3: f64,
    pub distance_km: f64,
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
pub(crate) async fn fetch_current_offer_summary(db: &PgPool, offer_id: Uuid, inquiry_id: Uuid) -> OfferSummary {
    // Get offer price
    let price_cents = offer_repo::fetch_price(db, offer_id)
        .await
        .unwrap_or(None)
        .unwrap_or(0);

    // Get quote details
    let readiness = inquiry_repo::fetch_readiness(db, inquiry_id)
        .await
        .unwrap_or(None);
    let (volume, distance) = readiness
        .map(|r| (r.estimated_volume_m3, r.distance_km))
        .unwrap_or((None, None));

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
pub(crate) async fn llm_parse_edit_instructions(
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
pub(crate) fn parse_edit_instructions(text: &str) -> EditOverrides {
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
pub(crate) async fn handle_offer_edit(
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
        let _ = inquiry_repo::update_volume(&state.db, inquiry_id, volume, chrono::Utc::now()).await;
    }

    // Regenerate with overrides baked into the xlsx/PDF, preserving the existing offer ID and number
    let offer_overrides = OfferOverrides {
        price_cents: overrides.price_cents,
        persons: overrides.persons,
        hours: overrides.hours,
        rate: overrides.rate,
        line_items: None,
        existing_offer_id: Some(old_offer_id),
        fahrt_flat_total: None, // loaded from DB inside build_offer_with_overrides
        fahrt_reset: false,
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

#[cfg(test)]
mod tests {
    use super::*;

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
