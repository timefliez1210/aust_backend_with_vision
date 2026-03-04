use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Whether an email message was sent to or received from a customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailDirection {
    /// Email received from a customer (via IMAP).
    Inbound,
    /// Email sent to a customer (via SMTP).
    Outbound,
}

/// A conversation thread grouping related inbound and outbound messages.
///
/// Each customer inquiry results in one `EmailThread`. All follow-up messages
/// (both human-written and LLM-generated) are attached to the same thread so
/// that the full conversation history is preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailThread {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// The customer this conversation belongs to.
    pub customer_id: Uuid,
    /// The quote that was initiated by this conversation, if one exists.
    pub inquiry_id: Option<Uuid>,
    /// Email subject line of the first message in the thread.
    pub subject: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single email message within a thread.
///
/// Persisted for audit purposes and to allow re-sending or inspecting the full
/// conversation from the admin dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMessage {
    /// UUID v7 primary key.
    pub id: Uuid,
    /// Parent thread this message belongs to.
    pub thread_id: Uuid,
    /// Whether this message was inbound (from customer) or outbound (to customer).
    pub direction: EmailDirection,
    pub from_address: String,
    pub to_address: String,
    pub subject: Option<String>,
    /// Plain-text body; primary content used for parsing and display.
    pub body_text: Option<String>,
    /// HTML body; stored when present in the original email, but not parsed.
    pub body_html: Option<String>,
    /// SMTP/IMAP `Message-ID` header value, used for threading and deduplication.
    pub message_id: Option<String>,
    /// `true` when the body was written by the LLM rather than a human.
    pub llm_generated: bool,
    pub created_at: DateTime<Utc>,
}

/// Input for inserting a new email message into the database.
///
/// **Caller**: `EmailProcessor` in `crates/email-agent` creates one of these
/// for every email it sends or receives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEmailMessage {
    pub thread_id: Uuid,
    pub direction: EmailDirection,
    pub from_address: String,
    pub to_address: String,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub message_id: Option<String>,
    /// Set to `true` when the body was produced by the LLM responder.
    pub llm_generated: bool,
}

/// A structured representation of a raw email fetched from IMAP.
///
/// **Caller**: `EmailParser::parse_inquiry()` receives this and returns a
/// `MovingInquiry`. It is also persisted via `CreateEmailMessage` for the
/// audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedEmail {
    /// Sender address as extracted from the `From:` header.
    pub from: String,
    /// Primary recipient address from the `To:` header.
    pub to: String,
    pub subject: String,
    /// Decoded plain-text body; used for field extraction and LLM prompts.
    pub body_text: String,
    /// HTML body; stored but not parsed for field extraction.
    pub body_html: Option<String>,
    /// SMTP `Message-ID` used for deduplication (prevents reprocessing the
    /// same email across multiple poll cycles).
    pub message_id: String,
    pub date: DateTime<Utc>,
    /// All MIME attachments; the JSON form attachment and any photos/videos
    /// sent by the customer are found here.
    pub attachments: Vec<EmailAttachment>,
}

/// A single decoded MIME attachment from an email.
///
/// **Why**: The "Kostenloses Angebot" web form sends a `.json` file containing
/// all structured form fields as an email attachment. Image/video attachments
/// are forwarded to the vision pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAttachment {
    /// MIME filename (e.g., `"form-data.json"` or `"room.jpg"`).
    pub filename: String,
    /// MIME content-type (e.g., `"application/json"` or `"image/jpeg"`).
    pub content_type: String,
    /// Raw decoded bytes of the attachment body.
    pub data: Vec<u8>,
}
