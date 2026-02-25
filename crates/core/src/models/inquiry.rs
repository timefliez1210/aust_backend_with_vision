use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents all data the email agent needs to collect for a moving quote.
/// Fields are Option because data arrives incrementally (across multiple emails).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MovingInquiry {
    pub id: Uuid,
    pub quote_id: Option<Uuid>,
    pub source: InquirySource,

    // Contact
    pub name: Option<String>,
    pub email: String,
    pub phone: Option<String>,

    // Move details
    pub preferred_date: Option<NaiveDate>,

    // Departure (Auszug)
    pub departure_address: Option<String>,
    pub departure_floor: Option<String>,
    pub departure_parking_ban: Option<bool>,

    // Intermediate stop (Zwischenstopp)
    pub has_intermediate_stop: bool,
    pub intermediate_address: Option<String>,
    pub intermediate_floor: Option<String>,
    pub intermediate_parking_ban: Option<bool>,

    // Arrival (Einzug)
    pub arrival_address: Option<String>,
    pub arrival_floor: Option<String>,
    pub arrival_parking_ban: Option<bool>,

    // Volume
    pub volume_m3: Option<f64>,
    pub items_list: Option<String>,
    pub has_photos: bool,
    pub photo_count: u32,

    // Additional services (Zusatzleistungen)
    pub service_packing: bool,
    pub service_assembly: bool,
    pub service_disassembly: bool,
    pub service_storage: bool,
    pub service_disposal: bool,

    // Notes
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InquirySource {
    /// Structured data from the "Kostenloses Angebot" form.
    QuoteForm,
    /// Basic contact form submission.
    ContactForm,
    /// Direct email (unstructured).
    #[default]
    DirectEmail,
    /// Email with media attachments (photos/videos).
    MediaEmail,
}

impl MovingInquiry {
    /// Returns a list of fields that are still missing for a complete quote.
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

    /// Whether we have enough data to generate a quote.
    pub fn is_complete(&self) -> bool {
        self.missing_fields().is_empty()
    }

    /// How complete the inquiry is, as a percentage (0.0 - 1.0).
    pub fn completeness(&self) -> f64 {
        let total = 8.0; // total required fields
        let filled = total - self.missing_fields().len() as f64;
        (filled / total).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissingField {
    Name,
    Phone,
    PreferredDate,
    DepartureAddress,
    DepartureFloor,
    ArrivalAddress,
    ArrivalFloor,
    Volume,
}

impl MissingField {
    /// German prompt text for requesting this field from the customer.
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
