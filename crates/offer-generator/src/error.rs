use thiserror::Error;

#[derive(Debug, Error)]
pub enum OfferError {
    #[error("Pricing error: {0}")]
    Pricing(String),

    #[error("Template error: {0}")]
    Template(String),

    #[error("PDF generation error: {0}")]
    Pdf(String),

    #[error("Storage error: {0}")]
    Storage(String),
}
