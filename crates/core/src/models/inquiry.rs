use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Aggregated moving inquiry state collected from one or more emails or API calls.
///
/// This is the central hand-off structure between the email agent / API routes
/// and the orchestrator. All fields are `Option` because data arrives
/// incrementally — a first email may contain only the customer's name and
/// addresses, while a follow-up provides the volume estimate.
///
/// Once `is_complete()` returns `true`, the orchestrator can create a quote
/// and trigger offer generation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MovingInquiry {
    /// In-memory correlation ID (UUID v7); not persisted to the database.
    pub id: Uuid,
    /// Set by the orchestrator once it has created the corresponding `Quote` record.
    pub quote_id: Option<Uuid>,
    /// How this inquiry reached the system.
    pub source: InquirySource,

    // Contact
    /// Customer's full name; extracted from the form or email body.
    pub name: Option<String>,
    /// Customer's email address. For form submissions, this comes from the JSON
    /// attachment — *not* from the IMAP `From:` header which is always the
    /// company inbox (`angebot@aust-umzuege.de`).
    pub email: String,
    pub phone: Option<String>,

    // Move details
    /// Customer's preferred moving date.
    pub preferred_date: Option<NaiveDate>,

    // Departure (Auszug)
    /// Full departure street address.
    pub departure_address: Option<String>,
    /// Floor description (e.g., `"2. Stock"`, `"Erdgeschoss"`).
    pub departure_floor: Option<String>,
    /// Whether a temporary parking ban (Halteverbotszone) is needed at departure.
    pub departure_parking_ban: Option<bool>,
    /// Whether there is an elevator at the departure address.
    pub departure_elevator: Option<bool>,

    // Intermediate stop (Zwischenstopp)
    /// `true` when an intermediate address was supplied.
    pub has_intermediate_stop: bool,
    pub intermediate_address: Option<String>,
    pub intermediate_floor: Option<String>,
    pub intermediate_parking_ban: Option<bool>,
    pub intermediate_elevator: Option<bool>,

    // Arrival (Einzug)
    /// Full arrival street address.
    pub arrival_address: Option<String>,
    pub arrival_floor: Option<String>,
    pub arrival_parking_ban: Option<bool>,
    pub arrival_elevator: Option<bool>,

    // Volume
    /// Estimated total volume in cubic metres from the form or LLM.
    pub volume_m3: Option<f64>,
    /// Raw items list string (VolumeCalculator format, e.g. `"2x Sofa (0.80 m³)"`).
    /// Parsed into `InventoryItem` records by the orchestrator.
    pub items_list: Option<String>,
    /// `true` when the customer attached at least one photo to the email.
    pub has_photos: bool,
    /// Total count of image and video attachments.
    pub photo_count: u32,

    // Additional services (Zusatzleistungen)
    /// Customer requested Einpackservice (packing materials + service).
    pub service_packing: bool,
    /// Customer requested Möbelmontage (furniture assembly at destination).
    pub service_assembly: bool,
    /// Customer requested Möbeldemontage (furniture disassembly at origin).
    pub service_disassembly: bool,
    /// Customer requested temporary storage (Einlagerung).
    pub service_storage: bool,
    /// Customer requested disposal of unwanted items (Entsorgung).
    pub service_disposal: bool,

    // Notes
    /// Free-text message from the customer (`nachricht` form field).
    pub notes: Option<String>,
}

/// How a moving inquiry entered the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InquirySource {
    /// Structured data from the "Kostenloses Angebot" form (JSON attachment).
    QuoteForm,
    /// Basic contact form submission (name, email, message only).
    ContactForm,
    /// Direct email (unstructured; LLM extracts data in the responder step).
    #[default]
    DirectEmail,
    /// Email with photo or video attachments (forwarded to vision pipeline).
    MediaEmail,
}

impl MovingInquiry {
    /// Returns a list of fields that are still missing for a complete quote.
    ///
    /// **Caller**: `EmailProcessor` calls this to decide whether to auto-generate
    /// an offer or first send a follow-up email requesting more information.
    ///
    /// # Returns
    /// A `Vec<MissingField>` with one entry per absent required field.
    /// An empty vec means the inquiry is actionable.
    pub fn missing_fields(&self) -> Vec<MissingField> {
        let mut missing = Vec::new();

        if self.name.is_none() {
            missing.push(MissingField::Name);
        }
        if self.phone.is_none() {
            missing.push(MissingField::Phone);
        }
        if self.preferred_date.is_none() {
            missing.push(MissingField::PreferredDate);
        }
        if self.departure_address.is_none() {
            missing.push(MissingField::DepartureAddress);
        }
        if self.departure_floor.is_none() {
            missing.push(MissingField::DepartureFloor);
        }
        if self.arrival_address.is_none() {
            missing.push(MissingField::ArrivalAddress);
        }
        if self.arrival_floor.is_none() {
            missing.push(MissingField::ArrivalFloor);
        }
        if self.volume_m3.is_none() && self.items_list.is_none() && !self.has_photos {
            missing.push(MissingField::Volume);
        }

        missing
    }

    /// Whether this inquiry has enough data for the orchestrator to generate a quote.
    ///
    /// **Caller**: `EmailProcessor::process_incoming_email` gates the pipeline
    /// on this check before forwarding the inquiry to the orchestrator channel.
    pub fn is_complete(&self) -> bool {
        self.missing_fields().is_empty()
    }

    /// How complete the inquiry is, as a fraction from 0.0 to 1.0.
    ///
    /// **Caller**: Displayed in admin dashboards to show inquiry progress.
    ///
    /// # Math
    /// `completeness = (8 − missing_count) / 8`, clamped to `[0.0, 1.0]`.
    pub fn completeness(&self) -> f64 {
        let total = 8.0; // total required fields
        let filled = total - self.missing_fields().len() as f64;
        (filled / total).clamp(0.0, 1.0)
    }
}

/// A required field that is absent from a `MovingInquiry`.
///
/// Used by `EmailParser` and `EmailProcessor` to produce targeted German
/// follow-up questions for the customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissingField {
    Name,
    Phone,
    PreferredDate,
    DepartureAddress,
    DepartureFloor,
    ArrivalAddress,
    ArrivalFloor,
    /// No volume data at all: no `volume_m3`, no `items_list`, and no photos.
    Volume,
}

impl MissingField {
    /// German prompt text for requesting this field from the customer via email.
    ///
    /// **Caller**: `EmailProcessor` (responder step) includes these strings in
    /// the LLM system prompt so the generated follow-up email asks for exactly
    /// the missing data in natural German.
    pub fn german_prompt(&self) -> &'static str {
        match self {
            Self::Name => "Ihren vollständigen Namen",
            Self::Phone => "Ihre Telefonnummer für Rückfragen",
            Self::PreferredDate => "Ihren Wunschtermin für den Umzug",
            Self::DepartureAddress => "die vollständige Auszugsadresse (Straße, Hausnummer, PLZ, Ort)",
            Self::DepartureFloor => "in welchem Stockwerk sich die Auszugswohnung befindet",
            Self::ArrivalAddress => "die vollständige Einzugsadresse (Straße, Hausnummer, PLZ, Ort)",
            Self::ArrivalFloor => "in welchem Stockwerk sich die Einzugswohnung befindet",
            Self::Volume => "eine Auflistung der zu transportierenden Gegenstände oder Fotos der Räumlichkeiten",
        }
    }
}
