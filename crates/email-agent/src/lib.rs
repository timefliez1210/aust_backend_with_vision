pub mod error;

mod imap_client;
mod parser;
mod processor;
mod responder;
mod smtp_client;
mod telegram;

pub use error::EmailError;
pub use imap_client::ImapClient;
pub use parser::EmailParser;
pub use processor::EmailProcessor;
pub use responder::{EmailResponse, EmailResponder};
pub use smtp_client::SmtpClient;
pub use telegram::{ApprovalDecision, ApprovalResponse, CalendarCommand, DraftMessage, TelegramBot};
