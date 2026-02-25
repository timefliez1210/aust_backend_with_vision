use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailThread {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub quote_id: Option<Uuid>,
    pub subject: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMessage {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub direction: EmailDirection,
    pub from_address: String,
    pub to_address: String,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub message_id: Option<String>,
    pub llm_generated: bool,
    pub created_at: DateTime<Utc>,
}

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
    pub llm_generated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedEmail {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub message_id: String,
    pub date: DateTime<Utc>,
    pub attachments: Vec<EmailAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

