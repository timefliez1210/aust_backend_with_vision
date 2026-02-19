use aust_core::models::{InquirySource, MovingInquiry, ParsedEmail};
use chrono::NaiveDate;
use tracing::{debug, info};
use uuid::Uuid;

pub struct EmailParser;

impl EmailParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse an incoming email into a MovingInquiry, extracting as much
    /// structured data as possible. Works for both form submissions and
    /// free-text emails.
    pub fn parse_inquiry(&self, email: &ParsedEmail) -> MovingInquiry {
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

    /// Detect whether the email body is a structured form submission or free-text.
    fn detect_source(&self, body: &str) -> InquirySource {
        let lower = body.to_lowercase();

        // The website's send-mail.php generates emails with these markers
        if lower.contains("kostenloses angebot") && lower.contains("auszugsadresse") {
            InquirySource::QuoteForm
        } else if lower.contains("neue kontaktanfrage") || lower.contains("kontaktformular") {
            InquirySource::ContactForm
        } else {
            InquirySource::DirectEmail
        }
    }

    /// Parse a "Kostenloses Angebot" form submission email.
    /// These have a known structure from send-mail.php.
    fn parse_quote_form(
        &self,
        email: &ParsedEmail,
        has_photos: bool,
        photo_count: u32,
    ) -> MovingInquiry {
        let body = &email.body_text;

        let name = extract_field(body, "Name");
        let form_email = extract_field(body, "E-Mail");
        let phone = extract_field(body, "Telefon");
        let preferred_date = extract_field(body, "Wunschtermin").and_then(|d| parse_date(&d));

        let departure_address = extract_field(body, "Auszugsadresse");
        let departure_floor = extract_field(body, "Etage Auszug");
        let departure_parking_ban = extract_bool_field(body, "Halteverbot Auszug");

        let intermediate_address = extract_field(body, "Zwischenstopp");
        let intermediate_floor = extract_field(body, "Etage Zwischenstopp");
        let intermediate_parking_ban = extract_bool_field(body, "Halteverbot Zwischenstopp");
        let has_intermediate_stop = intermediate_address.is_some();

        let arrival_address = extract_field(body, "Einzugsadresse");
        let arrival_floor = extract_field(body, "Etage Einzug");
        let arrival_parking_ban = extract_bool_field(body, "Halteverbot Einzug");

        let volume_m3 = extract_field(body, "Umzugsvolumen")
            .or_else(|| extract_field(body, "Volumen"))
            .and_then(|v| {
                v.replace("m³", "")
                    .replace("m3", "")
                    .replace(',', ".")
                    .trim()
                    .parse::<f64>()
                    .ok()
            });

        let items_list = extract_field(body, "Gegenstände")
            .or_else(|| extract_field(body, "Gegenstaende"));

        let services_text = extract_field(body, "Zusatzleistungen").unwrap_or_default();
        let services_lower = services_text.to_lowercase();

        let notes = extract_field(body, "Nachricht")
            .or_else(|| extract_field(body, "Bemerkung"));

        debug!(
            "Parsed quote form: name={:?}, departure={:?}, arrival={:?}, volume={:?}",
            name, departure_address, arrival_address, volume_m3
        );

        MovingInquiry {
            id: Uuid::now_v7(),
            quote_id: None,
            source: InquirySource::QuoteForm,
            name,
            email: form_email.unwrap_or_else(|| email.from.clone()),
            phone,
            preferred_date,
            departure_address,
            departure_floor,
            departure_parking_ban,
            has_intermediate_stop,
            intermediate_address,
            intermediate_floor,
            intermediate_parking_ban,
            arrival_address,
            arrival_floor,
            arrival_parking_ban,
            volume_m3,
            items_list,
            has_photos,
            photo_count,
            service_packing: services_lower.contains("einpack"),
            service_assembly: services_lower.contains("montage")
                && !services_lower.contains("demontage"),
            service_disassembly: services_lower.contains("demontage"),
            service_storage: services_lower.contains("einlagerung")
                || services_lower.contains("lagerung"),
            service_disposal: services_lower.contains("entsorgung"),
            notes,
        }
    }

    /// Parse a basic contact form submission.
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

    /// Parse a free-text email or media email.
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

/// Extract a "Key: Value" field from a structured email body.
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

/// Extract a boolean field (looks for "Ja"/"Nein" pattern).
fn extract_bool_field(body: &str, key: &str) -> Option<bool> {
    extract_field(body, key).map(|v| {
        let lower = v.to_lowercase();
        lower.contains("ja") || lower.contains("yes") || lower.contains("true")
    })
}

/// Parse a date string in common German formats.
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

        let contact_body = "=== Neue Kontaktanfrage ===\nName: Max";
        assert_eq!(
            parser.detect_source(contact_body),
            InquirySource::ContactForm
        );

        let direct = "Hallo, ich möchte umziehen...";
        assert_eq!(parser.detect_source(direct), InquirySource::DirectEmail);
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
}
