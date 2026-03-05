use aust_core::models::{InquirySource, MovingInquiry, ParsedEmail};
use chrono::NaiveDate;
use serde::Deserialize;
use tracing::{debug, info};
use uuid::Uuid;

/// Raw JSON form data from the "Kostenloses Angebot" Netlify form, sent as a
/// `.json` email attachment by the `send-mail.php` handler.
///
/// All fields are `Option` because not every customer fills in every field.
/// Field names match the HTML `name` attributes of the web form (German).
#[derive(Debug, Deserialize)]
struct FormSubmission {
    /// HTML form `name` attribute value (e.g., `"kostenloses-angebot"`).
    #[serde(rename = "form-name")]
    form_name: Option<String>,
    name: Option<String>,
    /// Customer's real email address (distinct from the IMAP sender which is
    /// always `angebot@aust-umzuege.de`).
    email: Option<String>,
    phone: Option<String>,
    /// Customer's preferred moving date; may be ISO (`2025-03-15`) or German
    /// (`15.03.2025`) format.
    wunschtermin: Option<String>,
    /// Departure street address (Auszugsadresse).
    auszugsadresse: Option<String>,
    /// Floor at departure (e.g., `"2. Stock"`, `"Erdgeschoss"`).
    #[serde(rename = "etage-auszug")]
    etage_auszug: Option<String>,
    /// Parking ban needed at departure; `"on"` = true, absent = false.
    #[serde(rename = "halteverbot-auszug")]
    halteverbot_auszug: Option<String>,
    /// Elevator at departure; `"on"` = true, absent = false.
    #[serde(rename = "aufzug-auszug")]
    aufzug_auszug: Option<String>,
    /// Arrival street address (Einzugsadresse).
    einzugsadresse: Option<String>,
    #[serde(rename = "etage-einzug")]
    etage_einzug: Option<String>,
    #[serde(rename = "halteverbot-einzug")]
    halteverbot_einzug: Option<String>,
    #[serde(rename = "aufzug-einzug")]
    aufzug_einzug: Option<String>,
    /// Optional intermediate stop address (Zwischenstopp).
    #[serde(rename = "zwischenstopp-adresse")]
    zwischenstopp_adresse: Option<String>,
    #[serde(rename = "etage-zwischenstopp")]
    etage_zwischenstopp: Option<String>,
    #[serde(rename = "halteverbot-zwischenstopp")]
    halteverbot_zwischenstopp: Option<String>,
    #[serde(rename = "aufzug-zwischenstopp")]
    aufzug_zwischenstopp: Option<String>,
    /// Total moving volume in cubic metres as a string (may use comma as decimal separator).
    #[serde(rename = "umzugsvolumen-m3")]
    umzugsvolumen_m3: Option<String>,
    /// VolumeCalculator items list (e.g., `"2x Sofa, Couch (0.80 m³)\n1x Tisch (0.40 m³)"`).
    #[serde(rename = "gegenstaende-liste")]
    gegenstaende_liste: Option<String>,
    /// Comma-separated additional services (e.g., `"Möbeldemontage, Einpackservice"`).
    zusatzleistungen: Option<String>,
    /// Free-text customer message.
    nachricht: Option<String>,
}

/// Converts raw `ParsedEmail` values into a structured `MovingInquiry`.
///
/// Strategy (in priority order):
/// 1. Try to deserialise a `.json` attachment (most reliable; comes from the
///    "Kostenloses Angebot" web form via `send-mail.php`).
/// 2. Detect email type from body text markers and parse key/value fields.
/// 3. Fall back to free-text mode, storing the body in `notes` for the LLM
///    responder to extract data in a subsequent step.
pub struct EmailParser;

impl EmailParser {
    /// Creates a new `EmailParser`.
    pub fn new() -> Self {
        Self
    }

    /// Parse an incoming email into a `MovingInquiry`, extracting as much
    /// structured data as possible. Works for both form submissions and
    /// free-text emails.
    ///
    /// **Caller**: `EmailProcessor::process_incoming_email` calls this for
    /// every new IMAP message.
    ///
    /// # Parameters
    /// - `email` — The decoded IMAP message including body and attachments.
    ///
    /// # Returns
    /// A `MovingInquiry` with as many fields populated as the input allows.
    /// The caller should check `inquiry.is_complete()` before proceeding to
    /// offer generation.
    pub fn parse_inquiry(&self, email: &ParsedEmail) -> MovingInquiry {
        // Try JSON attachment first (most reliable for form submissions)
        if let Some(inquiry) = self.try_parse_json_attachment(email) {
            return inquiry;
        }

        let body = &email.body_text;
        let source = self.detect_source(body);

        info!(
            "Parsing email from {} as {:?} source",
            email.from, source
        );

        let has_photos = email
            .attachments
            .iter()
            .any(|a| a.content_type.starts_with("image/"));
        let has_videos = email
            .attachments
            .iter()
            .any(|a| a.content_type.starts_with("video/"));
        let photo_count = email
            .attachments
            .iter()
            .filter(|a| {
                a.content_type.starts_with("image/") || a.content_type.starts_with("video/")
            })
            .count() as u32;

        let source = if (has_photos || has_videos) && source == InquirySource::DirectEmail {
            InquirySource::MediaEmail
        } else {
            source
        };

        match source {
            InquirySource::QuoteForm => self.parse_quote_form(email, has_photos, photo_count),
            InquirySource::ContactForm => self.parse_contact_form(email, has_photos, photo_count),
            _ => self.parse_freetext(email, source, has_photos, photo_count),
        }
    }

    /// Try to parse a JSON attachment from the email (send-mail.php form data).
    ///
    /// Looks for the first attachment with content-type `application/json` or
    /// filename ending in `.json`, then deserialises it as `FormSubmission`.
    ///
    /// # Returns
    /// `Some(MovingInquiry)` when a valid JSON attachment was found and parsed,
    /// `None` when no JSON attachment exists or parsing failed (caller falls
    /// through to text-based parsing).
    fn try_parse_json_attachment(&self, email: &ParsedEmail) -> Option<MovingInquiry> {
        let json_attachment = email.attachments.iter().find(|a| {
            a.content_type.contains("json")
                || a.filename.ends_with(".json")
        })?;

        let json_str = std::str::from_utf8(&json_attachment.data).ok()?;
        let form: FormSubmission = match serde_json::from_str(json_str) {
            Ok(f) => f,
            Err(e) => {
                debug!("JSON attachment parse failed: {e}");
                return None;
            }
        };

        info!(
            "Parsed JSON form attachment: name={:?}, email={:?}, form={:?}",
            form.name, form.email, form.form_name
        );

        let has_photos = email
            .attachments
            .iter()
            .any(|a| a.content_type.starts_with("image/"));
        let photo_count = email
            .attachments
            .iter()
            .filter(|a| {
                a.content_type.starts_with("image/") || a.content_type.starts_with("video/")
            })
            .count() as u32;

        let preferred_date = form
            .wunschtermin
            .as_deref()
            .and_then(|d| parse_date(d));

        let volume_m3 = form
            .umzugsvolumen_m3
            .as_deref()
            .and_then(|v| v.replace(',', ".").trim().parse::<f64>().ok());

        let services = form.zusatzleistungen.as_deref().unwrap_or("");
        let services_lower = services.to_lowercase();
        let without_demontage = services_lower.replace("demontage", "");

        let has_intermediate = form.zwischenstopp_adresse.is_some();

        Some(MovingInquiry {
            id: Uuid::now_v7(),
            inquiry_id: None,
            source: InquirySource::QuoteForm,
            name: form.name,
            email: form.email.unwrap_or_else(|| email.from.clone()),
            phone: form.phone,
            preferred_date,
            departure_address: form.auszugsadresse,
            departure_floor: form.etage_auszug,
            departure_parking_ban: Some(form.halteverbot_auszug.as_deref() == Some("on")),
            departure_elevator: Some(form.aufzug_auszug.as_deref() == Some("on")),
            has_intermediate_stop: has_intermediate,
            intermediate_address: form.zwischenstopp_adresse,
            intermediate_floor: form.etage_zwischenstopp,
            intermediate_parking_ban: Some(form.halteverbot_zwischenstopp.as_deref() == Some("on")),
            intermediate_elevator: Some(form.aufzug_zwischenstopp.as_deref() == Some("on")),
            arrival_address: form.einzugsadresse,
            arrival_floor: form.etage_einzug,
            arrival_parking_ban: Some(form.halteverbot_einzug.as_deref() == Some("on")),
            arrival_elevator: Some(form.aufzug_einzug.as_deref() == Some("on")),
            volume_m3,
            items_list: form.gegenstaende_liste,
            has_photos,
            photo_count,
            service_packing: services_lower.contains("einpack"),
            service_assembly: without_demontage.contains("montage"),
            service_disassembly: services_lower.contains("demontage"),
            service_storage: services_lower.contains("einlagerung")
                || services_lower.contains("lagerung"),
            service_disposal: services_lower.contains("entsorgung"),
            notes: form.nachricht,
        })
    }

    /// Detect whether the email body is a structured form submission or free-text.
    ///
    /// Detection is based on known marker strings produced by the website's
    /// `send-mail.php` handler. Both "Neue Angebotsanfrage" and "Kostenloses
    /// Angebot" variants are recognised as `QuoteForm`.
    ///
    /// # Parameters
    /// - `body` — Plain-text email body.
    ///
    /// # Returns
    /// `InquirySource::QuoteForm`, `ContactForm`, or `DirectEmail`.
    fn detect_source(&self, body: &str) -> InquirySource {
        let lower = body.to_lowercase();

        // The website's send-mail.php generates emails with these markers.
        // "Angebotsanfrage" covers both "Neue Angebotsanfrage" and "Kostenloses Angebot" variants.
        if (lower.contains("kostenloses angebot") || lower.contains("angebotsanfrage"))
            && lower.contains("auszugsadresse")
        {
            InquirySource::QuoteForm
        } else if lower.contains("neue kontaktanfrage") || lower.contains("kontaktformular") {
            InquirySource::ContactForm
        } else {
            InquirySource::DirectEmail
        }
    }

    /// Parse a "Kostenloses Angebot" form submission email using text extraction.
    ///
    /// Handles two formats produced by `send-mail.php`:
    /// 1. **Flat**: `"Auszugsadresse: …"`, `"Etage Auszug: …"`, `"Halteverbot Auszug: Ja"`
    /// 2. **Sectioned**: `"--- Auszugsadresse ---"` followed by `"Adresse: …"`, `"Etage: …"`
    ///
    /// The flat format is tried first; if the field is absent, the section-based
    /// format is attempted as a fallback.
    ///
    /// **Note**: `departure_elevator` and `arrival_elevator` are not captured in
    /// text-format emails — only JSON attachments contain those values.
    fn parse_quote_form(
        &self,
        email: &ParsedEmail,
        has_photos: bool,
        photo_count: u32,
    ) -> MovingInquiry {
        let body = &email.body_text;

        let name = extract_field(body, "Name").or_else(|| {
            // New form format sends Vorname + Nachname separately instead of a single Name field
            let vorname = extract_field(body, "Vorname").unwrap_or_default();
            let nachname = extract_field(body, "Nachname")?;
            // If the user put their full name in Nachname (e.g. "Clemens Fabig" instead of just
            // "Fabig"), avoid doubling the first name by checking if Nachname already starts with
            // Vorname.
            let full = if !vorname.is_empty()
                && nachname
                    .to_lowercase()
                    .starts_with(&vorname.to_lowercase())
            {
                nachname
            } else {
                format!("{} {}", vorname, nachname).trim().to_string()
            };
            Some(full)
        });
        let form_email = extract_field(body, "E-Mail")
            .or_else(|| extract_field(body, "Email"))
            .or_else(|| extract_section_field(body, "Kontaktdaten", "E-Mail"))
            .or_else(|| extract_section_field(body, "Kontaktdaten", "Email"))
            .or_else(|| extract_email_from_body(body, &email.from));

        debug!("Extracted form_email={:?} (from={})", form_email, email.from);

        let phone = extract_field(body, "Telefon");
        let preferred_date = extract_field(body, "Wunschtermin").and_then(|d| parse_date(&d));

        // Try flat format first, fall back to section-based format
        let departure_address = extract_field(body, "Auszugsadresse")
            .or_else(|| extract_section_field(body, "Auszugsadresse", "Adresse"));
        let departure_floor = extract_field(body, "Etage Auszug")
            .or_else(|| extract_section_field(body, "Auszugsadresse", "Etage"));
        let departure_parking_ban = extract_bool_field(body, "Halteverbot Auszug")
            .or_else(|| extract_section_bool(body, "Auszugsadresse", "Halteverbot"));

        let intermediate_address = extract_field(body, "Zwischenstopp")
            .or_else(|| extract_section_field(body, "Zwischenstopp", "Adresse"));
        let intermediate_floor = extract_field(body, "Etage Zwischenstopp")
            .or_else(|| extract_section_field(body, "Zwischenstopp", "Etage"));
        let intermediate_parking_ban = extract_bool_field(body, "Halteverbot Zwischenstopp")
            .or_else(|| extract_section_bool(body, "Zwischenstopp", "Halteverbot"));
        let has_intermediate_stop = intermediate_address.is_some();

        let arrival_address = extract_field(body, "Einzugsadresse")
            .or_else(|| extract_section_field(body, "Einzugsadresse", "Adresse"));
        let arrival_floor = extract_field(body, "Etage Einzug")
            .or_else(|| extract_section_field(body, "Einzugsadresse", "Etage"));
        let arrival_parking_ban = extract_bool_field(body, "Halteverbot Einzug")
            .or_else(|| extract_section_bool(body, "Einzugsadresse", "Halteverbot"));

        let volume_m3 = extract_field(body, "Umzugsvolumen")
            .or_else(|| extract_field(body, "Volumen"))
            .or_else(|| extract_field(body, "Geschätztes Volumen"))
            .and_then(|v| {
                v.replace("m³", "")
                    .replace("m3", "")
                    .replace(',', ".")
                    .trim()
                    .parse::<f64>()
                    .ok()
            });

        // Items can span multiple lines: "Gegenstände: 1x Sofa\n1x Tisch\n..."
        let items_list = extract_multiline_field(body, "Gegenstände")
            .or_else(|| extract_multiline_field(body, "Gegenstaende"));

        let services_text = extract_field(body, "Zusatzleistungen").unwrap_or_default();
        let services_lower = services_text.to_lowercase();

        let notes = extract_field(body, "Nachricht")
            .or_else(|| extract_field(body, "Bemerkung"));

        debug!(
            "Parsed quote form: name={:?}, departure={:?}, arrival={:?}, volume={:?}, parking_ban_dep={:?}, parking_ban_arr={:?}",
            name, departure_address, arrival_address, volume_m3, departure_parking_ban, arrival_parking_ban
        );

        MovingInquiry {
            id: Uuid::now_v7(),
            inquiry_id: None,
            source: InquirySource::QuoteForm,
            name,
            email: form_email.unwrap_or_else(|| email.from.clone()),
            phone,
            preferred_date,
            departure_address,
            departure_floor,
            departure_parking_ban,
            departure_elevator: None, // not captured in text-form emails
            has_intermediate_stop,
            intermediate_address,
            intermediate_floor,
            intermediate_parking_ban,
            intermediate_elevator: None,
            arrival_address,
            arrival_floor,
            arrival_parking_ban,
            arrival_elevator: None,
            volume_m3,
            items_list,
            has_photos,
            photo_count,
            service_packing: services_lower.contains("einpack"),
            service_assembly: {
                // Check for Montage even when Demontage is also present:
                // remove "demontage" first, then check if "montage" remains
                let without_demontage = services_lower.replace("demontage", "");
                without_demontage.contains("montage")
            },
            service_disassembly: services_lower.contains("demontage"),
            service_storage: services_lower.contains("einlagerung")
                || services_lower.contains("lagerung"),
            service_disposal: services_lower.contains("entsorgung"),
            notes,
        }
    }

    /// Parse a basic contact form submission.
    ///
    /// Contact forms only have name, email, phone, and a free-text message.
    /// All move-specific fields are left as defaults; the LLM responder will
    /// ask the customer for them.
    fn parse_contact_form(
        &self,
        email: &ParsedEmail,
        has_photos: bool,
        photo_count: u32,
    ) -> MovingInquiry {
        let body = &email.body_text;

        let name = extract_field(body, "Name");
        let form_email = extract_field(body, "E-Mail");
        let phone = extract_field(body, "Telefon");
        let notes = extract_field(body, "Nachricht");

        MovingInquiry {
            id: Uuid::now_v7(),
            source: InquirySource::ContactForm,
            name,
            email: form_email.unwrap_or_else(|| email.from.clone()),
            phone,
            has_photos,
            photo_count,
            notes,
            ..Default::default()
        }
    }

    /// Parse a free-text or media email.
    ///
    /// For direct emails the structured fields cannot be reliably extracted
    /// without an LLM, so only the sender info is populated here. The full
    /// body is stored in `notes` so the responder step can pass it to the LLM
    /// for structured data extraction.
    fn parse_freetext(
        &self,
        email: &ParsedEmail,
        source: InquirySource,
        has_photos: bool,
        photo_count: u32,
    ) -> MovingInquiry {
        // For free-text emails, we extract the sender info from headers
        // and leave content extraction to the LLM in the responder step.
        MovingInquiry {
            id: Uuid::now_v7(),
            source,
            email: email.from.clone(),
            has_photos,
            photo_count,
            notes: Some(email.body_text.clone()),
            ..Default::default()
        }
    }
}

impl Default for EmailParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract a `"Key: Value"` field from a structured email body.
///
/// Matches lines where the line starts with `key` followed by an optional
/// space and colon (handles both `"Key: Value"` and `"Key : Value"`).
/// Returns `None` for empty values, `"-"`, `"Keine"`, or `"keine"`.
fn extract_field(body: &str, key: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();

        // Match "Key: Value" or "Key : Value"
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim_start_matches(':').trim_start_matches(' ').trim();
            if !rest.is_empty() && rest != "-" && rest != "Keine" && rest != "keine" {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Extract a boolean field by looking for "Ja"/"Nein"/"yes"/"true" patterns.
///
/// Returns `Some(true)` for affirmative values, `Some(false)` for negatives,
/// and `None` when the key is not present.
fn extract_bool_field(body: &str, key: &str) -> Option<bool> {
    extract_field(body, key).map(|v| {
        let lower = v.to_lowercase();
        lower.contains("ja") || lower.contains("yes") || lower.contains("true")
    })
}

/// Extract a field from within a section delimited by `"--- SectionName ---"`.
///
/// Scans lines after the section header until the next section delimiter or the
/// end of the body. Case-insensitive matching for both section name and key.
///
/// # Parameters
/// - `section` — Section header text (e.g., `"Auszugsadresse"`).
/// - `key` — Field name within the section (e.g., `"Adresse"`).
fn extract_section_field(body: &str, section: &str, key: &str) -> Option<String> {
    let section_lower = section.to_lowercase();
    let key_lower = key.to_lowercase();
    let mut in_section = false;

    for line in body.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Check for section headers: "--- SectionName ---" or "=== SectionName ==="
        if (lower.contains("---") || lower.contains("==="))
            && lower.contains(&section_lower)
        {
            in_section = true;
            continue;
        }

        // Another section starts — stop
        if in_section && (trimmed.starts_with("---") || trimmed.starts_with("===")) {
            break;
        }

        if in_section {
            if let Some(rest) = lower.strip_prefix(&key_lower) {
                let rest_trimmed = rest.trim_start_matches(':').trim_start_matches(' ').trim();
                if !rest_trimmed.is_empty() && rest_trimmed != "-" && rest_trimmed != "keine" {
                    // Return the original (non-lowered) value
                    let orig_rest = trimmed[key.len()..].trim_start_matches(':').trim_start_matches(' ').trim();
                    return Some(orig_rest.to_string());
                }
            }
        }
    }
    None
}

/// Extract a boolean field from within a named section.
///
/// Delegates to `extract_section_field` then maps the value with affirmative
/// detection (`"ja"`, `"yes"`, `"true"`).
fn extract_section_bool(body: &str, section: &str, key: &str) -> Option<bool> {
    extract_section_field(body, section, key).map(|v| {
        let lower = v.to_lowercase();
        lower.contains("ja") || lower.contains("yes") || lower.contains("true")
    })
}

/// Extract a multi-line field value from the email body.
///
/// The first match line is `"Key: first_value"`. Continuation lines are
/// collected until the next `"Key: …"` field, a section delimiter (`---`,
/// `===`), or end of body. Blank lines within the items block are skipped.
///
/// Used for the `"Gegenstände"` items list which may span many lines:
/// ```text
/// Gegenstände: 1x Bettumbau (0.30 m³)
/// 1x Französisches Bett komplett (1.50 m³)
/// 1x Nachttisch (0.20 m³)
/// Zusatzleistungen: Einpackservice
/// ```
///
/// # Parameters
/// - `key` — The field name that starts the block (e.g., `"Gegenstände"`).
fn extract_multiline_field(body: &str, key: &str) -> Option<String> {
    let mut lines_iter = body.lines().peekable();
    let mut result_lines = Vec::new();
    let mut found = false;

    while let Some(line) = lines_iter.next() {
        let trimmed = line.trim();

        if !found {
            // Look for the key
            if let Some(rest) = trimmed.strip_prefix(key) {
                let rest = rest.trim_start_matches(':').trim_start_matches(' ').trim();
                if !rest.is_empty() && rest != "-" && rest != "Keine" && rest != "keine" {
                    result_lines.push(rest.to_string());
                    found = true;
                }
            }
        } else {
            // Continuation: lines starting with digits (e.g., "1x ...") or non-empty lines
            // that don't look like a new "Key: Value" field
            if trimmed.is_empty() {
                continue; // skip blank lines within items
            }

            // Stop at section headers or new labeled fields
            if trimmed.starts_with("---") || trimmed.starts_with("===") {
                break;
            }

            // Check if this looks like a new field (contains ":" after a word)
            if let Some(colon_pos) = trimmed.find(':') {
                let before_colon = &trimmed[..colon_pos];
                // If the part before ":" is a label (letters/spaces, no digits at start),
                // it's a new field — stop collecting
                if !before_colon.is_empty()
                    && before_colon.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false)
                    && !before_colon.contains("m³")
                    && !before_colon.contains("m3")
                {
                    break;
                }
            }

            result_lines.push(trimmed.to_string());
        }
    }

    if result_lines.is_empty() {
        None
    } else {
        Some(result_lines.join("\n"))
    }
}

/// Last-resort email address extraction: scans the body for email-like tokens
/// and returns the first one that is not the company sender address.
///
/// Used when the structured `"E-Mail:"` field is absent from the email body
/// (e.g., older form versions that omit it).
///
/// # Parameters
/// - `body` — Plain-text email body to scan.
/// - `sender` — IMAP `From:` address to exclude (usually `angebot@aust-umzuege.de`).
fn extract_email_from_body(body: &str, sender: &str) -> Option<String> {
    let sender_lower = sender.to_lowercase();
    // Simple email regex: word chars + dots/hyphens @ domain
    for word in body.split_whitespace() {
        let word = word.trim_matches(|c: char| c == '<' || c == '>' || c == '(' || c == ')' || c == ',');
        if word.contains('@') && word.contains('.') {
            let candidate = word.to_lowercase();
            // Skip the sender/company address
            if candidate != sender_lower
                && !candidate.contains("aust-umzuege")
                && !candidate.contains("noreply")
                && !candidate.contains("no-reply")
            {
                return Some(word.to_string());
            }
        }
    }
    None
}

/// Parse a date string in common German and ISO formats.
///
/// Tries each format in order; returns `None` when no format matches rather
/// than panicking.
///
/// # Supported formats
/// - ISO: `2025-03-15`
/// - German full: `15.03.2025`
/// - German short month: `15.3.2025`
/// - German 2-digit year: `15.03.25`
fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();

    // Try ISO format: 2025-03-15
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }

    // Try German format: 15.03.2025
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d.%m.%Y") {
        return Some(d);
    }

    // Try German short: 15.3.2025
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d.%-m.%Y") {
        return Some(d);
    }

    // Try German with 2-digit year: 15.03.25
    if let Ok(d) = NaiveDate::parse_from_str(s, "%d.%m.%y") {
        return Some(d);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_field() {
        let body = "Name: Max Mustermann\nE-Mail: max@example.com\nTelefon: 0176 12345678\n";
        assert_eq!(
            extract_field(body, "Name"),
            Some("Max Mustermann".to_string())
        );
        assert_eq!(
            extract_field(body, "E-Mail"),
            Some("max@example.com".to_string())
        );
        assert_eq!(
            extract_field(body, "Telefon"),
            Some("0176 12345678".to_string())
        );
        assert_eq!(extract_field(body, "Fax"), None);
    }

    #[test]
    fn test_parse_date() {
        assert_eq!(
            parse_date("2025-03-15"),
            Some(NaiveDate::from_ymd_opt(2025, 3, 15).unwrap())
        );
        assert_eq!(
            parse_date("15.03.2025"),
            Some(NaiveDate::from_ymd_opt(2025, 3, 15).unwrap())
        );
    }

    #[test]
    fn test_detect_source() {
        let parser = EmailParser::new();

        let quote_body = "=== Kostenloses Angebot ===\nAuszugsadresse: Musterstr. 1";
        assert_eq!(parser.detect_source(quote_body), InquirySource::QuoteForm);

        // Also detect "Neue Angebotsanfrage" variant
        let quote_body2 = "=== Neue Angebotsanfrage ===\n--- Auszugsadresse ---\nAdresse: Str 1";
        assert_eq!(parser.detect_source(quote_body2), InquirySource::QuoteForm);

        let contact_body = "=== Neue Kontaktanfrage ===\nName: Max";
        assert_eq!(
            parser.detect_source(contact_body),
            InquirySource::ContactForm
        );

        let direct = "Hallo, ich möchte umziehen...";
        assert_eq!(parser.detect_source(direct), InquirySource::DirectEmail);
    }

    #[test]
    fn test_section_field_extraction() {
        let body = "=== Neue Angebotsanfrage ===\n\
            --- Auszugsadresse ---\n\
            Adresse: Steinbergstr. 3, 31139 Hildesheim\n\
            Etage: 2. Stock\n\
            Halteverbot: Ja\n\
            \n\
            --- Einzugsadresse ---\n\
            Adresse: Kaiserstr. 32, 31134 Hildesheim\n\
            Etage: 3. Stock\n\
            Halteverbot: Ja\n";

        assert_eq!(
            extract_section_field(body, "Auszugsadresse", "Adresse"),
            Some("Steinbergstr. 3, 31139 Hildesheim".to_string())
        );
        assert_eq!(
            extract_section_field(body, "Auszugsadresse", "Etage"),
            Some("2. Stock".to_string())
        );
        assert_eq!(
            extract_section_bool(body, "Auszugsadresse", "Halteverbot"),
            Some(true)
        );
        assert_eq!(
            extract_section_field(body, "Einzugsadresse", "Adresse"),
            Some("Kaiserstr. 32, 31134 Hildesheim".to_string())
        );
        assert_eq!(
            extract_section_field(body, "Einzugsadresse", "Etage"),
            Some("3. Stock".to_string())
        );
    }

    #[test]
    fn test_multiline_field_extraction() {
        let body = "Gegenstände: 1x Bettumbau (0.30 m³)\n\
            1x Französisches Bett komplett (1.50 m³)\n\
            1x Nachttisch (0.20 m³)\n\
            Zusatzleistungen: Einpackservice\n";

        let items = extract_multiline_field(body, "Gegenstände").unwrap();
        assert!(items.contains("Bettumbau"));
        assert!(items.contains("Französisches Bett"));
        assert!(items.contains("Nachttisch"));
        // Should NOT include the next field
        assert!(!items.contains("Zusatzleistungen"));
    }

    #[test]
    fn test_json_attachment_parsing() {
        use aust_core::models::EmailAttachment;

        let parser = EmailParser::new();

        let json = r#"{
            "form-name": "kostenloses-angebot",
            "umzugsvolumen-m3": "4.90",
            "gegenstaende-liste": "1x Schreibtisch über 1,6 m (1.70 m³)\n1x Tisch über 1,2 m (0.80 m³)",
            "zusatzleistungen": "Möbeldemontage, Einpackservice",
            "name": "Clemens Fabig",
            "email": "crfabig@googlemail.com",
            "phone": "015203080947",
            "wunschtermin": "2026-02-24",
            "auszugsadresse": "Steinbergstr. 3, 31139 Hildesheim",
            "etage-auszug": "1. Stock",
            "halteverbot-auszug": "on",
            "einzugsadresse": "Kaiserstr. 32, 31134 Hildsheim",
            "etage-einzug": "3. Stock",
            "halteverbot-einzug": "on",
            "nachricht": "Termin is ein wenig flexibel",
            "datenschutz-akzeptiert": "on",
            "_submitted_at": "2026-02-21T10:27:15+01:00"
        }"#;

        let email = ParsedEmail {
            from: "umzug@example.com".to_string(),
            to: "umzug@example.com".to_string(),
            subject: "Neue Angebotsanfrage".to_string(),
            body_text: "some text body".to_string(),
            body_html: None,
            message_id: "test@test".to_string(),
            date: chrono::Utc::now(),
            attachments: vec![EmailAttachment {
                filename: "form-data.json".to_string(),
                content_type: "application/json".to_string(),
                data: json.as_bytes().to_vec(),
            }],
        };

        let inquiry = parser.parse_inquiry(&email);

        assert_eq!(inquiry.source, InquirySource::QuoteForm);
        assert_eq!(inquiry.name, Some("Clemens Fabig".to_string()));
        assert_eq!(inquiry.email, "crfabig@googlemail.com");
        assert_eq!(inquiry.phone, Some("015203080947".to_string()));
        assert_eq!(
            inquiry.preferred_date,
            Some(NaiveDate::from_ymd_opt(2026, 2, 24).unwrap())
        );
        assert_eq!(
            inquiry.departure_address,
            Some("Steinbergstr. 3, 31139 Hildesheim".to_string())
        );
        assert_eq!(inquiry.departure_floor, Some("1. Stock".to_string()));
        assert_eq!(inquiry.departure_parking_ban, Some(true));
        assert_eq!(inquiry.arrival_floor, Some("3. Stock".to_string()));
        assert_eq!(inquiry.arrival_parking_ban, Some(true));
        assert_eq!(inquiry.volume_m3, Some(4.9));
        assert!(inquiry.items_list.is_some());
        assert!(inquiry.service_packing);
        assert!(inquiry.service_disassembly);
        assert!(!inquiry.service_assembly); // only Demontage, not Montage
        assert_eq!(
            inquiry.notes,
            Some("Termin is ein wenig flexibel".to_string())
        );
    }

    #[test]
    fn test_services_parsing() {
        let parser = EmailParser::new();

        // Simulate a quote form email with multiple services including both Montage and Demontage
        let body = "=== Neue Angebotsanfrage ===\n\
            Name: Max Mustermann\n\
            E-Mail: max@example.com\n\
            Telefon: 0176 12345678\n\
            Wunschtermin: 15.03.2025\n\
            --- Auszugsadresse ---\n\
            Adresse: Musterstr. 1, 31139 Hildesheim\n\
            Etage: 2. Stock\n\
            Halteverbot: Ja\n\
            --- Einzugsadresse ---\n\
            Adresse: Zielstr. 5, 30159 Hannover\n\
            Etage: EG\n\
            Halteverbot: Nein\n\
            Umzugsvolumen: 15 m³\n\
            Zusatzleistungen: Einpackservice, Möbelmontage, Möbeldemontage, Entsorgung von Sperrmüll\n\
            Nachricht: Bitte um Angebot\n";

        let email = ParsedEmail {
            from: "form@aust-umzuege.de".to_string(),
            to: "umzug@example.com".to_string(),
            subject: "Neue Angebotsanfrage".to_string(),
            body_text: body.to_string(),
            body_html: None,
            message_id: "test@test".to_string(),
            date: chrono::Utc::now(),
            attachments: vec![],
        };

        let inquiry = parser.parse_inquiry(&email);

        assert!(inquiry.service_packing, "Einpackservice should be detected");
        assert!(inquiry.service_assembly, "Möbelmontage should be detected even when Demontage is also present");
        assert!(inquiry.service_disassembly, "Möbeldemontage should be detected");
        assert!(inquiry.service_disposal, "Entsorgung should be detected");
    }

    #[test]
    fn test_missing_fields() {
        let empty = MovingInquiry {
            email: "test@test.de".to_string(),
            ..Default::default()
        };
        assert_eq!(empty.missing_fields().len(), 8);
        assert!(!empty.is_complete());

        let complete = MovingInquiry {
            email: "test@test.de".to_string(),
            name: Some("Max".to_string()),
            phone: Some("0176".to_string()),
            preferred_date: Some(NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()),
            departure_address: Some("Musterstr 1, 31134 Hildesheim".to_string()),
            departure_floor: Some("2. Stock".to_string()),
            arrival_address: Some("Zielstr 5, 30159 Hannover".to_string()),
            arrival_floor: Some("Erdgeschoss".to_string()),
            volume_m3: Some(25.0),
            ..Default::default()
        };
        assert!(complete.is_complete());
        assert_eq!(complete.completeness(), 1.0);
    }

    // ---------------------------------------------------------------
    // Helper: build a ParsedEmail with a JSON attachment
    // ---------------------------------------------------------------
    fn make_json_email(json: &str) -> ParsedEmail {
        use aust_core::models::EmailAttachment;
        ParsedEmail {
            from: "umzug@example.com".to_string(),
            to: "umzug@example.com".to_string(),
            subject: "Neue Angebotsanfrage".to_string(),
            body_text: "some text body".to_string(),
            body_html: None,
            message_id: "test@test".to_string(),
            date: chrono::Utc::now(),
            attachments: vec![EmailAttachment {
                filename: "form-data.json".to_string(),
                content_type: "application/json".to_string(),
                data: json.as_bytes().to_vec(),
            }],
        }
    }

    // ---------------------------------------------------------------
    // Elevator field tests
    // ---------------------------------------------------------------

    #[test]
    fn elevator_on_maps_to_true() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "aufzug-auszug": "on"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.departure_elevator, Some(true));
    }

    #[test]
    fn elevator_absent_maps_to_false() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.departure_elevator, Some(false));
    }

    #[test]
    fn elevator_empty_string_maps_to_false() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "aufzug-auszug": ""
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.departure_elevator, Some(false));
    }

    #[test]
    fn all_three_elevator_fields() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "aufzug-auszug": "on",
            "aufzug-einzug": "on",
            "aufzug-zwischenstopp": ""
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.departure_elevator, Some(true));
        assert_eq!(inquiry.arrival_elevator, Some(true));
        assert_eq!(inquiry.intermediate_elevator, Some(false));
    }

    #[test]
    fn elevator_zwischenstopp_on() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "aufzug-zwischenstopp": "on"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.intermediate_elevator, Some(true));
    }

    // ---------------------------------------------------------------
    // Edge case tests
    // ---------------------------------------------------------------

    #[test]
    fn corrupted_json_returns_none() {
        let email = make_json_email("not valid json");
        let parser = EmailParser::new();
        // try_parse_json_attachment is private but accessible from within the module
        let result = parser.try_parse_json_attachment(&email);
        assert!(result.is_none(), "Corrupted JSON should return None, not panic");
    }

    #[test]
    fn empty_json_object_returns_inquiry_with_defaults() {
        let email = make_json_email("{}");
        let parser = EmailParser::new();
        // An empty JSON object deserializes into FormSubmission (all fields Option::None),
        // so try_parse_json_attachment returns Some with defaults filled in.
        let result = parser.try_parse_json_attachment(&email);
        assert!(result.is_some(), "Empty JSON object should still produce a MovingInquiry");
        let inquiry = result.unwrap();
        // email falls back to the IMAP sender
        assert_eq!(inquiry.email, "umzug@example.com");
        assert_eq!(inquiry.name, None);
    }

    #[test]
    fn missing_email_field_uses_imap_sender() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        // When "email" field is absent, the parser falls back to the IMAP sender
        assert_eq!(inquiry.email, "umzug@example.com");
    }

    #[test]
    fn german_date_format() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "wunschtermin": "15.03.2026"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(
            inquiry.preferred_date,
            Some(NaiveDate::from_ymd_opt(2026, 3, 15).unwrap())
        );
    }

    #[test]
    fn iso_date_format() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "wunschtermin": "2026-03-15"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(
            inquiry.preferred_date,
            Some(NaiveDate::from_ymd_opt(2026, 3, 15).unwrap())
        );
    }

    #[test]
    fn invalid_date_gracefully_ignored() {
        let json = r#"{
            "form-name": "kostenloses-angebot",
            "name": "Test User",
            "email": "test@example.com",
            "wunschtermin": "not-a-date"
        }"#;
        let email = make_json_email(json);
        let inquiry = EmailParser::new().parse_inquiry(&email);
        assert_eq!(inquiry.preferred_date, None, "Invalid date should be None, not panic");
    }
}
