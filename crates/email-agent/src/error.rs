use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmailError {
    #[error("IMAP error: {0}")]
    Imap(String),

    #[error("SMTP error: {0}")]
    Smtp(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Telegram error: {0}")]
    Telegram(String),
}
