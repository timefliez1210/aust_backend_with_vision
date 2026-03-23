use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::snapshots::Services;

/// Status state machine for the unified inquiry lifecycle.
///
/// **Why**: Replaces the old QuoteStatus + OfferStatus split with one unified enum.
///
/// Pre-sales: pending -> info_requested -> estimating -> estimated -> offer_ready -> offer_sent
///   -> accepted | rejected | expired | cancelled
/// Operations: scheduled -> completed -> invoiced -> paid
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InquiryStatus {
    Pending,
    InfoRequested,
    Estimating,
    Estimated,
    OfferReady,
    OfferSent,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
    Scheduled,
    Completed,
    Invoiced,
    Paid,
}

impl Default for InquiryStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl InquiryStatus {
    /// Check whether transitioning from `self` to `target` is allowed.
    ///
    /// **Caller**: status update handlers (API routes, orchestrator).
    /// **Why**: Enforces the inquiry lifecycle state machine so invalid jumps
    /// (e.g., Pending -> Paid) are rejected before hitting the database.
    ///
    /// # Parameters
    /// - `target` -- the desired next status
    ///
    /// # Returns
    /// `true` if the transition is valid, `false` otherwise.
    pub fn can_transition_to(&self, target: &InquiryStatus) -> bool {
        use InquiryStatus::*;
        matches!(
            (self, target),
            // Normal forward flow
            (Pending, InfoRequested)
            | (Pending, Estimating)
            | (Pending, Estimated)
            | (Pending, Cancelled)
            // Skip-ahead shortcuts for direct API and auto-pipeline
            | (Pending, OfferReady)
            | (InfoRequested, Estimating)
            | (InfoRequested, Estimated)
            | (InfoRequested, Cancelled)
            | (Estimating, Estimated)
            | (Estimating, Cancelled)
            | (Estimated, OfferReady)
            | (Estimated, Cancelled)
            // Skip intermediate: Estimated -> OfferSent
            | (Estimated, OfferSent)
            | (OfferReady, OfferSent)
            | (OfferReady, Accepted)
            | (OfferReady, Cancelled)
            | (OfferSent, Accepted)
            | (OfferSent, Rejected)
            | (OfferSent, Expired)
            | (OfferSent, Cancelled)
            | (Accepted, Scheduled)
            | (Accepted, Cancelled)
            | (Scheduled, Completed)
            | (Scheduled, Cancelled)
            | (Completed, Invoiced)
            | (Invoiced, Paid)
        )
    }

    /// Derive the offer-level status string from the inquiry status.
    ///
    /// **Caller**: snapshot builders, API response mappers.
    /// **Why**: The unified inquiry status subsumes the old OfferStatus enum.
    /// This helper extracts the offer-relevant portion for backward-compatible
    /// API responses and Telegram display.
    ///
    /// # Returns
    /// `Some(&str)` with the offer status when the inquiry is in an offer-relevant
    /// state, `None` otherwise.
    pub fn to_offer_status(&self) -> Option<&'static str> {
        match self {
            Self::OfferReady => Some("draft"),
            Self::OfferSent => Some("sent"),
            Self::Accepted => Some("accepted"),
            Self::Rejected => Some("rejected"),
            Self::Expired => Some("expired"),
            Self::Cancelled => Some("cancelled"),
            _ => None,
        }
    }

    /// Returns the lowercase snake_case string representation.
    ///
    /// **Caller**: DB serialization, API responses.
    /// **Why**: Consistent string form for storage and wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InfoRequested => "info_requested",
            Self::Estimating => "estimating",
            Self::Estimated => "estimated",
            Self::OfferReady => "offer_ready",
            Self::OfferSent => "offer_sent",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Scheduled => "scheduled",
            Self::Completed => "completed",
            Self::Invoiced => "invoiced",
            Self::Paid => "paid",
        }
    }
}

impl std::fmt::Display for InquiryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for InquiryStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "info_requested" => Ok(Self::InfoRequested),
            "estimating" => Ok(Self::Estimating),
            "estimated" => Ok(Self::Estimated),
            "offer_ready" => Ok(Self::OfferReady),
            "offer_sent" => Ok(Self::OfferSent),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            "scheduled" => Ok(Self::Scheduled),
            "completed" => Ok(Self::Completed),
            "invoiced" => Ok(Self::Invoiced),
            "paid" => Ok(Self::Paid),
            other => Err(format!("unknown InquiryStatus: {other}")),
        }
    }
}

/// A persisted moving inquiry — the central DB record linking customer, addresses,
/// volume estimation, and generated offers.
///
/// Created by the orchestrator when a complete or near-complete `MovingInquiry` is received.
/// All downstream records (volume estimations, offers, bookings) reference this record via
/// their `inquiry_id` foreign key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inquiry {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// Customer who submitted the inquiry.
    pub customer_id: Uuid,
    /// Departure address (Auszug); `None` until the orchestrator creates it.
    pub origin_address_id: Option<Uuid>,
    /// Destination address (Einzug); `None` until the orchestrator creates it.
    pub destination_address_id: Option<Uuid>,
    /// Optional intermediate stop (Zwischenstopp) address.
    pub stop_address_id: Option<Uuid>,
    pub status: InquiryStatus,
    /// Agreed total moving volume in cubic metres; set after volume estimation
    /// completes or when the inventory form is used.
    pub estimated_volume_m3: Option<f64>,
    /// Total driving distance in kilometres for the full route (depot->origin->
    /// [stop]->destination); `None` until the distance calculator runs.
    pub distance_km: Option<f64>,
    pub preferred_date: Option<DateTime<Utc>>,
    /// Free-text customer message (e.g., `"Bitte vorsichtig mit dem Klavier"`).
    pub notes: Option<String>,
    /// How this inquiry entered the system (email_form, contact_form, direct_email, photo_api, etc.).
    pub source: Option<String>,
    /// Structured service flags; replaces comma-separated notes parsing in the new schema.
    pub services: Option<Services>,
    /// Timestamp when the offer was emailed to the customer.
    pub offer_sent_at: Option<DateTime<Utc>>,
    /// Timestamp when the customer accepted the offer.
    pub accepted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

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
    /// Set by the orchestrator once it has created the corresponding `Inquiry` record.
    pub inquiry_id: Option<Uuid>,
    /// How this inquiry reached the system.
    pub source: InquirySource,

    // Contact
    /// Customer's full name; extracted from the form or email body.
    pub name: Option<String>,
    /// Customer's salutation (Anrede): "Herr", "Frau", or "Divers".
    pub salutation: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_can_transition_to_info_requested() {
        assert!(InquiryStatus::Pending.can_transition_to(&InquiryStatus::InfoRequested));
    }

    #[test]
    fn test_pending_can_transition_to_estimated() {
        assert!(InquiryStatus::Pending.can_transition_to(&InquiryStatus::Estimated));
    }

    #[test]
    fn test_pending_can_transition_to_offer_ready() {
        assert!(InquiryStatus::Pending.can_transition_to(&InquiryStatus::OfferReady));
    }

    #[test]
    fn test_pending_cannot_transition_to_paid() {
        assert!(!InquiryStatus::Pending.can_transition_to(&InquiryStatus::Paid));
    }

    #[test]
    fn test_terminal_states_cannot_transition() {
        let terminals = [
            InquiryStatus::Rejected,
            InquiryStatus::Expired,
            InquiryStatus::Cancelled,
            InquiryStatus::Paid,
        ];
        let all = [
            InquiryStatus::Pending,
            InquiryStatus::InfoRequested,
            InquiryStatus::Estimating,
            InquiryStatus::Estimated,
            InquiryStatus::OfferReady,
            InquiryStatus::OfferSent,
            InquiryStatus::Accepted,
            InquiryStatus::Rejected,
            InquiryStatus::Expired,
            InquiryStatus::Cancelled,
            InquiryStatus::Scheduled,
            InquiryStatus::Completed,
            InquiryStatus::Invoiced,
            InquiryStatus::Paid,
        ];
        for terminal in &terminals {
            for target in &all {
                assert!(
                    !terminal.can_transition_to(target),
                    "{:?} should not transition to {:?}",
                    terminal,
                    target,
                );
            }
        }
    }

    #[test]
    fn test_offer_sent_can_be_accepted_or_rejected() {
        assert!(InquiryStatus::OfferSent.can_transition_to(&InquiryStatus::Accepted));
        assert!(InquiryStatus::OfferSent.can_transition_to(&InquiryStatus::Rejected));
        assert!(InquiryStatus::OfferSent.can_transition_to(&InquiryStatus::Expired));
        assert!(InquiryStatus::OfferSent.can_transition_to(&InquiryStatus::Cancelled));
        assert!(!InquiryStatus::OfferSent.can_transition_to(&InquiryStatus::Pending));
    }

    #[test]
    fn test_estimated_can_skip_to_offer_sent() {
        assert!(InquiryStatus::Estimated.can_transition_to(&InquiryStatus::OfferSent));
    }

    #[test]
    fn test_inquiry_status_serde_roundtrip() {
        let all = [
            InquiryStatus::Pending,
            InquiryStatus::InfoRequested,
            InquiryStatus::Estimating,
            InquiryStatus::Estimated,
            InquiryStatus::OfferReady,
            InquiryStatus::OfferSent,
            InquiryStatus::Accepted,
            InquiryStatus::Rejected,
            InquiryStatus::Expired,
            InquiryStatus::Cancelled,
            InquiryStatus::Scheduled,
            InquiryStatus::Completed,
            InquiryStatus::Invoiced,
            InquiryStatus::Paid,
        ];
        for status in all {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: InquiryStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, status, "roundtrip for {:?}", status);
        }
    }

    #[test]
    fn test_inquiry_status_from_str() {
        assert_eq!("pending".parse::<InquiryStatus>().unwrap(), InquiryStatus::Pending);
        assert_eq!("offer_ready".parse::<InquiryStatus>().unwrap(), InquiryStatus::OfferReady);
        assert_eq!("scheduled".parse::<InquiryStatus>().unwrap(), InquiryStatus::Scheduled);
        assert!("bogus".parse::<InquiryStatus>().is_err());
    }

    #[test]
    fn test_inquiry_status_display() {
        assert_eq!(InquiryStatus::OfferReady.to_string(), "offer_ready");
        assert_eq!(InquiryStatus::Paid.to_string(), "paid");
    }

    #[test]
    fn test_to_offer_status_mapping() {
        assert_eq!(InquiryStatus::OfferReady.to_offer_status(), Some("draft"));
        assert_eq!(InquiryStatus::OfferSent.to_offer_status(), Some("sent"));
        assert_eq!(InquiryStatus::Accepted.to_offer_status(), Some("accepted"));
        assert_eq!(InquiryStatus::Rejected.to_offer_status(), Some("rejected"));
        assert_eq!(InquiryStatus::Expired.to_offer_status(), Some("expired"));
        assert_eq!(InquiryStatus::Cancelled.to_offer_status(), Some("cancelled"));
        assert_eq!(InquiryStatus::Pending.to_offer_status(), None);
        assert_eq!(InquiryStatus::Estimating.to_offer_status(), None);
        assert_eq!(InquiryStatus::Paid.to_offer_status(), None);
    }

    #[test]
    fn test_inquiry_status_default() {
        assert_eq!(InquiryStatus::default(), InquiryStatus::Pending);
    }
}
